// Copyright 2025 Kaisei Hishida.
//
// SPDX-License-Identifier: (Apache-2.0 AND BSD-3-Clause)
use std::io;
use std::os::fd::AsRawFd as _;

use crate::filesystem::filesystem_async::{AsyncZeroCopyReader, AsyncZeroCopyWriter};
use crate::filesystem::{Context, FileSystem};
use crate::oslib;
use crate::passthrough::credentials::drop_effective_cap;
use crate::passthrough::PassthroughFs;

impl PassthroughFs {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn do_read_async<W: AsyncZeroCopyWriter + Send>(
        &self,
        _ctx: Context,
        inode: <Self as FileSystem>::Inode,
        handle: <Self as FileSystem>::Handle,
        mut w: W,
        size: u32,
        offset: u64,
        _lock_owner: Option<u64>,
        _flags: u32,
    ) -> io::Result<usize> {
        let data = self.find_handle(handle, inode)?;

        // This is safe because read_from_file_at uses preadv64, so the underlying file descriptor
        // offset is not affected by this operation.
        let f = data.file.get()?.read().await;
        AsyncZeroCopyWriter::read_from_file_at(
            &mut w,
            self.io_uring().unwrap(),
            f.get_file(),
            size as usize,
            offset,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn do_write_async<R: AsyncZeroCopyReader + Send>(
        &self,
        _ctx: Context,
        inode: <Self as FileSystem>::Inode,
        handle: <Self as FileSystem>::Handle,
        mut r: R,
        size: u32,
        offset: u64,
        _lock_owner: Option<u64>,
        delayed_write: bool,
        kill_priv: bool,
        flags: u32,
    ) -> io::Result<usize> {
        let data = self.find_handle(handle, inode)?;

        // This is safe because write_to_file_at uses `pwritev2(2)`, so the underlying file
        // descriptor offset is not affected by this operation.
        let f = data.file.get()?.read().await;

        {
            let _killpriv_guard = if self.cfg.killpriv_v2 && kill_priv {
                // We need to drop FSETID during a write so that the kernel will remove setuid
                // or setgid bits from the file if it was written to by someone other than the
                // owner.
                drop_effective_cap("FSETID")?
            } else {
                None
            };

            self.clear_file_capabilities(f.as_raw_fd(), false)?;

            // We don't set the `RWF_APPEND` (i.e., equivalent to `O_APPEND`) flag, if it's a
            // delayed write (i.e., using writeback mode or a mem mapped file) even if the file
            // was open in append mode, since the guest kernel sends the correct offset.
            // For non-delayed writes, we set the append mode, if necessary, to correctly handle
            // writes on a file shared among VMs. This case can only be handled correctly if the
            // write on the underlying file is performed in append mode.
            let is_append = flags & libc::O_APPEND as u32 != 0;
            let flags = (!delayed_write && is_append).then_some(oslib::WritevFlags::RWF_APPEND);
            AsyncZeroCopyReader::write_to_file_at(
                &mut r,
                self.io_uring().unwrap(),
                f.get_file(),
                size as usize,
                offset,
                flags,
            )
            .await
        }
    }
}
