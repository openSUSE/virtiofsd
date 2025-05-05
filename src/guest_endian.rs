// Copyright 2025 IBM. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Utilities for representing values in guest endianness
use std::fmt::Debug;
use std::sync::OnceLock;

use crate::soft_idmap::{GuestGid, GuestUid, Id};

/// Is there a difference between the guest's endianness and ours?
pub(crate) static SWAP_GUEST_BYTES: OnceLock<bool> = OnceLock::new();

/// An integer from the guest, in its native endianness. Use `.from_guest()`
/// or `.into()` to use the same value in host (our) endianness.
#[repr(transparent)]
#[derive(Default, Copy, Clone)]
pub struct GuestEndian<T>(T);

macro_rules! mk_guest_endian {
    ($base_type:ident) => {
        impl GuestEndian<$base_type> {
            pub fn from_guest(&self) -> $base_type {
                if let Some(true) = SWAP_GUEST_BYTES.get() {
                    self.0.swap_bytes()
                } else {
                    self.0
                }
            }

            pub fn new(val: $base_type) -> Self {
                Self(if let Some(true) = SWAP_GUEST_BYTES.get() {
                    val.swap_bytes()
                } else {
                    val
                })
            }
        }
        impl From<$base_type> for GuestEndian<$base_type> {
            fn from(val: $base_type) -> GuestEndian<$base_type> {
                GuestEndian::<$base_type>::new(val)
            }
        }
        impl From<GuestEndian<$base_type>> for $base_type {
            fn from(val: GuestEndian<$base_type>) -> $base_type {
                val.from_guest()
            }
        }
        impl Debug for GuestEndian<$base_type> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
                self.from_guest().fmt(f)
            }
        }
    };
}
mk_guest_endian!(u16);
mk_guest_endian!(i16);
mk_guest_endian!(u32);
mk_guest_endian!(i32);
mk_guest_endian!(u64);
mk_guest_endian!(i64);

macro_rules! mk_guest_endian_id {
    ($base_type:ident) => {
        impl GuestEndian<$base_type> {
            pub fn from_guest(&self) -> $base_type {
                if let Some(true) = SWAP_GUEST_BYTES.get() {
                    self.0.into_inner().swap_bytes().into()
                } else {
                    self.0
                }
            }

            pub fn new(val: $base_type) -> Self {
                Self(if let Some(true) = SWAP_GUEST_BYTES.get() {
                    val.into_inner().swap_bytes().into()
                } else {
                    val
                })
            }
        }
        impl From<$base_type> for GuestEndian<$base_type> {
            fn from(val: $base_type) -> GuestEndian<$base_type> {
                GuestEndian::<$base_type>::new(val)
            }
        }
        impl From<GuestEndian<$base_type>> for $base_type {
            fn from(val: GuestEndian<$base_type>) -> $base_type {
                val.from_guest()
            }
        }
        impl Debug for GuestEndian<$base_type> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
                self.from_guest().fmt(f)
            }
        }
    };
}

mk_guest_endian_id!(GuestUid);
mk_guest_endian_id!(GuestGid);

#[macro_export]
macro_rules! guest_endian_wrapper {
    (i16) => {$crate::guest_endian::GuestEndian<i16>};
    (u16) => {$crate::guest_endian::GuestEndian<u16>};
    (i32) => {$crate::guest_endian::GuestEndian<i32>};
    (u32) => {$crate::guest_endian::GuestEndian<u32>};
    (i64) => {$crate::guest_endian::GuestEndian<i64>};
    (u64) => {$crate::guest_endian::GuestEndian<u64>};
    (GuestUid) => {$crate::guest_endian::GuestEndian<GuestUid>};
    (GuestGid) => {$crate::guest_endian::GuestEndian<GuestGid>};
    ($tp:ty) => {$tp};
}

#[macro_export]
macro_rules! guest_endian_struct {
    (
        $(#[$outer:meta])*
        $vis:vis struct $name:ident {
        $(
            $field_vis:vis $field_name:ident : $field_typ:tt,
        )*
    }) => {
        $(#[$outer])*
        $vis struct $name {
            $(
                $field_vis $field_name: $crate::guest_endian_wrapper!($field_typ),
            )*
        }
    }
}
