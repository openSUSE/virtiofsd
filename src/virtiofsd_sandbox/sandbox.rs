use std::{
    ffi::CString,
    io::{self, Read, Write},
    process,
};

use virtiofsd::{
    idmap::{self, GidMap, UidMap},
    oslib,
    sandbox::Error,
    util,
};

pub struct Sandbox {
    virtiofsd_path: String,
    socket_path: String,
    shared_dir: String,
    uid_map: Option<UidMap>,
    gid_map: Option<GidMap>,
    virtiofsd_args: Vec<String>,
}

impl Sandbox {
    pub fn new(
        virtiofsd_path: String,
        socket_path: String,
        shared_dir: String,
        uid_map: Option<UidMap>,
        gid_map: Option<GidMap>,
        virtiofsd_args: Vec<String>,
    ) -> Sandbox {
        Sandbox {
            virtiofsd_path,
            socket_path,
            shared_dir,
            uid_map,
            gid_map,
            virtiofsd_args,
        }
    }

    pub fn enter(&self) -> Result<(), Error> {
        // Create pipes for synchronization between parent and child processes.
        let (mut x_reader, mut x_writer) = oslib::pipe().unwrap();
        let (mut y_reader, mut y_writer) = oslib::pipe().unwrap();

        let pid = util::sfork().map_err(Error::Fork)?;
        let mut output = [0];

        if pid == 0 {
            // Child.
            // Sets up ID mappings for the parent process in the new user namespace,
            // allowing it to have arbitrary mappings (with respect to /proc/[pid]/uid_map and /proc/[pid]/gid_map).

            // Drop unneeded ends of pipes.
            drop(x_writer);
            drop(y_reader);

            // Wait until unshare() returns.
            x_reader.read_exact(&mut output).unwrap();
            assert_eq!(output[0], idmap::IdMapSetUpPipeMessage::Request as u8);

            let ppid = unsafe { libc::getppid() };
            virtiofsd::sandbox::Sandbox::setup_id_mappings(
                self.uid_map.as_ref(),
                self.gid_map.as_ref(),
                ppid,
            )?;

            y_writer
                .write_all(&[idmap::IdMapSetUpPipeMessage::Done as u8])
                .unwrap();

            process::exit(0);
        } else {
            // Parent.

            // Drop unneeded ends of pipes.
            drop(x_reader);
            drop(y_writer);

            // Unshare namespaces. This places the current process in a new user and mount namespace.
            // Only child processes are placed in the new PID namespace.
            unsafe { libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS | libc::CLONE_NEWPID) };

            // Signal that unshare() has returned.
            x_writer
                .write_all(&[idmap::IdMapSetUpPipeMessage::Request as u8])
                .unwrap();

            // Wait until ID mappings have been set by child.
            y_reader.read_exact(&mut output).unwrap();
            assert_eq!(output[0], idmap::IdMapSetUpPipeMessage::Done as u8);

            // Set the process inside the user namespace as root.
            let mut ret = unsafe { libc::setresuid(0, 0, 0) };
            if ret != 0 {
                log::warn!("Couldn't set the process uid as root: {}", ret);
            }
            ret = unsafe { libc::setresgid(0, 0, 0) };
            if ret != 0 {
                log::warn!("Couldn't set the process gid as root: {}", ret);
            }

            // Fork to create a child process in the new PID namespace.
            let pid = unsafe { libc::fork() };
            if pid == 0 {
                // Child.

                todo!()
            } else {
                // Parent.

                let mut status = 0;
                // Never returns.
                unsafe { libc::waitpid(pid, &mut status, 0) };
            }
        }

        Ok(())
    }
}
