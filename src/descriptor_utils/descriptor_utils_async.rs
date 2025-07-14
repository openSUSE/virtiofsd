// Copyright 2025 Kaisei Hishida.
//
// SPDX-License-Identifier: (Apache-2.0 AND BSD-3-Clause)
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::sync::Mutex;
use vm_memory::bitmap::{Bitmap, BitmapSlice};

use crate::async_wrappers::IoUringState;
use crate::descriptor_utils::{Reader, Writer};
use crate::file_traits::file_traits_async::{FileReadAtVolatileTask, FileWriteAtVolatileTask};
use crate::oslib;

impl<'a, B: Bitmap + BitmapSlice + Send + 'static> Writer<'a, B> {
    /// Read data into the descriptor chain buffer from a File at offset `off`.
    ///
    /// Return the number of bytes read.  This can be less than `count` if there isn't enough data
    /// in the descriptor chain buffer, or if the file’s read function performed a short read.
    pub async fn read_from_file_at_async(
        &mut self,
        ring: &Mutex<IoUringState>,
        src: &File,
        count: usize,
        off: u64,
        flags: Option<oslib::ReadvFlags>,
    ) -> io::Result<usize> {
        let bufs = self.buffer.head(count);

        // SAFETY: The returned Future is awaited immediately, well before `self.buffer` is dropped.
        let bytes_read =
            unsafe { FileReadAtVolatileTask::new(ring, src.as_raw_fd(), bufs, off, flags) }.await?;

        self.buffer.discard_front(bytes_read)?;
        Ok(bytes_read)
    }
}

impl<'a, B: Bitmap + BitmapSlice + Send + 'static> Reader<'a, B> {
    /// Write data from the descriptor chain buffer into a File at offset `off`.
    ///
    /// Return the number of bytes written.  This can be less than `count` if there isn't enough
    /// data in the descriptor chain buffer, or if the file’s write function performed a short
    /// write.
    pub async fn write_to_file_at_async(
        &mut self,
        ring: &Mutex<IoUringState>,
        dst: &File,
        count: usize,
        off: u64,
        flags: Option<oslib::WritevFlags>,
    ) -> io::Result<usize> {
        let bufs = self.buffer.head(count);

        // SAFETY: The returned Future is awaited immediately, well before `self.buffer` is dropped.
        let bytes_written =
            unsafe { FileWriteAtVolatileTask::new(ring, dst.as_raw_fd(), bufs, off, flags) }
                .await?;

        self.buffer.discard_front(bytes_written)?;
        Ok(bytes_written)
    }
}
