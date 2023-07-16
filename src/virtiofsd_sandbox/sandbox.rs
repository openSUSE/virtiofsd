use std::{
    ffi::CString,
    io::{self, Read, Write},
    path::Path,
    process,
};

use inotify::{EventMask, Inotify, WatchMask};
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

            // Obtain file descriptor for virtiofsd executable.
            let c_virtiofsd_path =
                CString::new(self.virtiofsd_path.clone()).expect("Invalid virtiofsd path");
            let virtiofsd_fd = unsafe { libc::open(c_virtiofsd_path.as_ptr(), libc::O_RDONLY) };
            if virtiofsd_fd == -1 {
                panic!("virtiofsd_fd error: {}", io::Error::last_os_error());
            }

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

                let shared_dir_file_name = self.setup_mounts()?;
                self.spawn_virtiofsd(virtiofsd_fd, &shared_dir_file_name)?;
            } else {
                // Parent.

                let mut status = 0;
                // Never returns.
                unsafe { libc::waitpid(pid, &mut status, 0) };
            }
        }

        Ok(())
    }

    fn spawn_virtiofsd(
        &self,
        //socket_listener_fd: i32,
        virtiofsd_fd: i32,
        shared_dir_file_name: &str,
    ) -> Result<(), Error> {
        // Create a pipe for virtiofsd to tell us when it has started.
        //let (mut reader, writer) = oslib::pipe().unwrap();

        print_contents("/socket-dir");

        let mut inotify = Inotify::init().expect("Failed to initialize inotify");
        inotify
            .watches()
            .add(
                Path::new("socket-dir"),
                WatchMask::MODIFY | WatchMask::CREATE | WatchMask::DELETE,
            )
            .expect("Failed to add inotify watch");

        let pid = util::sfork().map_err(Error::Fork)?;
        if pid == 0 {
            // Child.

            // Drop read end of pipe so we are notified if the pipe is broken, i.e., the parent died.
            //drop(reader);
            drop(inotify);

            // Get file descriptors for /proc/self/fd and /proc/self/mountinfo.
            let c_proc_pid_fd_path = CString::new("/proc/self/fd").unwrap();
            let proc_pid_fd_fd = unsafe { libc::open(c_proc_pid_fd_path.as_ptr(), libc::O_PATH) };
            if proc_pid_fd_fd < 0 {
                return Err(Error::OpenProcSelfFd(io::Error::last_os_error()));
            }
            let cproc_pid_mountinfo_fd_path = CString::new("/proc/self/mountinfo").unwrap();
            let proc_pid_mountinfo_fd =
                unsafe { libc::open(cproc_pid_mountinfo_fd_path.as_ptr(), libc::O_PATH) };
            if proc_pid_mountinfo_fd < 0 {
                return Err(Error::OpenProcSelfFd(io::Error::last_os_error()));
            }

            assert_eq!(unsafe { libc::fcntl(proc_pid_fd_fd, libc::F_GETFD) }, 0);
            assert_eq!(
                unsafe { libc::fcntl(proc_pid_mountinfo_fd, libc::F_GETFD) },
                0
            );

            self.exec_virtiofsd(
                //socket_listener_fd,
                virtiofsd_fd,
                shared_dir_file_name,
                //writer.as_raw_fd(),
                proc_pid_fd_fd,
                proc_pid_mountinfo_fd,
            );
        } else {
            // Parent.

            // Drop write end of pipe so we are notified if the pipe is broken, i.e., the child died.
            //drop(writer);

            // Wait until virtiofsd has started.
            // let mut output = [0];
            // reader.read_exact(&mut output).unwrap();
            // assert_eq!(output[0], IdMapSetUpPipeMessage::Done as u8);

            print_contents("/");

            log::debug!("Listening for events for creating socket");
            let mut buffer = [0u8; 4096];
            'outer: loop {
                let events = inotify
                    .read_events_blocking(&mut buffer)
                    .expect("Failed to read inotify events");

                for event in events {
                    if event.mask.contains(EventMask::CREATE) {
                        if event.mask.contains(EventMask::ISDIR) {
                            println!("Directory created: {:?}", event.name);
                        } else {
                            println!("File created: {:?}", event.name);
                        }
                        if event.name.unwrap().to_string_lossy().to_string() == "vfsd.sock.pid" {
                            break 'outer;
                        }
                    } else if event.mask.contains(EventMask::DELETE) {
                        if event.mask.contains(EventMask::ISDIR) {
                            println!("Directory deleted: {:?}", event.name);
                        } else {
                            println!("File deleted: {:?}", event.name);
                        }
                    } else if event.mask.contains(EventMask::MODIFY) {
                        if event.mask.contains(EventMask::ISDIR) {
                            println!("Directory modified: {:?}", event.name);
                        } else {
                            println!("File modified: {:?}", event.name);
                        }
                        if event.name.unwrap().to_string_lossy().to_string() == "vfsd.sock.pid" {
                            break 'outer;
                        }
                    }
                }
            }

            drop(inotify);

            print_contents("socket-dir");

            log::debug!("virtiofsd started, unmounting dependencies");

            // Unmount and remove /proc now that virtiofsd is running.
            // We keep /lib and /lib64 mounted in case lazy loading in dynamic linker is enabled.
            oslib::umount2("/proc", libc::MNT_DETACH).unwrap();
            std::fs::remove_dir("/proc").unwrap();

            oslib::umount2("/socket-dir", libc::MNT_DETACH).unwrap();
            std::fs::remove_dir("/socket-dir").unwrap();

            // Remount new root as read-only.
            oslib::mount(
                None,
                ".",
                None,
                libc::MS_BIND | libc::MS_REC | libc::MS_REMOUNT | libc::MS_RDONLY,
            )
            .map_err(Error::CleanMount)?;

            //print_contents("/");

            let mut status = 0;
            unsafe { libc::waitpid(pid, &mut status, 0) };
        }

        Ok(())
    }

    fn exec_virtiofsd(
        &self,
        virtiofsd_fd: i32,
        shared_dir_file_name: &str,
        proc_self_fd_fd: i32,
        proc_self_mountinfo_fd: i32,
    ) {
        let proc_self_fd_fd_str = proc_self_fd_fd.to_string();
        let proc_self_mountinfo_fd_str = proc_self_mountinfo_fd.to_string();

        let mut virtiofsd_args = Vec::new();
        virtiofsd_args.push(self.virtiofsd_path.as_str());
        virtiofsd_args.push("--sandbox");
        virtiofsd_args.push("none");
        virtiofsd_args.extend(self.virtiofsd_args.iter().map(|s| s.as_str()));
        virtiofsd_args.push("--shared-dir");
        virtiofsd_args.push(shared_dir_file_name);
        virtiofsd_args.push("--socket-path");
        virtiofsd_args.push("/socket-dir/vfsd.sock");
        virtiofsd_args.push("--proc-self-fd");
        virtiofsd_args.push(&proc_self_fd_fd_str);
        virtiofsd_args.push("--proc-self-mountinfo");
        virtiofsd_args.push(&proc_self_mountinfo_fd_str);

        log::debug!("virtiofsd arguments: {:?}", virtiofsd_args);

        // Transform arguments into CString's.
        let virtiofsd_args: Vec<CString> = virtiofsd_args
            .into_iter()
            .map(|s| CString::new(s).expect(&format!("Invalid argument: {}", s)))
            .collect();

        // Collect pointers of arguments.
        let mut virtiofsd_args: Vec<*const i8> =
            virtiofsd_args.iter().map(|s| s.as_ptr()).collect();
        virtiofsd_args.push(std::ptr::null());

        // Execute virtiofsd.
        let res = unsafe {
            libc::fexecve(
                virtiofsd_fd,
                virtiofsd_args.as_ptr(),
                [std::ptr::null()].as_ptr(),
            )
        };
        unreachable!(
            "fexecve() error (returned {}): {}",
            res,
            io::Error::last_os_error()
        );
    }

    fn setup_mounts(&self) -> Result<String, Error> {
        // Mark the root directory as slave recursively so its mounts propagate to us, but our mounts do not propagate to it.
        oslib::mount(None, "/", None, libc::MS_SLAVE | libc::MS_REC).map_err(Error::CleanMount)?;

        // Create a tmpfs which we will use as the new root filesystem.
        oslib::mount(
            "tmpfs".into(),
            "/tmp",
            "tmpfs".into(),
            libc::MS_NODEV | libc::MS_NOSUID,
        )
        .map_err(Error::MountTmpFs)?;

        // Get absolute shared directory path before we change directory in case the path is relative.
        let shared_dir_abs_path = Path::new(&self.shared_dir).canonicalize().unwrap();

        // Get path of directory containing socket file.
        dbg!(&self.socket_path);
        // Handle cases of:
        // - vfsd.sock
        // - ./vfsd.sock`
        // - some/path/[..]/vfsd.sock
        let parent = Path::new(&self.socket_path)
            .parent()
            .map(|p| {
                if p.to_string_lossy().is_empty() {
                    Path::new(".")
                } else {
                    p
                }
            })
            .unwrap_or(Path::new("."));
        dbg!(&parent);
        let socket_dir_abs_path = parent.canonicalize().unwrap().to_string_lossy().to_string();
        dbg!(&socket_dir_abs_path);

        // Switch our working directory to the new tmpfs.
        std::env::set_current_dir("/tmp").unwrap();

        // Make shared directory.
        let shared_dir_file_name = shared_dir_abs_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        std::fs::create_dir(&shared_dir_file_name).unwrap();
        // Make lib directory.
        std::fs::create_dir("lib").unwrap();
        // Make lib64 directory.
        std::fs::create_dir("lib64").unwrap();
        // Make proc directory.
        std::fs::create_dir("proc").unwrap();
        // Make directory the socket file is to be mounted in.
        std::fs::create_dir("socket-dir").unwrap();

        // Bind mount the shared directory in the tmpfs.
        let shared_dir_abs_path = shared_dir_abs_path.to_string_lossy().to_string();
        oslib::mount(
            shared_dir_abs_path.as_str().into(),
            &shared_dir_file_name,
            None,
            libc::MS_BIND | libc::MS_REC,
        )
        .unwrap();

        // Bind mount /lib.
        oslib::mount("/lib".into(), "lib", None, libc::MS_BIND | libc::MS_REC)
            .map_err(Error::MountLib)?;

        // Bind mount /lib64.
        oslib::mount("/lib64".into(), "lib64", None, libc::MS_BIND | libc::MS_REC)
            .map_err(Error::MountLib64)?;

        // Bind mount /socket-dir.
        oslib::mount(
            socket_dir_abs_path.as_str().into(),
            "socket-dir",
            None,
            libc::MS_BIND | libc::MS_REC,
        )
        .unwrap();

        // Get a file descriptor to our old root so we can reference it after switching root.
        let c_root_dir = CString::new("/").unwrap();
        let oldroot_fd = unsafe {
            libc::open(
                c_root_dir.as_ptr(),
                libc::O_DIRECTORY | libc::O_RDONLY | libc::O_CLOEXEC,
            )
        };
        if oldroot_fd < 0 {
            return Err(Error::OpenOldRoot(std::io::Error::last_os_error()));
        }

        // Get a file descriptor to the new root so we can reference it after switching root.
        let c_current_dir = CString::new(".").unwrap();
        let newroot_fd = unsafe {
            libc::open(
                c_current_dir.as_ptr(),
                libc::O_DIRECTORY | libc::O_RDONLY | libc::O_CLOEXEC,
            )
        };
        if newroot_fd < 0 {
            return Err(Error::OpenNewRoot(std::io::Error::last_os_error()));
        }

        // Our current working directory is `/tmp`.
        // Call to `pivot_root` using `.` as both new and old root.
        let c_current_dir = CString::new(".").unwrap();
        let ret = unsafe {
            libc::syscall(
                libc::SYS_pivot_root,
                c_current_dir.as_ptr(),
                c_current_dir.as_ptr(),
            )
        };
        if ret < 0 {
            return Err(Error::PivotRoot(std::io::Error::last_os_error()));
        }

        // Change to new root.
        oslib::fchdir(newroot_fd).map_err(Error::ChdirNewRoot)?;

        // Mount proc.
        oslib::mount(
            "proc".into(),
            "/proc",
            "proc".into(),
            libc::MS_NODEV | libc::MS_NOEXEC | libc::MS_NOSUID | libc::MS_RELATIME,
        )
        .map_err(Error::MountProc)?;

        // Change to old root directory to prepare for cleaning up and unmounting it.
        oslib::fchdir(oldroot_fd).map_err(Error::ChdirOldRoot)?;

        // Set the mount propagation type of old root to `slave` to avoid our mount events propagate to it.
        oslib::mount(None, ".", None, libc::MS_SLAVE | libc::MS_REC).map_err(Error::CleanMount)?;

        // Lazily unmount old root.
        oslib::umount2(".", libc::MNT_DETACH).map_err(Error::UmountOldRoot)?;

        // Change to new root.
        oslib::fchdir(newroot_fd).map_err(Error::ChdirNewRoot)?;

        // We no longer need these file descriptors, so close them.
        unsafe { libc::close(newroot_fd) };
        unsafe { libc::close(oldroot_fd) };

        Ok(shared_dir_file_name)
    }
}

fn print_contents(path: &str) {
    println!("Contents of {}:", path);
    for path in std::fs::read_dir(path).unwrap() {
        println!("Name: {}", path.unwrap().path().display())
    }
}
