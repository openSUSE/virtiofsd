// Copyright 2025 Kaisei Hishida.
//
// SPDX-License-Identifier: (Apache-2.0 AND BSD-3-Clause)
use std::io;

use crate::filesystem::filesystem_async::AsyncZeroCopyWriter;
use crate::filesystem::{Context, FileSystem};
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
}
