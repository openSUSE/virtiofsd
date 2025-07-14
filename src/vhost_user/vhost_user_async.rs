// Copyright 2025 Kaisei Hishida.
//
// SPDX-License-Identifier: (Apache-2.0 AND BSD-3-Clause)
use std::io;
use std::sync::{Arc, Mutex};

use io_uring::cqueue;
use smol::Executor;
use vhost_user_backend::{VringMutex, VringT as _};
use virtio_queue::QueueOwnedT as _;
use vm_memory::GuestAddressSpace as _;

use crate::async_wrappers::{IoUringRuntime, IoUringRuntimeWithEventFd, IoUringState};
use crate::descriptor_utils::{Reader, Writer};
use crate::filesystem::{FileSystem, SerializableFileSystem};
use crate::passthrough::asyncs::io_uring::{IoUringAwait, SubmissionBatchingGuard};
use crate::vhost_user::{
    Error, LoggedMemoryAtomic, Result, VhostUserFsThread, HIPRIO_QUEUE_EVENT, IO_URING_EVENT,
    REQ_QUEUE_EVENT,
};

impl<F: FileSystem + SerializableFileSystem + Send + Sync + 'static> VhostUserFsThread<F>
where
    <F as FileSystem>::Inode: Send,
    <F as FileSystem>::Handle: Send,
    <F as FileSystem>::DirIter: Send,
{
    pub fn handle_event_async(
        &self,
        device_event: u16,
        vrings: &[VringMutex<LoggedMemoryAtomic>],
    ) -> io::Result<()> {
        let idx = match device_event {
            HIPRIO_QUEUE_EVENT => {
                debug!("HIPRIO_QUEUE_EVENT");
                0
            }
            REQ_QUEUE_EVENT => {
                debug!("QUEUE_EVENT");
                1
            }
            IO_URING_EVENT => {
                debug!("IO_URING_EVENT");
                if !self.event_idx {
                    self.complete_tasks();
                    return Ok(());
                }
                // `complete_tasks()` can take a while, and if the guest is active,
                // it might send new requests in the meantime.
                // Checking the main virtqueue for new entries is cheap,
                // so we might as well do it afterwards to skip a maybe unnecessary `epoll()`.
                // Disable notifications so we also save the guest a VM exit in that case.
                let idx = 1;
                vrings[idx].disable_notification().unwrap();
                self.complete_tasks();
                idx
            }
            _ => return Err(Error::HandleEventUnknownEvent.into()),
        };
        if self.event_idx {
            if device_event != IO_URING_EVENT {
                // If event_idx is enabled, we disable notification while processing the queue
                // In case of IO_URING_EVENT, we already did it above
                vrings[idx].disable_notification().unwrap();
            }
            // vm-virtio's Queue implementation only checks avail_index
            // once, so to properly support EVENT_IDX we need to keep
            // calling process_queue() until it stops finding new
            // requests on the queue.
            loop {
                // we can't recover from an error here, so let's hope it's transient
                if let Err(e) = self.process_queue_async(vrings[idx].clone()) {
                    error!("processing the vring: {e}");
                }
                if !vrings[idx].enable_notification().unwrap() {
                    break;
                }
                vrings[idx].disable_notification().unwrap();
            }
        } else {
            // Without EVENT_IDX, a single call is enough.
            self.process_queue_async(vrings[idx].clone())?;
        }

        Ok(())
    }

    fn complete_tasks(&self) {
        let IoUringRuntimeWithEventFd {
            io_uring_runtime: IoUringRuntime { state, executor },
            event_fd: io_uring_evt,
        } = self.io_uring_runtime.as_ref().unwrap();
        while let Err(e) = io_uring_evt.read() {
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            panic!("{}: EventFD should always be valid.", e);
        }
        // Completing tasks may take some time, and new completions may arrive during that period.
        // The kernel might fill the completion queue when we issue `io_uring_enter`.
        // Since checking `.completion().is_empty()` is syscall-free, and inexpensive, we re-check for new CQEs.
        // We loop here until no more new CQEs are found.
        loop {
            let cqes: Vec<_> = {
                let mut guard = state
                    .lock()
                    .expect("Lock poisoned: another thread must have panicked");
                if guard.ring.completion().is_empty() {
                    return;
                }
                guard.ring.completion().collect()
            };
            if !Self::complete_tasks_cqes(cqes, state, executor) {
                return;
            }
        }
    }

    /// Completes tasks by processing the completion queue entries (CQEs).
    /// Returns true if any new submission entries are submitted while completing.
    /// For example, if a short read/write occurs, it results in another submission
    /// within the same task.
    fn complete_tasks_cqes(
        cqes: Vec<cqueue::Entry>,
        state: &Arc<Mutex<IoUringState>>,
        executor: &Arc<Executor<'static>>,
    ) -> bool {
        // This batches potential follow-up submissions triggered by a short read/write.
        // In such cases, another I/O is issued before responding to the guest,
        // and this batching ensures they are grouped in the same CQE batch.
        let submission_batching_guard = SubmissionBatchingGuard::new(state);
        for cqe in cqes {
            // Safety: all the submissions in this crate is created by `IoUringAwait::op`.
            unsafe {
                IoUringAwait::process_cqe(cqe);
            }
        }
        loop {
            if !executor.try_tick() {
                break;
            }
        }
        submission_batching_guard.submit_and_drop()
    }

    fn process_queue_async(&self, vring: VringMutex<LoggedMemoryAtomic>) -> Result<bool> {
        let IoUringRuntime { state, executor } = self.io_uring_runtime.as_ref().unwrap().runtime();
        let mut used_any = false;
        let atomic_mem = match &self.mem {
            Some(m) => m.clone(),
            None => return Err(Error::NoMemoryConfigured),
        };

        let avails_descs = vring
            .get_mut()
            .get_queue_mut()
            .iter(atomic_mem.memory())
            .map_err(|_| Error::IterateQueue)?
            .collect::<Vec<_>>();
        {
            let _submission_batching_guard = SubmissionBatchingGuard::new(state);
            for avail_desc in avails_descs {
                used_any = true;

                // Prepare a set of objects that can be moved to the async block.
                let mem = atomic_mem.memory();
                let server = self.server.clone();
                let vu_req = self.vu_req.clone();
                let event_idx = self.event_idx;
                let worker_vring = vring.clone();
                let worker_desc = avail_desc.clone();
                let head_index = worker_desc.head_index();

                executor
                    .spawn(async move {
                        let reader = Reader::new(&mem, worker_desc.clone())
                            .map_err(Error::QueueReader)
                            .unwrap();
                        let writer = Writer::new(&mem, worker_desc)
                            .map_err(Error::QueueWriter)
                            .unwrap();

                        let len = server
                            .handle_message_async(reader, writer, vu_req)
                            .await
                            .map_err(Error::ProcessQueue)
                            .unwrap();
                        let mut vring_state = worker_vring.get_mut();

                        Self::return_descriptor(&mut vring_state, head_index, event_idx, len);
                    })
                    .detach();
            }
            loop {
                if !executor.try_tick() {
                    break;
                }
            }
        }
        Ok(used_any)
    }
}
