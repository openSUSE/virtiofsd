use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};

// We use `rw-------` in case we want to use group and other `rw` bits to encode the file type,
// so all 0's is a symlink
pub(crate) const HOST_SYMLINK_MODE: libc::mode_t = libc::S_ISVTX | libc::S_IRUSR | libc::S_IWUSR;

const PHONY_XATTR_ROOT: &str = "user.virtiofs.phony";

pub(crate) fn block_xattr(name: &[u8]) -> bool {
    // FIXME: this will be checked using PHONY_XATTR_ROOT
    let phony_data = "phony".as_bytes();
    let phony_data2 = "virtiofs.phony".as_bytes();
    let phony_data3 = "user.virtiofs.phony".as_bytes();

    name.starts_with(phony_data) || name.starts_with(phony_data2) || name.starts_with(phony_data3)
}

// we usa the sticky bit as a marker for our ohony special files
#[inline]
pub(crate) fn is_phony(mode: u32) -> bool {
    (mode & libc::S_IFMT == libc::S_IFREG) && (mode & libc::S_ISVTX != 0)
}

#[inline]
pub(crate) fn mode(mode: u32) -> u32 {
    if is_phony(mode) {
        symlink_mode()
    } else {
        mode
    }
}

#[inline]
fn symlink_mode() -> u32 {
    let usr = libc::S_IRUSR | libc::S_IWUSR | libc::S_IXUSR;
    let grp = libc::S_IRGRP | libc::S_IWGRP | libc::S_IXGRP;
    let oth = libc::S_IROTH | libc::S_IWOTH | libc::S_IXOTH;
    libc::S_IFLNK | usr | grp | oth
}

pub(crate) fn readlink(fd: BorrowedFd) -> io::Result<Vec<u8>> {
    let name = CString::new(format!("{PHONY_XATTR_ROOT}.link")).expect("infallible");
    let mut buf = vec![0u8; libc::PATH_MAX as usize + 1];

    // Safe because this will only modify the contents of `buf`.
    let res = unsafe {
        libc::fgetxattr(
            fd.as_raw_fd(),
            name.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_void,
            libc::PATH_MAX as libc::size_t,
        )
    };

    if res < 0 {
        return Err(io::Error::last_os_error());
    };

    buf.resize(res as usize, 0);
    Ok(buf)
}

pub(crate) fn symlink(fd: RawFd, linkname: &CStr) -> io::Result<()> {
    let name = CString::new(format!("{PHONY_XATTR_ROOT}.link")).expect("infallible");
    let res = unsafe {
        libc::fsetxattr(
            fd,
            name.as_ptr(),
            linkname.as_ptr() as *const libc::c_void,
            linkname.to_bytes_with_nul().len(),
            libc::XATTR_CREATE,
        )
    };
    if res == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
