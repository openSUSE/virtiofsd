// Copyright 2019 Intel Corporation. All Rights Reserved.
//
// SPDX-License-Identifier: (Apache-2.0 AND BSD-3-Clause)

//! Watchdog for the guest processing the used vring.
//!
//! It seems possible in very rare cases that the interrupt we send to the guest via the call FD
//! after adding buffers to the used vring is lost.  This module provides a watchdog to re-send the
//! interrupt in case the guest seems to be stuck.
//!
//! To use the watchdog, create a [`UsedVringWatchdogLeash`] before polling a virtqueue (via
//! [`UsedVringWatchdog::begin_virtqueue_polling_by_ref()`] or
//! [`UsedVringWatchdog::begin_virtqueue_polling_owned()`]).  When this object is dropped (i.e. the
//! leash is taken off), the watchdog (running in a dedicated thread) is armed and will check that
//! the guest makes progress on the used buffer vring until it catches up with the end of the
//! vring.  If the guest is found to not make any progress for [`WATCHDOG_TIMEOUT`], the watchdog
//! sends a notification.
//!
//! When polling the virtqueue, all accesses must go through [`UsedVringWatchdogLeash`] or
//! references to it ([`DescriptorGuard`]).  This ensures that the watchdog is armed only once all
//! descriptors have been processed and returned to the guest, so that we don’t unnecessarily arm
//! it while polling is still ongoing.

use super::LoggedMemoryAtomic;
use std::collections::HashMap;
use std::mem;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use vhost_user_backend::{VringMutex, VringState, VringStateMutGuard, VringT};
use virtio_queue::QueueT;
use vm_memory::{Bytes, GuestAddress, GuestAddressSpace, GuestMemory};

/// Timeout we give the guest to process used buffers.
///
/// The lower this is, the less impact a missed call will have.  However, there may also be more
/// false positives.  Therefore, this duration needs to be weighed against the cost of a false
/// positive (i.e. a spurious call to the guest).  100 ms greatly outweighs the cost of a spurious
/// call, so it seems OK.
const WATCHDOG_TIMEOUT: Duration = Duration::from_millis(100);

/// Mutex guard of `VringMutex`.
type VringMutexGuard<'a> =
    <VringMutex<LoggedMemoryAtomic> as VringStateMutGuard<'a, LoggedMemoryAtomic>>::G;

/// Condition on which a watchdog can be armed.
///
/// Every watchdog thread has its own combination of `Mutex` and `Condvar`.  Putting something
/// into the mutex and notifying the condvar arms the watchdog for that virtqueue; removing the
/// object from the mutex (and notifying the condvar) disarms the watchdog.
type WatchdogCond = Arc<(Mutex<Option<WatchdogAwait>>, Condvar)>;

/// Watchdog to check the guest processing the used vring.
///
/// After returning a batch of descriptors to the used vring (and most likely notifying the guest),
/// we want to install a timeout to see the guest actually processing those descriptors.  If it
/// seems stuck, we re-send a new interrupt.
#[derive(Default)]
pub(super) struct UsedVringWatchdog {
    /// Threads that act as the watchdog.
    ///
    /// There is one thread per virtqueue, and this map maps queue indices to watchdog threads.
    threads: Mutex<HashMap<usize, JoinHandle<()>>>,

    /// Workloads for the watchdogs.
    condvars: Mutex<HashMap<usize, WatchdogCond>>,
}

/// Information for arming a watchdog.
struct WatchdogAwait {
    /// Guest memory.
    mem: LoggedMemoryAtomic,

    /// Virtqueue to watch.
    vring: VringMutex<LoggedMemoryAtomic>,

    /// Initial used event index.
    ///
    /// The watchdog will check the virtqueue’s `used_event` after the timeout, compare it, and if
    /// it is still the same and not at the end of the used vring, i.e. if the guest did not make
    /// progress even though there is still work to be done, re-send the notification to the guest
    /// (i.e. re-trigger the interrupt).
    used_event: u16,
}

/// Guard around `&mut VringState`.
///
/// Can be either a mutex guard object or just the reference itself.
pub(super) enum VringStateGuard<'a> {
    /// Plain reference.
    Ref(&'a mut VringState<LoggedMemoryAtomic>),

    /// Mutex guard.
    Mutex(VringMutexGuard<'a>),
}

/// Allow mutable access to `VringState`.
pub(super) trait VringDeref {
    /// Gain mutable access to `VringState`.
    ///
    /// May return a plain `&mut VringState` reference or a mutex guard.
    fn vring_state(&mut self) -> VringStateGuard<'_>;
}

/// Puts the used vring watchdog “on a leash”.
///
/// When dropped, the “leash is taken off”, i.e. the watchdog is armed.
///
/// Before processing a virtqueue, obtain this guard via
/// [`UsedVringWatchdog::begin_virtqueue_polling_by_ref()`] or
/// [`UsedVringWatchdog::begin_virtqueue_polling_owned()`].  Drop them when all new descriptors in the
/// virtqueue have been processed (i.e. polling ends).  This will arm the watchdog to check that
/// the guest will indeed process the descriptors we returned.
///
/// When processing a single descriptor chain, obtain a guard for it via
/// [`UsedVringWatchdogLeash::guard_descriptor_by_ref()`] or
/// [`UsedVringWatchdogLeash::guard_descriptor_owned()`].  This ensures that the
/// `UsedVringWatchdogLeash` object is not dropped until all descriptors have been processed and
/// returned via the used vring.
///
/// Use the `by_ref` variants for when it is possible to keep a mutable reference to the virtqueue
/// throughout (i.e. serial single-thread processing), and the `owned` variants to re-lock the
/// virtqueue mutex whenever accessing the virtqueue (i.e. processing in a thread pool).
pub(super) struct UsedVringWatchdogLeash<'a, W: Deref<Target = UsedVringWatchdog> + 'a> {
    /// Reference to the central watchdog object.
    watchdog: W,

    /// Virtqueue index.
    vring_index: usize,

    /// Vring state reference, if available.
    ///
    /// If not available, to get this reference, `vring_mutex` will be locked instead.
    vring_ref: Option<&'a mut VringState<LoggedMemoryAtomic>>,

    /// Vring mutex.
    ///
    /// `Option` so it can be `.take()`n in `drop()` and moved into the watchdog thread.
    vring_mutex: Option<VringMutex<LoggedMemoryAtomic>>,

    /// Guest memory.
    ///
    /// `Option` so it can be `.take()`n in `drop()` and moved into the watchdog thread.
    mem: Option<LoggedMemoryAtomic>,

    /// Keep track of whether any work was performed.
    ///
    /// Set to `true` when
    /// [`guard_descriptor_by_ref()`](UsedVringWatchdogLeash::guard_descriptor_by_ref()) or
    /// [`guard_descriptor_owned()`](UsedVringWatchdogLeash::guard_descriptor_owned()) is called.
    ///
    /// This way, we can skip arming the watchdog if no descriptors have been processed (e.g.
    /// because of a spurious kick from the guest).
    processed_any: AtomicBool,
}

/// Guards processing a descriptor chain.
///
/// Obtain this via [`UsedVringWatchdogLeash::guard_descriptor_by_ref()`] or
/// [`UsedVringWatchdogLeash::guard_descriptor_owned()`] when processing a descriptor chain, and
/// access the virt queue only through [`DescriptorGuard::vring_state()`].  This ensures that the
/// [`UsedVringWatchdogLeash`] is not dropped until all descriptors have been processed and
/// returned via the used vring.
pub(super) enum DescriptorGuard<'a, 'b> {
    /// Plain reference.
    Ref(&'a mut UsedVringWatchdogLeash<'b, &'b UsedVringWatchdog>),

    /// Reference through an `Arc`.
    Arc(Arc<UsedVringWatchdogLeash<'static, Arc<UsedVringWatchdog>>>),
}

impl UsedVringWatchdog {
    /// Create a new instance.
    pub fn new() -> Self {
        UsedVringWatchdog::default()
    }

    /// Ensure there is a thread for the given virtqueue.
    fn ensure_thread(&self, vring_index: usize) {
        let mut threads = self.threads.lock().unwrap();
        if threads.contains_key(&vring_index) {
            return;
        }

        let condvar = Arc::new((Mutex::new(None), Condvar::new()));

        self.condvars
            .lock()
            .unwrap()
            .insert(vring_index, Arc::clone(&condvar));

        // This is the watchdog thread code
        let watchdog_fn = move || {
            let (mutex, cvar) = &*condvar;
            let mut watch_next_used = mutex.lock().unwrap();

            loop {
                // Wait until we get armed
                watch_next_used = cvar
                    .wait_while(watch_next_used, |locked| locked.is_none())
                    .unwrap();

                // Wait until the timeout or we get disarmed
                watch_next_used = cvar
                    .wait_timeout_while(watch_next_used, WATCHDOG_TIMEOUT, |locked| {
                        locked.is_some()
                    })
                    .unwrap()
                    .0;

                // (Note that this `.take()` disarms us)
                let Some(WatchdogAwait {
                    mem,
                    vring,
                    used_event,
                }) = watch_next_used.take()
                else {
                    continue;
                };

                // If we ran into the timeout (i.e. did not get disarmed), check whether the guest
                // made progress.
                let vring_state = vring.get_ref();
                let queue = vring_state.get_queue();
                let used_index = queue.next_used();
                let Some(current_used_event) = get_used_event(&*mem.memory(), queue) else {
                    // Should never happen
                    error!("Failed to get used_event from vring {vring_index}");
                    continue;
                };

                // Did the guest reach the end of the used vring?
                if current_used_event == used_index {
                    // Yes, so we can stay disarmed
                    continue;
                }

                // Is the guest still on the same index as last time?
                if current_used_event == used_event {
                    warn!("Guest has stopped processing the used vring for {WATCHDOG_TIMEOUT:#?}, re-sending notification");
                    warn!("queue state: {:#x?}", queue.state());
                    warn!("used_event: {current_used_event:#x}");

                    vring_state.signal_used_queue().unwrap();
                }

                drop(vring_state);

                // Whether the guest made progress or not, it did not reach the end of the used
                // vring yet, so re-arm us and keep monitoring
                *watch_next_used = Some(WatchdogAwait {
                    mem,
                    vring,
                    used_event: current_used_event,
                });
            }
        };

        // Given that `thread::spawn()` unwraps this as well, errors seem to be unlikely enough
        // that we can just unwrap, too.
        // Linux limits names to 15 characters, so be concise.
        let watchdog_thread = thread::Builder::new()
            .name(format!("used-r{vring_index}-wdog"))
            .spawn(watchdog_fn)
            .expect("Failed to spawn used vring watchdog thread");

        threads.insert(vring_index, watchdog_thread);
    }

    /// Disarm the watchdog for the given virtqueue.
    fn disarm_thread(&self, vring_index: usize) {
        let condvars = self.condvars.lock().unwrap();
        let (mutex, cvar) = condvars.get(&vring_index).unwrap().as_ref();
        if mutex.lock().unwrap().take().is_some() {
            cvar.notify_one();
        }
    }

    /// Mark the start of polling of the given virtqueue.
    ///
    /// Return a [`UsedVringWatchdogLeash`] for the given virtqueue that will arm its used vring
    /// watchdog once dropped.
    ///
    /// In contrast to [`begin_virtqueue_polling_owned()`](Self::begin_virtqueue_polling_owned()),
    /// accessing the virtqueue (e.g. through [`UsedVringWatchdogLeash::vring_state()`] will
    /// generally use the plain reference `vring_ref`, i.e. need not lock a mutex.
    ///
    /// Still, this function takes the `VringMutex` in addition to the plain reference so the
    /// watchdog will be able to access the virtqueue after the lifetime `'a`.
    pub fn begin_virtqueue_polling_by_ref<'a>(
        &'a self,
        guest_mem: LoggedMemoryAtomic,
        vring_index: usize,
        vring_ref: &'a mut VringState<LoggedMemoryAtomic>,
        vring_mutex: VringMutex<LoggedMemoryAtomic>,
    ) -> UsedVringWatchdogLeash<'a, &'a Self> {
        self.ensure_thread(vring_index);

        UsedVringWatchdogLeash {
            watchdog: self,
            vring_index,
            vring_ref: Some(vring_ref),
            vring_mutex: Some(vring_mutex),
            mem: Some(guest_mem),
            processed_any: AtomicBool::new(false),
        }
    }

    /// Mark the start of polling of the given virtqueue.
    ///
    /// Return a [`UsedVringWatchdogLeash`] for the given virtqueue that will arm its used vring
    /// watchdog once dropped.
    ///
    /// In contrast to
    /// [`begin_virtqueue_polling_by_ref()`](Self::begin_virtqueue_polling_by_ref()), does not
    /// capture a plain reference to the virtqueue, so is not bound by any lifetime.  However, the
    /// downside is that accessing the virtqueue (via
    /// [`lock_vring_state()`](UsedVringWatchdogLeash::lock_vring_state()) or
    /// [`DescriptorGuard::vring_state()`]) will always need to lock the `vring_mutex`.
    pub fn begin_virtqueue_polling_owned(
        self: Arc<Self>,
        guest_mem: LoggedMemoryAtomic,
        vring_index: usize,
        vring_mutex: VringMutex<LoggedMemoryAtomic>,
    ) -> UsedVringWatchdogLeash<'static, Arc<Self>> {
        self.ensure_thread(vring_index);

        UsedVringWatchdogLeash {
            watchdog: self,
            vring_index,
            vring_ref: None,
            vring_mutex: Some(vring_mutex),
            mem: Some(guest_mem),
            processed_any: AtomicBool::new(false),
        }
    }
}

impl<W: Deref<Target = UsedVringWatchdog>> UsedVringWatchdogLeash<'_, W> {
    /// Lock the inner `VringMutex`, gaining access to the `VringState`.
    ///
    /// The returned object implements `DerefMut<Target = VringState>`.
    ///
    /// Prefer to use [`vring_state()`](Self::vring_state()) if possible.
    pub fn lock_vring_state(&self) -> VringMutexGuard<'_> {
        self.vring_mutex.as_ref().unwrap().get_mut()
    }
}

impl<'a> UsedVringWatchdogLeash<'a, Arc<UsedVringWatchdog>> {
    /// Guard processing of a descriptor chain.
    ///
    /// The virtqueue should only be accessed through [`DescriptorGuard::vring_state()`].  This
    /// ensures that the [`UsedVringWatchdogLeash`] object will only be dropped when all
    /// descriptors have been processed and returned via the used vring, arming the watchdog.
    ///
    /// Prefer to use [`guard_descriptor_by_ref()`](Self::guard_descriptor_by_ref()) if possible,
    /// so that no refcounting needs to be done at runtime.
    pub fn guard_descriptor_owned(self: Arc<Self>) -> DescriptorGuard<'static, 'a> {
        // We are doing work, we will be arming the watchdog when dropped, so we can disarm it for
        // now.  We only need to do this once, so do it before processing the first descriptor.
        if !self.processed_any.swap(true, Ordering::Relaxed) {
            self.watchdog.disarm_thread(self.vring_index);
        }

        DescriptorGuard::Arc(self)
    }
}

impl<'a> UsedVringWatchdogLeash<'a, &'a UsedVringWatchdog> {
    /// Guard processing of a descriptor chain.
    ///
    /// The virtqueue should only be accessed through [`DescriptorGuard::vring_state()`].  This
    /// ensures that the [`UsedVringWatchdogLeash`] object will only be dropped when all
    /// descriptors have been processed and returned via the used vring, arming the watchdog.
    pub fn guard_descriptor_by_ref<'b>(&'b mut self) -> DescriptorGuard<'b, 'a> {
        // We are doing work, we will be arming the watchdog when dropped, so we can disarm it for
        // now.  We only need to do this once, so do it before processing the first descriptor.
        if !mem::replace(self.processed_any.get_mut(), true) {
            self.watchdog.disarm_thread(self.vring_index);
        }

        DescriptorGuard::Ref(self)
    }

    /// Gain access to the `VringState`.
    pub fn vring_state(&mut self) -> &mut VringState<LoggedMemoryAtomic> {
        self.vring_ref.as_mut().unwrap()
    }
}

impl<W: Deref<Target = UsedVringWatchdog>> Drop for UsedVringWatchdogLeash<'_, W> {
    /// Arm the watchdog.
    fn drop(&mut self) {
        if !*self.processed_any.get_mut() {
            // No work done, no need to arm the watchdog.
            return;
        }

        let mem = self.mem.take().unwrap();
        let vring_mutex = self.vring_mutex.take().unwrap();

        // Get the current used_event value (based upon which the watchdog will check whether the
        // guest made progress)
        let used_event = {
            let vring_guard = if let Some(vring_ref) = self.vring_ref.as_mut() {
                VringStateGuard::Ref(vring_ref)
            } else {
                VringStateGuard::Mutex(vring_mutex.get_mut())
            };
            if let Some(idx) = get_used_event(&*mem.memory(), vring_guard.get_queue()) {
                idx
            } else {
                error!("Failed to get used_event from vring {}", self.vring_index);
                return;
            }
        };

        let condvars = self.watchdog.condvars.lock().unwrap();
        let (mutex, cvar) = condvars.get(&self.vring_index).unwrap().as_ref();
        // Arm the watchdog: Tell it what to look for...
        *mutex.lock().unwrap() = Some(WatchdogAwait {
            mem,
            vring: vring_mutex,
            used_event,
        });
        // ...and notify it.
        cvar.notify_one();
    }
}

impl Deref for VringStateGuard<'_> {
    type Target = VringState<LoggedMemoryAtomic>;

    fn deref(&self) -> &VringState<LoggedMemoryAtomic> {
        match self {
            VringStateGuard::Ref(r) => r,
            VringStateGuard::Mutex(mg) => mg.deref(),
        }
    }
}

impl DerefMut for VringStateGuard<'_> {
    fn deref_mut(&mut self) -> &mut VringState<LoggedMemoryAtomic> {
        match self {
            VringStateGuard::Ref(r) => r,
            VringStateGuard::Mutex(mg) => mg.deref_mut(),
        }
    }
}

impl VringDeref for &mut VringState<LoggedMemoryAtomic> {
    fn vring_state(&mut self) -> VringStateGuard<'_> {
        VringStateGuard::Ref(self)
    }
}

impl VringDeref for VringMutex<LoggedMemoryAtomic> {
    fn vring_state(&mut self) -> VringStateGuard<'_> {
        VringStateGuard::Mutex(self.get_mut())
    }
}

impl VringDeref for DescriptorGuard<'_, '_> {
    fn vring_state(&mut self) -> VringStateGuard<'_> {
        match self {
            DescriptorGuard::Ref(inner) => VringStateGuard::Ref(inner.vring_state()),
            DescriptorGuard::Arc(inner) => VringStateGuard::Mutex(inner.lock_vring_state()),
        }
    }
}

/// Get a virtqueue’s `used_event` value (if possible).
///
/// Ideally, there’d be a function for this in virtio-queue, but for the time being, replicating it
/// here is not too difficult.
///
/// Code has been taken from virtio-queue.
fn get_used_event<M: GuestMemory, Q: QueueT>(mem: &M, queue: &Q) -> Option<u16> {
    const VIRTQ_AVAIL_RING_HEADER_SIZE: u64 = 4;
    const VIRTQ_AVAIL_ELEMENT_SIZE: u64 = 2;

    let used_event_offset =
        VIRTQ_AVAIL_RING_HEADER_SIZE + u64::from(queue.size()) * VIRTQ_AVAIL_ELEMENT_SIZE;

    let used_event_addr = queue.avail_ring().checked_add(used_event_offset)?;

    mem.load(GuestAddress(used_event_addr), Ordering::Relaxed)
        .map(u16::from_le)
        .ok()
}
