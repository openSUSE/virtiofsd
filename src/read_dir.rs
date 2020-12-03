use crate::filesystem::{self, DirEntry, DirectoryIterator};
use std::{
    io,
    mem::size_of,
    ops::{Deref, DerefMut},
    os::unix::io::AsRawFd,
};
use vm_memory::ByteValued;

const CURRENT_DIR_CSTR: &[u8] = b".\0";
const PARENT_DIR_CSTR: &[u8] = b"..\0";

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
struct LinuxDirent64 {
    d_ino: libc::ino64_t,
    d_off: libc::off64_t,
    d_reclen: libc::c_ushort,
    d_ty: libc::c_uchar,
}
unsafe impl ByteValued for LinuxDirent64 {}

#[derive(Default)]
pub struct ReadDir<B = Vec<u8>> {
    buf: B,
    start: usize,
    end: usize,
}

impl<B: DerefMut<Target = [u8]>> ReadDir<B> {
    pub fn new<D: Deref>(mut buf: B, dir: D, offset: u64) -> io::Result<Self>
    where
        D::Target: AsRawFd,
    {
        let dir = dir.deref();
        // Safe because this doesn't modify any memory and we check the return value.
        let res =
            unsafe { libc::lseek64(dir.as_raw_fd(), offset as libc::off64_t, libc::SEEK_SET) };
        if res < 0 {
            return Err(io::Error::last_os_error());
        }

        // Safe because the kernel guarantees that it will only write to `buf` and we check the
        // return value.
        let res = unsafe {
            libc::syscall(
                libc::SYS_getdents64,
                dir.as_raw_fd(),
                buf.as_mut_ptr() as *mut LinuxDirent64,
                buf.len() as libc::c_int,
            )
        };
        if res < 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(ReadDir {
            buf,
            start: 0,
            end: res as usize,
        })
    }
}

impl<B: DerefMut<Target = [u8]>> ReadDir<B> {
    // The directory entry metadata and the entry name
    fn get_next<'b>(
        buf: &'b B,
        start: &mut usize,
        end: usize,
    ) -> Option<(&'b LinuxDirent64, &'b [u8])> {
        let rem = &buf[*start..end];
        if rem.is_empty() {
            return None;
        }

        // We only use debug asserts here because these values are coming from the kernel and we
        // trust them implicitly.
        debug_assert!(
            rem.len() >= size_of::<LinuxDirent64>(),
            "not enough space left in `rem`"
        );

        let (front, back) = rem.split_at(size_of::<LinuxDirent64>());
        let dirent64 =
            LinuxDirent64::from_slice(front).expect("unable to get LinuxDirent64 from slice");
        debug_assert!(
            back.len() >= dirent64.d_reclen as usize,
            "back is smaller than `d_reclen`"
        );

        let namelen = dirent64.d_reclen as usize - size_of::<LinuxDirent64>();
        debug_assert!(namelen <= back.len(), "back is smaller than `namelen`");

        let name = &back[..namelen];
        *start += namelen;

        Some((dirent64, name))
    }
}

impl<'f, B: DerefMut<Target = [u8]>> DirectoryIterator<'f> for ReadDir<B> {
    type Item = filesystem::DirIterItem;

    fn next<'b, 's: 'b>(&'s mut self) -> Option<DirEntry<'b>>
    where
        'f: 's,
    {
        while let Some((dirent64, name)) = Self::get_next(&self.buf, &mut self.start, self.end) {
            // We don't want to report the "." and ".." entries.
            if !name.starts_with(CURRENT_DIR_CSTR) && !name.starts_with(PARENT_DIR_CSTR) {
                return Some(DirEntry {
                    ino: dirent64.d_ino,
                    offset: dirent64.d_off as u64,
                    type_: u32::from(dirent64.d_ty),
                    name,
                });
            }
        }
        None
    }
}
