// Copyright 2025 Kaisei Hishida.
//
// SPDX-License-Identifier: (Apache-2.0 AND BSD-3-Clause)
use crate::async_wrappers::IoUringState;
use crate::oslib;
use crate::passthrough::asyncs::io_uring::IoUringAwait;
use futures::FutureExt as _;
use io_uring::opcode;
use io_uring::types::Fd;
use libc::size_t;
use std::ffi::c_void;
use std::future::Future;
use std::io;
use std::os::fd::RawFd;
use std::sync::Mutex;
use std::task::{Context, Poll};
use vm_memory::bitmap::BitmapSlice;
use vm_memory::volatile_memory::PtrGuardMut;
use vm_memory::VolatileSlice;

/// A `Future` that reads from a file at a specific offset into buffers.
///
/// This `Future` cannot be written as an async block because in that case
/// the generated annonymous `Future` would not be `Send` due to capturing `PtrGuardMut`,
/// which itself is not `Send`. However, we know that this `Future` as a whole is actually `Send`,
/// and we need a concrete type in order to implement `Send` manually (via an unsafe implementation).
pub struct FileReadAtVolatileTask<'ring, 'a, B: BitmapSlice> {
    fd: RawFd,
    ring: &'ring Mutex<IoUringState>,
    bufs: Vec<&'a VolatileSlice<'a, B>>,
    slice_guards: Vec<PtrGuardMut>,
    offset: u64,
    flags: Option<oslib::ReadvFlags>,
    io_uring_op: Option<IoUringAwait>,
    iovecs: Option<Vec<libc::iovec>>,
}

// SAFETY: This type is safe to send across threads because inner raw pointers
// are just for being read by the kernel. This does not imply that this type is not `Send`.
unsafe impl<'ring, 'a, B: BitmapSlice + Send> Send for FileReadAtVolatileTask<'ring, 'a, B> {}

impl<'ring, 'a, B: BitmapSlice> FileReadAtVolatileTask<'ring, 'a, B> {
    /// Create a Future that reads from a file at a specific offset into `bufs`.
    ///
    /// # Safety
    /// The caller must ensure that `bufs` remain valid until this task completes.
    /// i.e. the caller must `await` the returned Future before `bufs` become invalid.
    pub unsafe fn new(
        ring: &'ring Mutex<IoUringState>,
        f: RawFd,
        bufs: Vec<&'a VolatileSlice<'a, B>>,
        offset: u64,
        flags: Option<oslib::ReadvFlags>,
    ) -> Self {
        let slice_guards: Vec<_> = bufs.iter().map(|s| s.ptr_guard_mut()).collect();
        Self {
            fd: f,
            ring,
            bufs,
            slice_guards,
            offset,
            flags,
            io_uring_op: None,
            iovecs: None,
        }
    }
}

impl<B: BitmapSlice> Future for FileReadAtVolatileTask<'_, '_, B> {
    type Output = io::Result<usize>;

    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.io_uring_op.is_none() {
            let iovecs: Vec<libc::iovec> = self
                .slice_guards
                .iter()
                .map(|s| libc::iovec {
                    iov_base: s.as_ptr() as *mut c_void,
                    iov_len: s.len() as size_t,
                })
                .collect();

            if iovecs.is_empty() {
                return Poll::Ready(Ok(0));
            }

            self.iovecs = Some(iovecs);
            let self_iovecs_ref = self.iovecs.as_ref().unwrap();
            let flags = self.flags.unwrap_or(oslib::ReadvFlags::empty());
            let e = opcode::Readv::new(
                Fd(self.fd),
                self_iovecs_ref.as_ptr(),
                self_iovecs_ref.len() as u32,
            )
            .offset(self.offset)
            .rw_flags(flags.bits())
            .build();
            self.io_uring_op = Some(unsafe { IoUringAwait::op(self.ring, e) });
        }
        // SAFETY: Safe because only bytes inside the buffers are
        // accessed and the kernel is expected to handle arbitrary
        // memory for I/O. The pointers into the slice are valid since
        // the slice_guards are alive. We immediately poll the created Future,
        // so `e` remains valid.
        let ret = match self.io_uring_op.as_mut().unwrap().poll_unpin(cx) {
            Poll::Ready(result) => result,
            Poll::Pending => return Poll::Pending,
        };

        if ret >= 0 {
            let mut total = 0;
            for vs in self.bufs.iter() {
                // Each `VolatileSlice` has a "local" bitmap (i.e., the offset 0 in the
                // bitmap corresponds to the beginning of the `VolatileSlice`)
                vs.bitmap()
                    .mark_dirty(0, std::cmp::min(ret as usize - total, vs.len()));
                total += vs.len();
                if total >= ret as usize {
                    break;
                }
            }
            Poll::Ready(Ok(ret as usize))
        } else {
            Poll::Ready(Err(io::Error::from_raw_os_error(-ret)))
        }
    }
}
