// Copyright 2025 Kaisei Hishida.
//
// SPDX-License-Identifier: (Apache-2.0 AND BSD-3-Clause)
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::Poll;

use futures::channel::oneshot::{Receiver, Sender};
use futures::FutureExt;
use io_uring::cqueue;

use crate::async_wrappers::IoUringState;

/// A `Future` that awaits the completion of an `io_uring` operation.
/// `IoUringAwait` submits an operation and waits for its completion via a `oneshot::Receiver`.
/// It is not responsible for retrieving the result from io_uring's completion queue.
pub struct IoUringAwait {
    receiver: Receiver<i32>,
}

/// `BatchingState` is used to track the state of submission batching of io_uring.
#[derive(Debug)]
pub(crate) struct BatchingState {
    /// Whether there are pending submissions.
    pending: bool,
    /// `batching > 0` indicates that we are currently batching submissions.
    /// `batching` is incremented when a [`SubmissionBatchingGuard`] is created and decremented when it is dropped.
    batching: usize,
}

impl BatchingState {
    pub fn new() -> Self {
        BatchingState {
            pending: false,
            batching: 0,
        }
    }

    pub fn is_batching(&self) -> bool {
        self.batching > 0
    }

    pub fn is_pending(&self) -> bool {
        self.pending
    }

    pub fn set_pending(&mut self) {
        self.pending = true;
    }

    pub fn reset_pending(&mut self) {
        self.pending = false;
    }

    pub fn increment_batching(&mut self) {
        self.batching += 1;
    }

    pub fn decrement_batching(&mut self) {
        if self.batching > 0 {
            self.batching -= 1;
        }
    }
}

/// A guard that ensures that io_uring submissions are batched until it is dropped.
/// `SubmissionBatchingGuard` automatically submits the batched operations when dropped.
pub struct SubmissionBatchingGuard<'a> {
    state: &'a Arc<Mutex<IoUringState>>,
    /// Whether the guard's drop operation has already performed.
    dropped: bool,
}

impl<'a> SubmissionBatchingGuard<'a> {
    pub fn new(state: &'a Arc<Mutex<IoUringState>>) -> Self {
        state
            .lock()
            .expect("Lock poisoned: another thread must have panicked")
            .batching_state
            .increment_batching();
        Self {
            state,
            dropped: false,
        }
    }

    /// Submits the current batch of operations and resets the pending state.
    /// Returns whether actually submitted any operations.
    fn submit(&mut self) -> bool {
        let mut guard = self
            .state
            .lock()
            .expect("Lock poisoned: another thread must have panicked");
        guard.batching_state.decrement_batching();
        if !guard.batching_state.is_pending() {
            return false;
        }

        guard.error_handled_submit();
        guard.batching_state.reset_pending();
        true
    }

    /// Submits the current batch of operations and resets the pending state.
    /// Returns whether actually submitted any operations.
    ///
    /// This is useful when you want to know whether anything was submitted,
    /// while also preventing a second submission when the guard is dropped.
    pub fn submit_and_drop(mut self) -> bool {
        self.dropped = true;
        self.submit()
        // drop without submit
    }
}

impl Drop for SubmissionBatchingGuard<'_> {
    fn drop(&mut self) {
        if self.dropped {
            return;
        }
        self.submit();
    }
}

type IoUringAwaitResultSenderPtr = *mut Sender<i32>;
impl IoUringAwait {
    /// Submits the given io_uring entry and returns a `Future` that will yield the result of the operation.
    ///
    /// It is the caller's responsibility to handle the completion queue entry (CQE).
    /// The caller must reconstruct a `Sender<i32>` from CQE's user_data field using:
    /// `let sender = Box::<Sender<i32>>::from_raw(<user_data>)`,
    /// and send the result via `sender.send(cqe.result())`.
    ///
    /// # Safety
    ///
    /// The caller must not send through the reconstructed sender before the io_uring operation completes.
    ///
    /// The caller must ensure that all memory references or file descriptors are valid
    /// and remain valid until the returned `Future` is polled and yields `Poll::Ready`,
    /// i.e., until it has been awaited.
    pub unsafe fn op(ring: &Mutex<IoUringState>, entry: io_uring::squeue::Entry) -> IoUringAwait {
        let (sender, receiver) = futures::channel::oneshot::channel();
        let sender_ptr: IoUringAwaitResultSenderPtr = Box::into_raw(Box::new(sender));
        let e = entry.user_data(sender_ptr as u64);
        {
            let mut guard = ring
                .lock()
                .expect("Lock poisoned: another thread must have panicked");

            while unsafe { guard.ring.submission().push(&e) }.is_err() {
                // If the submission queue is full, we need to submit the current batch.
                // `SubmissionQueue::push()` can fail only if the submission queue is full.
                guard.error_handled_submit();
                guard.batching_state.reset_pending();
            }

            if !guard.batching_state.is_batching() {
                guard.error_handled_submit();
            } else {
                guard.batching_state.set_pending();
            }
        }
        IoUringAwait { receiver }
    }

    /// Processes a io_uring's completion queue entry. This function will awake
    /// `IoUringAwait` Future.
    /// # Safety
    /// The caller must ensure that `cqe` is a completion issued by using [`IoUringAwait::op`].
    pub(crate) unsafe fn process_cqe(cqe: cqueue::Entry) {
        let sender_ptr = cqe.user_data();
        // Safety: The user_data is a pointer to a `Box::<Sender<i32>>` that we created
        let sender = unsafe { Box::from_raw(sender_ptr as IoUringAwaitResultSenderPtr) };
        log::debug!("Completing task with result {}", cqe.result());
        sender
            .send(cqe.result())
            .expect("Receiver should not be dropped");
    }
}

impl Future for IoUringAwait {
    type Output = i32;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match this.receiver.poll_unpin(cx) {
            Poll::Ready(result) => Poll::Ready(result.expect("sender should not be dropped")),
            Poll::Pending => Poll::Pending,
        }
    }
}
