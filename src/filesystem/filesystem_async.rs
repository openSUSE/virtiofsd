// Copyright 2025 Kaisei Hishida.
//
// SPDX-License-Identifier: (Apache-2.0 AND BSD-3-Clause)
use std::fs::File;
use std::io;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::async_wrappers::IoUringState;
use crate::filesystem::ZeroCopyWriter;
use crate::oslib;

/// A trait for directly copying data from a `File` into the fuse transport without first storing
/// it in an intermediate buffer.
#[async_trait]
pub trait AsyncZeroCopyWriter: ZeroCopyWriter {
    /// Copies at most `count` bytes from `f` at offset `off` directly into `self` without storing
    /// it in any intermediate buffers. If the return value is `Ok(n)` then it must be guaranteed
    /// that `0 <= n <= count`. If `n` is `0`, then it can indicate one of 4 possibilities:
    ///
    /// 1. There is no more data left in `f`.
    /// 2. End of `f` reached.
    /// 3. There is no more space in `self`.
    /// 4. `count` was `0`.
    ///
    /// Does not do short reads, unless one of the above happens; i.e. will invoke the underlying
    /// file read function until `count` bytes have been read or it returns 0 (cases 1 or 2 above)
    /// or `self` has no more space (case 2 above).
    async fn read_from_file_at(
        &mut self,
        ring: &Mutex<IoUringState>,
        f: &File,
        count: usize,
        off: u64,
        flags: Option<oslib::ReadvFlags>,
    ) -> io::Result<usize>;
}
