// Copyright 2025 IBM. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Utilities for representing values in guest endianness
use std::fmt::Debug;
use std::sync::OnceLock;

use crate::soft_idmap::{GuestGid, GuestUid, Id};

#[derive(PartialEq)]
pub enum ByteOrder {
    Little,
    Big,
}

pub(crate) static GUEST_ENDIAN: OnceLock<ByteOrder> = OnceLock::new();

/// An integer from the guest, in its native endianness. Use `.from_guest()`
/// or `.into()` to use the same value in host (our) endianness.
#[repr(transparent)]
#[derive(Default, Copy, Clone)]
pub struct GuestEndian<T>(T);

macro_rules! mk_guest_endian {
    ($base_type:ident) => {
        impl GuestEndian<$base_type> {
            pub fn from_guest(&self) -> $base_type {
                match GUEST_ENDIAN.get() {
                    Some(ByteOrder::Big) => $base_type::from_be(self.0),
                    Some(ByteOrder::Little) => $base_type::from_le(self.0),
                    None => self.0,
                }
            }

            pub fn new(val: $base_type) -> Self {
                Self(match GUEST_ENDIAN.get() {
                    Some(ByteOrder::Big) => val.to_be(),
                    Some(ByteOrder::Little) => val.to_le(),
                    None => val,
                })
            }

            pub fn to_bytes(&self) -> [u8; std::mem::size_of::<$base_type>()] {
                // Note that since we read this value in from the guest endianness,
                // when we 'convert' it to native endianness, that will be in guest
                // endianness. to_ne_bytes could also be read as "as it was in memory"
                self.0.to_ne_bytes()
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
                let inner = self.0.into_inner();
                match GUEST_ENDIAN.get() {
                    Some(ByteOrder::Big) => u32::from_be(inner),
                    Some(ByteOrder::Little) => u32::from_le(inner),
                    None => inner,
                }
                .into()
            }

            pub fn new(val: $base_type) -> Self {
                let inner = val.into_inner();
                Self(
                    match GUEST_ENDIAN.get() {
                        Some(ByteOrder::Big) => inner.to_be(),
                        Some(ByteOrder::Little) => inner.to_le(),
                        None => inner,
                    }
                    .into(),
                )
            }

            pub fn to_bytes(&self) -> [u8; std::mem::size_of::<$base_type>()] {
                // Note that since we read this value in from the guest endianness,
                // when we 'convert' it to native endianness, that will be in guest
                // endianness. to_ne_bytes could also be read as "as it was in memory"
                self.0.into_inner().to_ne_bytes()
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
