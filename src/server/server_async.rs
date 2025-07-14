// Copyright 2025 Kaisei Hishida.
//
// SPDX-License-Identifier: (Apache-2.0 AND BSD-3-Clause)
use std::convert::TryFrom as _;
use std::fs::File;
use std::io;
use std::sync::Mutex;

use async_trait::async_trait;
use vhost::vhost_user::Backend;

use crate::async_wrappers::IoUringState;
use crate::descriptor_utils::{Reader, Writer};
use crate::filesystem::filesystem_async::AsyncZeroCopyWriter;
use crate::filesystem::{self, FileSystem};
use crate::fuse::{InHeader, Opcode};
use crate::server::{reply_error, Server, ZcWriter, FUSE_BUFFER_HEADER_SIZE, MAX_BUFFER_SIZE};
use crate::{oslib, Error, Result};

#[async_trait]
impl AsyncZeroCopyWriter for ZcWriter<'_> {
    async fn read_from_file_at(
        &mut self,
        ring: &Mutex<IoUringState>,
        f: &File,
        mut count: usize,
        mut off: u64,
        flags: Option<oslib::ReadvFlags>,
    ) -> io::Result<usize> {
        let start = off;
        while count > 0 {
            let read = self
                .0
                .read_from_file_at_async(ring, f, count, off, flags)
                .await?;
            if read == 0 {
                break;
            }
            off = off
                .checked_add(read as u64)
                .ok_or_else(|| io::Error::other("Read wrap-around"))?;
            count = count
                .checked_sub(read)
                .ok_or_else(|| io::Error::other("Read operation wrote more than requested"))?;
        }

        // Must fit: Cannot be greater than `count` originally
        Ok((off - start) as usize)
    }
}

impl<F: FileSystem + Sync> Server<F>
where
    <F as filesystem::FileSystem>::Inode: Send,
    <F as filesystem::FileSystem>::Handle: Send,
    <F as filesystem::FileSystem>::DirIter: Send,
{
    pub async fn handle_message_async(
        &self,
        mut r: Reader<'_>,
        w: Writer<'_>,
        mut vu_req: Option<Backend>,
    ) -> Result<usize> {
        let in_header: InHeader = r.read_obj().map_err(Error::DecodeMessage)?;

        if in_header.len > (MAX_BUFFER_SIZE + FUSE_BUFFER_HEADER_SIZE) {
            return reply_error(
                io::Error::from_raw_os_error(libc::ENOMEM),
                in_header.unique,
                w,
            );
        }

        if let Ok(opcode) = Opcode::try_from(in_header.opcode) {
            debug!(
                "Received request: opcode={opcode:?} ({}), inode={}, unique={}, pid={}",
                in_header.opcode, in_header.nodeid, in_header.unique, in_header.pid
            );
            match opcode {
                Opcode::Lookup => self.lookup(in_header, r, w),
                Opcode::Forget => self.forget(in_header, r), // No reply.
                Opcode::Getattr => self.getattr(in_header, r, w),
                Opcode::Setattr => self.setattr(in_header, r, w),
                Opcode::Readlink => self.readlink(in_header, w),
                Opcode::Symlink => self.symlink(in_header, r, w),
                Opcode::Mknod => self.mknod(in_header, r, w),
                Opcode::Mkdir => self.mkdir(in_header, r, w),
                Opcode::Unlink => self.unlink(in_header, r, w),
                Opcode::Rmdir => self.rmdir(in_header, r, w),
                Opcode::Rename => self.rename_async(in_header, r, w).await,
                Opcode::Link => self.link(in_header, r, w),
                Opcode::Open => self.open(in_header, r, w),
                Opcode::Read => self.read_async(in_header, r, w).await,
                Opcode::Write => self.write(in_header, r, w),
                Opcode::Statfs => self.statfs(in_header, w),
                Opcode::Release => self.release(in_header, r, w),
                Opcode::Fsync => self.fsync(in_header, r, w),
                Opcode::Setxattr => self.setxattr(in_header, r, w),
                Opcode::Getxattr => self.getxattr(in_header, r, w),
                Opcode::Listxattr => self.listxattr(in_header, r, w),
                Opcode::Removexattr => self.removexattr(in_header, r, w),
                Opcode::Flush => self.flush(in_header, r, w),
                Opcode::Init => self.init(in_header, r, w),
                Opcode::Opendir => self.opendir(in_header, r, w),
                Opcode::Readdir => self.readdir(in_header, r, w),
                Opcode::Releasedir => self.releasedir(in_header, r, w),
                Opcode::Fsyncdir => self.fsyncdir(in_header, r, w),
                Opcode::Getlk => self.getlk(in_header, r, w),
                Opcode::Setlk => self.setlk(in_header, r, w),
                Opcode::Setlkw => self.setlkw(in_header, r, w),
                Opcode::Access => self.access(in_header, r, w),
                Opcode::Create => self.create(in_header, r, w),
                Opcode::Interrupt => Ok(self.interrupt(in_header)),
                Opcode::Bmap => self.bmap(in_header, r, w),
                Opcode::Destroy => Ok(self.destroy()),
                Opcode::Ioctl => self.ioctl(in_header, r, w),
                Opcode::Poll => self.poll(in_header, r, w),
                Opcode::NotifyReply => self.notify_reply(in_header, r, w),
                Opcode::BatchForget => self.batch_forget(in_header, r, w),
                Opcode::Fallocate => self.fallocate(in_header, r, w),
                Opcode::Readdirplus => self.readdirplus(in_header, r, w),
                Opcode::Rename2 => self.rename2_async(in_header, r, w).await,
                Opcode::Lseek => self.lseek(in_header, r, w),
                Opcode::CopyFileRange => self.copyfilerange(in_header, r, w),
                Opcode::SetupMapping => self.setupmapping(in_header, r, w, vu_req.as_mut()),
                Opcode::RemoveMapping => self.removemapping(in_header, r, w, vu_req.as_mut()),
                Opcode::Syncfs => self.syncfs(in_header, w),
                Opcode::TmpFile => self.tmpfile(in_header, r, w),
            }
        } else {
            debug!(
                "Received unknown request: opcode={}, inode={}",
                in_header.opcode, in_header.nodeid
            );
            reply_error(
                io::Error::from_raw_os_error(libc::ENOSYS),
                in_header.unique,
                w,
            )
        }
    }
}
