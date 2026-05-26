// Copyright 2025 Kaisei Hishida.
//
// SPDX-License-Identifier: (Apache-2.0 AND BSD-3-Clause)
use std::io;
use std::sync::{Arc, Mutex};

use io_uring::IoUring;
use smol::Executor;
use vmm_sys_util::eventfd::EventFd;

use crate::passthrough::asyncs::io_uring::{BatchingState, IoUringAwait};

/// `IoUringState` holds the state of the io_uring instance and its batching state.
pub struct IoUringState {
    /// The io_uring instance
    pub ring: IoUring,
    /// The batching state of the io_uring submissions.
    pub(crate) batching_state: BatchingState,
}

impl IoUringState {
    pub fn new(ring: IoUring) -> Self {
        IoUringState {
            ring,
            batching_state: BatchingState::new(),
        }
    }

    pub fn error_handled_submit(&mut self) {
        loop {
            let sq_len = self.ring.submission().len();
            if sq_len == 0 {
                return;
            }
            match self.ring.submit() {
                Ok(n_submitted) if sq_len == n_submitted => break,
                Ok(n_submitted) => {
                    // `io_uring_enter` should only return zero if the submission queue was empty,
                    // but we early-return above when the queue is empty,
                    // so in this branch we should never find `n_submitted` to be 0.
                    // If it is anyway, we would get stuck in this loop,
                    // so we assert that the return value is non-zero.
                    assert_ne!(n_submitted, 0);
                    // If not all the entries are submitted, we need to submit again to ensure that
                    // every entry is submitted.
                    continue;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted /* EINTR */ => {
                    // Interrupted by a signal, just try again.
                    continue;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock /* EAGAIN */ => {
                    // In case *EAGAIN*, the application should wait for some completions
                    // and try again.
                    for cqe in self.ring.completion() {
                        // Safety: all submissions in virtiofsd are created by `IoUringAwait::op`.
                        unsafe {
                            IoUringAwait::process_cqe(cqe);
                        }
                    }
                    continue;
                }
                Err(e) => panic!(
                        "Unexpected error while submitting io_uring, this must be bug: {}",
                        e
                    ),
            }
        }
    }
}

/// A runtime for io_uring operations.
#[derive(Clone)]
pub struct IoUringRuntime {
    /// The io_uring state including io_uring instance where operations are submitted or results are retrieved.
    pub state: Arc<Mutex<IoUringState>>,
    /// The executor used to run futures.
    pub executor: Arc<Executor<'static>>,
}

impl IoUringRuntime {
    pub fn new(ring: IoUring, executor: Arc<Executor<'static>>) -> Self {
        IoUringRuntime {
            state: Arc::new(Mutex::new(IoUringState::new(ring))),
            executor,
        }
    }
}

/// A wrapper around `IoUringRuntime` and `EventFd`.
pub struct IoUringRuntimeWithEventFd {
    pub(crate) io_uring_runtime: IoUringRuntime,
    pub(crate) event_fd: EventFd,
}

impl IoUringRuntimeWithEventFd {
    pub fn new(io_uring_runtime: IoUringRuntime, event_fd: EventFd) -> Self {
        IoUringRuntimeWithEventFd {
            io_uring_runtime,
            event_fd,
        }
    }

    pub fn runtime(&self) -> &IoUringRuntime {
        &self.io_uring_runtime
    }

    pub fn event_fd(&self) -> &EventFd {
        &self.event_fd
    }
}
