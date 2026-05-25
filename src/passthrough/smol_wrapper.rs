// Copyright 2025 Kaisei Hishida.
//
// SPDX-License-Identifier: (Apache-2.0 AND BSD-3-Clause)
pub(crate) struct SmolRwLock<T>(
    #[cfg(feature = "iouring")] pub(crate) smol::lock::RwLock<T>,
    #[cfg(not(feature = "iouring"))] pub(crate) std::sync::RwLock<T>,
);
#[cfg(feature = "iouring")]
type SmolRwLockReadGuard<'a, T> = smol::lock::RwLockReadGuard<'a, T>;
#[cfg(not(feature = "iouring"))]
type SmolRwLockReadGuard<'a, T> = std::sync::RwLockReadGuard<'a, T>;
#[cfg(feature = "iouring")]
type SmolRwLockWriteGuard<'a, T> = smol::lock::RwLockWriteGuard<'a, T>;
#[cfg(not(feature = "iouring"))]
type SmolRwLockWriteGuard<'a, T> = std::sync::RwLockWriteGuard<'a, T>;

impl<T> SmolRwLock<T> {
    pub fn new(data: T) -> Self {
        #[cfg(feature = "iouring")]
        return SmolRwLock(smol::lock::RwLock::new(data));
        #[cfg(not(feature = "iouring"))]
        return SmolRwLock(std::sync::RwLock::new(data));
    }

    pub fn read_blocking(&self) -> SmolRwLockReadGuard<'_, T> {
        #[cfg(feature = "iouring")]
        return self.0.read_blocking();
        #[cfg(not(feature = "iouring"))]
        return self.0.read().unwrap();
    }

    pub fn write_blocking(&self) -> SmolRwLockWriteGuard<'_, T> {
        #[cfg(feature = "iouring")]
        return self.0.write_blocking();
        #[cfg(not(feature = "iouring"))]
        return self.0.write().unwrap();
    }

    #[allow(dead_code)]
    #[cfg(feature = "iouring")]
    pub async fn read(&self) -> SmolRwLockReadGuard<'_, T> {
        return self.0.read().await;
    }

    #[allow(dead_code)]
    #[cfg(feature = "iouring")]
    pub async fn write(&self) -> SmolRwLockWriteGuard<'_, T> {
        return self.0.write().await;
    }
}
