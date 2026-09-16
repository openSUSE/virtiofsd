// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE-BSD-3-Clause file.

use crate::passthrough::stat::{statx, MountId};
use crate::passthrough::util::openat;
use crate::util::ResultErrorContext;
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read, Seek};
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::sync::{Arc, Mutex, RwLock, Weak};

pub struct MountFd {
    map: Weak<RwLock<HashMap<MountId, Weak<MountFd>>>>,
    mount_id: MountId,
    file: File,
}

/// This type maintains a map where each entry maps a mount ID to an open FD on that mount.  Other
/// code can request an `Arc<MountFd>` for any mount ID.  A key gets added to the map, when the
/// first `Arc<MountFd>` for that mount ID is requested.  A key gets removed from the map, when the
/// last `Arc<MountFd>` for that mount ID is dropped.  That is, map entries are reference-counted
/// and other code can keep an entry in the map by holding on to an `Arc<MountFd>`.
///
/// We currently have one use case for `MountFds`:
///
/// 1. Creating a file handle only returns a mount ID, but opening a file handle requires an open FD
///    on the respective mount.  So we look that up in the map.
pub struct MountFds {
    map: Arc<RwLock<HashMap<MountId, Weak<MountFd>>>>,

    /// /proc/self/mountinfo
    mountinfo: Mutex<File>,

    /// An optional prefix to strip from all mount points in mountinfo
    mountprefix: Option<String>,

    /// Set of filesystems for which we have already logged file handle errors
    error_logged: Arc<RwLock<HashSet<MountId>>>,
}

impl MountFd {
    /**
     * Create a new mount FD for the given `path` relative to `dir`.
     *
     * Its mount ID is taken from `statx()`.  If `mount_fds` is given, the mount FD is entered
     * there; unless `mount_fds` already contains a mount FD for this mount ID, in which case that
     * FD is returned instead of creating a new one.
     *
     * The use case for this is migration: The migration source sends us a mapping of its mount
     * IDs to paths, so this function turns those paths into `MountFd` objects that can be used
     * for `SerializableFileHandle::to_openable()`.  (Note that those mount IDs are only valid on
     * the migration source, not here, on the destination; that’s why the `MountFd` object’s mount
     * ID is taken from `statx()` instead of using the source’s ID.)
     */
    pub fn new<D: AsRawFd>(
        mount_fds: Option<&MountFds>,
        dir: &D,
        path: &str,
    ) -> io::Result<Arc<MountFd>> {
        // Not documented in the man page, but mount FDs must be opened with `O_RDONLY` (not just
        // `O_PATH`)
        let file =
            openat(dir, path, libc::O_RDONLY).err_context(|| format!("Failed to open {path}"))?;
        let st = statx(&file, None).err_context(|| format!("Failed to get {path}'s mount ID"))?;

        if let Some(mount_fds) = mount_fds {
            let mut mfds_locked = mount_fds.map.write().unwrap();
            // Same as in `MountFds::get()`: If there is an entry but upgrade fails, treat it as
            // non-existent.  Overwriting it is safe because `MountFd::drop()` only removes
            // `MountFds` entries that have a refcount of 0.
            if let Some(mount_fd) = mfds_locked.get(&st.mnt_id).and_then(Weak::upgrade) {
                return Ok(mount_fd);
            }

            let mount_fd = Arc::new(MountFd {
                map: Arc::downgrade(&mount_fds.map),
                mount_id: st.mnt_id,
                file,
            });
            mfds_locked.insert(st.mnt_id, Arc::downgrade(&mount_fd));
            Ok(mount_fd)
        } else {
            Ok(Arc::new(MountFd {
                map: Weak::new(),
                mount_id: st.mnt_id,
                file,
            }))
        }
    }

    pub fn file(&self) -> &File {
        &self.file
    }

    /// Get the associated mount ID.
    pub fn mount_id(&self) -> MountId {
        self.mount_id
    }
}

/**
 * Error object (to be used as `Result<T, MPRError>`) for mount-point-related errors (hence MPR).
 * Includes a description (that is auto-generated from the `io::Error` at first), which can be
 * overridden with `MPRError::set_desc()`, or given a prefix with `MPRError::prefix()`.
 *
 * The full description can be retrieved through the `Display` trait implementation (or the
 * auto-derived `ToString`).
 *
 * `MPRError` objects should generally be logged at some point, because they may indicate an error
 * in the user's configuration or a bug in virtiofsd.  However, we only want to log them once per
 * filesystem, and so they can be silenced (setting `silent` to true if we know that we have
 * already logged an error for the respective filesystem) and then should not be logged.
 *
 * Naturally, a "mount-point-related" error should be associated with some mount point, which is
 * reflected in `fs_mount_id` and `fs_mount_root`.  Setting these values will improve the error
 * description, because the `Display` implementation will prepend these values to the returned
 * string.
 *
 * To achieve this association, `MPRError` objects should be created through
 * `MountFds::error_for()`, which obtains the mount root path for the given mount ID, and will thus
 * try to not only set `fs_mount_id`, but `fs_mount_root` also.  `MountFds::error_for()` will also
 * take care to set `silent` as appropriate.
 *
 * (Sometimes, though, we know an error is associated with a mount point, but we do not know with
 * which one.  That is why the `fs_mount_id` field is optional.)
 */
#[derive(Debug)]
pub struct MPRError {
    io: io::Error,
    description: String,
    silent: bool,

    fs_mount_id: Option<MountId>,
    fs_mount_root: Option<String>,
}

/// Type alias for convenience
pub type MPRResult<T> = Result<T, MPRError>;

impl Drop for MountFd {
    fn drop(&mut self) {
        debug!(
            "Dropping MountFd: mount_id={}, mount_fd={}",
            self.mount_id,
            self.file.as_raw_fd(),
        );

        // If `self.map.upgrade()` fails, then the `MountFds` structure was dropped while there was
        // still an `Arc<MountFd>` alive.  In this case, we don't need to remove it from the map,
        // because the map doesn't exist anymore.
        if let Some(map) = self.map.upgrade() {
            let mut map = map.write().unwrap();
            // After the refcount reaches zero and before we lock the map, there's a window where
            // the value can be concurrently replaced by a `Weak` pointer to a new `MountFd`.
            // Therefore, only remove the value if the refcount in the map is zero, too.
            if let Some(0) = map.get(&self.mount_id).map(Weak::strong_count) {
                map.remove(&self.mount_id);
            }
        }
    }
}

impl<E: ToString + Into<io::Error>> From<E> for MPRError {
    /// Convert any stringifyable error object that can be converted to an `io::Error` to an
    /// `MPRError`.  Note that `fs_mount_id` and `fs_mount_root` are not set, so this `MPRError`
    /// object is not associated with any mount point.
    /// The initial description is taken from the original error object.
    fn from(err: E) -> Self {
        let description = err.to_string();
        MPRError {
            io: err.into(),
            description,
            silent: false,

            fs_mount_id: None,
            fs_mount_root: None,
        }
    }
}

impl MPRError {
    /// Override the current description
    #[must_use]
    pub fn set_desc(mut self, s: String) -> Self {
        self.description = s;
        self
    }

    /// Add a prefix to the description
    #[must_use]
    pub fn prefix(self, s: String) -> Self {
        let new_desc = format!("{s}: {}", self.description);
        self.set_desc(new_desc)
    }

    /// To give additional information to the user (when this error is logged), add the mount ID of
    /// the filesystem associated with this error
    #[must_use]
    fn set_mount_id(mut self, mount_id: MountId) -> Self {
        self.fs_mount_id = Some(mount_id);
        self
    }

    /// To give additional information to the user (when this error is logged), add the mount root
    /// path for the filesystem associated with this error
    #[must_use]
    fn set_mount_root(mut self, mount_root: String) -> Self {
        self.fs_mount_root = Some(mount_root);
        self
    }

    /// Mark this error as silent (i.e. not to be logged)
    #[must_use]
    fn silence(mut self) -> Self {
        self.silent = true;
        self
    }

    /// Return whether this error is silent (i.e. should not be logged)
    pub fn silent(&self) -> bool {
        self.silent
    }

    /// Return the `io::Error` from an `MPRError` and drop the rest
    pub fn into_inner(self) -> io::Error {
        self.io
    }
}

impl std::fmt::Display for MPRError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.fs_mount_id, &self.fs_mount_root) {
            (None, None) => write!(f, "{}", self.description),

            (Some(id), None) => write!(f, "Filesystem with mount ID {id}: {}", self.description),

            (None, Some(root)) => {
                write!(f, "Filesystem mounted on \"{root}\": {}", self.description)
            }

            (Some(id), Some(root)) => write!(
                f,
                "Filesystem mounted on \"{root}\" (mount ID: {id}): {}",
                self.description
            ),
        }
    }
}

impl std::error::Error for MPRError {}

impl MountFds {
    pub fn new(mountinfo: File, mountprefix: Option<String>) -> Self {
        MountFds {
            map: Default::default(),
            mountinfo: Mutex::new(mountinfo),
            mountprefix,
            error_logged: Default::default(),
        }
    }

    pub fn get<F>(&self, mount_id: MountId, reopen_fd: F) -> MPRResult<Arc<MountFd>>
    where
        F: FnOnce(RawFd, libc::c_int) -> io::Result<File>,
    {
        let existing_mount_fd = self
            .map
            // The `else` branch below (where `existing_mount_fd` matches `None`) takes a write lock
            // to insert a new mount FD into the hash map.  This doesn't deadlock, because the read
            // lock taken here doesn't have its lifetime extended beyond the statement, because
            // `Weak::upgrade` returns a new pointer and not a reference into the read lock.
            .read()
            .unwrap()
            .get(&mount_id)
            // We treat a failed upgrade just like a non-existent key, because it means that all
            // strong references to the `MountFd` have disappeared, so it's in the process of being
            // dropped, but `MountFd::drop()` just did not yet get to remove it from the map.
            .and_then(Weak::upgrade);

        let mount_fd = if let Some(mount_fd) = existing_mount_fd {
            mount_fd
        } else {
            // `open_by_handle_at()` needs a non-`O_PATH` fd, which we will need to open here.  We
            // are going to open the filesystem's mount point, but we do not know whether that is a
            // special file[1], and we must not open special files with anything but `O_PATH`, so
            // we have to get some `O_PATH` fd first that we can stat to find out whether it is
            // safe to open.
            // [1] While mount points are commonly directories, it is entirely possible for a
            //     filesystem's root inode to be a regular or even special file.
            let mount_point = self.get_mount_root(mount_id)?;

            // Clone `mount_point` so we can still use it in error messages
            let c_mount_point = CString::new(mount_point.clone()).map_err(|e| {
                self.error_for(mount_id, e)
                    .prefix(format!("Failed to convert \"{mount_point}\" to a CString"))
            })?;

            let mount_point_fd = unsafe { libc::open(c_mount_point.as_ptr(), libc::O_PATH) };
            if mount_point_fd < 0 {
                return Err(self
                    .error_for(mount_id, io::Error::last_os_error())
                    .prefix(format!("Failed to open mount point \"{mount_point}\"")));
            }

            // Safe because we have just opened this FD
            let mount_point_path = unsafe { File::from_raw_fd(mount_point_fd) };

            // Ensure that `mount_point_path` refers to an inode with the mount ID we need
            let stx = statx(&mount_point_path, None).map_err(|e| {
                self.error_for(mount_id, e)
                    .prefix(format!("Failed to stat mount point \"{mount_point}\""))
            })?;

            if stx.mnt_id != mount_id {
                return Err(self
                    .error_for(mount_id, io::Error::from_raw_os_error(libc::EIO))
                    .set_desc(format!(
                        "Mount point's ({mount_point}) mount ID ({}) does not match expected value ({mount_id})",
                        stx.mnt_id
                    )));
            }

            // Ensure that we can safely reopen `mount_point_path` with `O_RDONLY`
            let file_type = stx.st.st_mode & libc::S_IFMT;
            if file_type != libc::S_IFREG && file_type != libc::S_IFDIR {
                return Err(self
                    .error_for(mount_id, io::Error::from_raw_os_error(libc::EIO))
                    .set_desc(format!(
                        "Mount point \"{mount_point}\" is not a regular file or directory"
                    )));
            }

            // Now that we know that this is a regular file or directory, really open it
            let file = reopen_fd(
                mount_point_path.as_raw_fd(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
            .map_err(|e| {
                self.error_for(mount_id, e).prefix(format!(
                    "Failed to reopen mount point \"{mount_point}\" for reading"
                ))
            })?;

            let mut mount_fds_locked = self.map.write().unwrap();

            // As above: by calling `and_then(Weak::upgrade)`, we treat a failed upgrade just like a
            // non-existent key.  If the key exists but upgrade fails, then `HashMap::insert()`
            // below will update the value.  `MountFd::drop()` takes care to only remove a `MountFd`
            // without strong references from the map, and hence will not touch the updated one.
            if let Some(mount_fd) = mount_fds_locked.get(&mount_id).and_then(Weak::upgrade) {
                // A mount FD was added concurrently while we did not hold a lock on
                // `mount_fds.map` -- use that entry (`file` will be dropped).
                mount_fd
            } else {
                debug!(
                    "Creating MountFd: mount_id={mount_id}, mount_fd={}",
                    file.as_raw_fd(),
                );
                let mount_fd = Arc::new(MountFd {
                    map: Arc::downgrade(&self.map),
                    mount_id,
                    file,
                });
                mount_fds_locked.insert(mount_id, Arc::downgrade(&mount_fd));
                mount_fd
            }
        };

        Ok(mount_fd)
    }

    /// Given a mount ID, return the mount root path (by reading `/proc/self/mountinfo`)
    pub fn get_mount_root(&self, mount_id: MountId) -> MPRResult<String> {
        let mountinfo = {
            let mountinfo_file = &mut *self.mountinfo.lock().unwrap();

            mountinfo_file.rewind().map_err(|e| {
                self.error_for_nolookup(mount_id, e)
                    .prefix("Failed to access /proc/self/mountinfo".into())
            })?;

            let mut mountinfo = String::new();
            mountinfo_file.read_to_string(&mut mountinfo).map_err(|e| {
                self.error_for_nolookup(mount_id, e)
                    .prefix("Failed to read /proc/self/mountinfo".into())
            })?;

            mountinfo
        };

        let path = mountinfo.split('\n').find_map(|line| {
            let mut columns = line.split(' ');

            if columns.next()?.parse::<MountId>().ok()? != mount_id {
                return None;
            }

            // Skip parent mount ID, major:minor device ID, and the root within the filesystem
            // (to get to the mount path)
            columns.nth(3)
        });

        match path {
            Some(p) => {
                // mountinfo escapes spaces, tabs, newlines, and backslashes in paths.
                // Decode backslashes last so a literal "\040" is not decoded a second time.
                let p = p
                    .replace("\\040", " ")
                    .replace("\\011", "\t")
                    .replace("\\012", "\n")
                    .replace("\\134", "\\");
                if let Some(prefix) = self.mountprefix.as_ref() {
                    if let Some(suffix) = p.strip_prefix(prefix).filter(|s| !s.is_empty()) {
                        Ok(suffix.into())
                    } else {
                        // The shared directory is the mount point (strip_prefix() returned "") or
                        // mount is outside the shared directory, so it must be the mount the root
                        // directory is on
                        Ok("/".into())
                    }
                } else {
                    Ok(p)
                }
            }

            None => Err(self
                .error_for_nolookup(mount_id, io::Error::from_raw_os_error(libc::EINVAL))
                .set_desc(format!("Failed to find mount root for mount ID {mount_id}"))),
        }
    }

    /// Generate an `MPRError` object for the given `mount_id`, and silence it if we have already
    /// generated such an object for that `mount_id`.
    /// (Called `..._nolookup`, because in contrast to `MountFds::error_for()`, this method will
    /// not try to look up the respective mount root path, and so is safe to call when such a
    /// lookup would be unwise.)
    fn error_for_nolookup<E: ToString + Into<io::Error>>(
        &self,
        mount_id: MountId,
        err: E,
    ) -> MPRError {
        let err = MPRError::from(err).set_mount_id(mount_id);

        if self.error_logged.read().unwrap().contains(&mount_id) {
            err.silence()
        } else {
            self.error_logged.write().unwrap().insert(mount_id);
            err
        }
    }

    /// Call `self.error_for_nolookup()`, and if the `MPRError` object is not silenced, try to
    /// obtain the mount root path for the given `mount_id` and add it to the error object.
    /// (Note: DO NOT call this method from `MountFds::get_mount_root()`, because that may lead to
    /// an infinite loop.)
    pub fn error_for<E: ToString + Into<io::Error>>(&self, mount_id: MountId, err: E) -> MPRError {
        let err = self.error_for_nolookup(mount_id, err);

        if err.silent() {
            // No need to add more information
            err
        } else {
            // This just adds some information, so ignore errors
            if let Ok(mount_root) = self.get_mount_root(mount_id) {
                err.set_mount_root(mount_root)
            } else {
                err
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::passthrough::util::reopen_fd_through_proc;
    use std::fs;
    use std::io::Write;
    use vmm_sys_util::tempdir::TempDir;
    use vmm_sys_util::tempfile::TempFile;

    /// Back a mount table with a temporary file, just like the seekable proc file.
    fn mount_fds(contents: &str, prefix: Option<&str>) -> MountFds {
        let mountinfo = TempFile::new().unwrap();
        mountinfo.as_file().write_all(contents.as_bytes()).unwrap();
        MountFds::new(
            mountinfo.as_file().try_clone().unwrap(),
            prefix.map(String::from),
        )
    }

    /// Select the mount-point column and decode exactly the escapes emitted by Linux.
    #[test]
    fn mount_root_decodes_paths() {
        let cases = [
            ("/plain", "/plain"),
            (r"/mount\040with\040spaces", "/mount with spaces"),
            (r"/tab\011name", "/tab\tname"),
            (r"/line\012name", "/line\nname"),
            (r"/back\134slash", r"/back\slash"),
            (
                r"/literal\134040\134011\134012\134134",
                r"/literal\040\011\012\134",
            ),
            (r"/mixed\040\011\012\134\134040", "/mixed \t\n\\\\040"),
            (r"/trailing\134", "/trailing\\"),
            (
                "/日本語/white\u{a0}space\rname",
                "/日本語/white\u{a0}space\rname",
            ),
        ];
        for (encoded, decoded) in cases {
            let mounts = mount_fds(
                &format!(
                    "10 1 8:1 / /other rw - ext4 /dev/root rw\n\
                     42 10 8:2 /different\\040root {encoded} rw shared:1 - ext4 /dev/data rw\n"
                ),
                None,
            );
            assert_eq!(mounts.get_mount_root(42).unwrap(), decoded, "{encoded}");
            // Each lookup must rewind the table, including after reading another mount.
            assert_eq!(mounts.get_mount_root(10).unwrap(), "/other");
            assert_eq!(mounts.get_mount_root(42).unwrap(), decoded);
        }
    }

    /// Decode before matching the host prefix used by chroot sandboxing.
    #[test]
    fn mount_root_strips_decoded_prefix() {
        for (encoded, prefix, expected) in [
            (
                r"/share\040dir/child\040mount",
                "/share dir",
                "/child mount",
            ),
            (r"/share\134040/child\134040", r"/share\040", r"/child\040"),
            (r"/share\040dir", "/share dir", "/"),
            ("/", "/share dir", "/"),
            ("/outside", "/share dir", "/"),
        ] {
            let mounts = mount_fds(
                &format!("42 1 8:1 / {encoded} rw - ext4 /dev/root rw\n"),
                Some(prefix),
            );
            assert_eq!(mounts.get_mount_root(42).unwrap(), expected);
        }
    }

    /// Missing IDs and incomplete entries keep returning EINVAL.
    #[test]
    fn mount_root_requires_matching_entry() {
        for table in [
            "",
            "42 1 8:1 /",
            "43 1 8:1 / /other rw - ext4 /dev/root rw\n",
        ] {
            let error = mount_fds(table, None).get_mount_root(42).unwrap_err();
            assert_eq!(error.into_inner().raw_os_error(), Some(libc::EINVAL));
        }
    }

    /// Exercise open, statx, and reopen against a real path without requiring mount privileges.
    #[test]
    fn get_opens_decoded_mount_path() {
        let directory = TempDir::new_with_prefix("/tmp/virtiofsd_mount_fd_").unwrap();
        let path = directory.as_path().join("mount \t\n\\040");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("probe"), b"mounted data").unwrap();
        let expected = statx(&File::open(&path).unwrap(), None).unwrap();
        let encoded = format!(
            "{}/mount\\040\\011\\012\\134040",
            directory.as_path().display()
        );
        let mounts = mount_fds(
            &format!(
                "{} 1 8:1 / {encoded} rw - ext4 /dev/root rw\n",
                expected.mnt_id
            ),
            None,
        );
        let proc_self_fd = File::open("/proc/self/fd").unwrap();
        let mount = mounts
            .get(expected.mnt_id, |fd, flags| {
                reopen_fd_through_proc(&fd, flags, &proc_self_fd)
            })
            .unwrap();
        let actual = statx(mount.file(), None).unwrap();
        assert_eq!(actual.mnt_id, expected.mnt_id);
        assert_eq!(actual.st.st_ino, expected.st.st_ino);
        let mut data = String::new();
        openat(mount.file(), "probe", libc::O_RDONLY)
            .unwrap()
            .read_to_string(&mut data)
            .unwrap();
        assert_eq!(data, "mounted data");
        let cached = mounts
            .get(expected.mnt_id, |_, _| panic!("mount already cached"))
            .unwrap();
        assert!(Arc::ptr_eq(&mount, &cached));
    }
}
