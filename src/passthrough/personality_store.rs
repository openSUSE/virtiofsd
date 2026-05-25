// Copyright 2025 Kaisei Hishida.
//
// SPDX-License-Identifier: (Apache-2.0 AND BSD-3-Clause)
use std::collections::BTreeMap;
use std::io;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, RwLock};

use crate::async_wrappers::IoUringState;
use crate::filesystem::{Context, Extensions};
use crate::oslib;
use crate::passthrough::PassthroughFs;
use crate::soft_idmap::{GuestGid, GuestUid};

/// A key that [`PersonalityStore`] internally uses to map personality IDs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PersonalityKey {
    uid: GuestUid,
    gid: GuestGid,
    supplementary_gids: Option<GuestGid>,
    umask: u32,
}

impl PersonalityKey {
    fn new(ctx: &Context, extensions: &Extensions, umask: u32) -> PersonalityKey {
        PersonalityKey {
            uid: ctx.uid,
            gid: ctx.gid,
            supplementary_gids: extensions.sup_gid,
            umask,
        }
    }
}

/// A store for personality IDs used by io_uring.
pub(crate) struct PersonalityStore {
    map: RwLock<BTreeMap<PersonalityKey, u16>>,
}

impl PersonalityStore {
    pub fn new() -> PersonalityStore {
        PersonalityStore {
            map: RwLock::new(BTreeMap::new()),
        }
    }

    /// Get or register a personality ID for the given context, extensions and umask.
    /// If the personality ID is already registered for the given three parameters,
    /// just return the ID.
    /// If not registered, register a new personality ID using io_uring and return it.
    pub fn get_or_register_personality_id(
        &self,
        fs: &PassthroughFs,
        state: &Arc<Mutex<IoUringState>>,
        ctx: &Context,
        extensions: &Extensions,
        umask: u32,
    ) -> io::Result<u16> {
        let key = PersonalityKey::new(ctx, extensions, umask);
        if let Some(&id) = self
            .map
            .read()
            .expect("Lock poisoned: another thread must have panicked")
            .get(&key)
        {
            return Ok(id);
        }

        let id = {
            let state = state
                .lock()
                .expect("Lock poisoned: another thread must have panicked");
            let _credentials_guard = fs.unix_credentials_guard(ctx, extensions)?;
            let _umask_guard = fs
                .posix_acl
                .load(Ordering::Relaxed)
                .then(|| oslib::ScopedUmask::new(umask));

            state.ring.submitter().register_personality()?
        };

        log::debug!("Registered personality with ID {}, Key: {:?}", id, key);

        self.map
            .write()
            .expect("Lock poisoned: another thread must have panicked")
            .insert(key, id);

        Ok(id)
    }
}
