use crate::oslib::check_retval;
use bitflags::bitflags;
use libc::{
    syscall, SYS_landlock_add_rule, SYS_landlock_create_ruleset, SYS_landlock_restrict_self,
};
use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

#[derive(Debug)]
pub enum Error {
    FailedToCreateRuleset(io::Error),
    FailedToEnforceRuleset(io::Error),
    FailedToOpenFile(io::Error),
    FailedToRestrictPrivileges(io::Error),
    FailedToUpdateRuleset(io::Error),
    UnsupportedByKernel,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use Error::*;
        match self {
            FailedToCreateRuleset(err) => write!(f, "Failed to create ruleset: {err}"),
            FailedToEnforceRuleset(err) => write!(f, "Failed to enforce ruleset: {err}"),
            FailedToOpenFile(err) => write!(f, "Failed to open file: {err}"),
            FailedToRestrictPrivileges(err) => write!(f, "Failed to restrict privileges: {err}"),
            FailedToUpdateRuleset(err) => write!(f, "Failed to update ruleset: {err}"),
            UnsupportedByKernel => write!(f, "Landlock unsupported by kernel"),
        }
    }
}

impl std::error::Error for Error {}

pub const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;

const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

const LANDLOCK_ACCESS_FS_EXECUTE: u32 = 1 << 0;
const LANDLOCK_ACCESS_FS_WRITE_FILE: u32 = 1 << 1;
const LANDLOCK_ACCESS_FS_READ_FILE: u32 = 1 << 2;
const LANDLOCK_ACCESS_FS_READ_DIR: u32 = 1 << 3;
const LANDLOCK_ACCESS_FS_REMOVE_DIR: u32 = 1 << 4;
const LANDLOCK_ACCESS_FS_REMOVE_FILE: u32 = 1 << 5;
const LANDLOCK_ACCESS_FS_MAKE_CHAR: u32 = 1 << 6;
const LANDLOCK_ACCESS_FS_MAKE_DIR: u32 = 1 << 7;
const LANDLOCK_ACCESS_FS_MAKE_REG: u32 = 1 << 8;
const LANDLOCK_ACCESS_FS_MAKE_SOCK: u32 = 1 << 9;
const LANDLOCK_ACCESS_FS_MAKE_FIFO: u32 = 1 << 10;
const LANDLOCK_ACCESS_FS_MAKE_BLOCK: u32 = 1 << 11;
const LANDLOCK_ACCESS_FS_MAKE_SYM: u32 = 1 << 12;
const LANDLOCK_ACCESS_FS_REFER: u32 = 1 << 13;
const LANDLOCK_ACCESS_FS_TRUNCATE: u32 = 1 << 14;

bitflags! {
    pub struct LandlockAccessFs: u32 {
        const EXECUTE = LANDLOCK_ACCESS_FS_EXECUTE;
        const WRITE_FILE = LANDLOCK_ACCESS_FS_WRITE_FILE;
        const READ_FILE = LANDLOCK_ACCESS_FS_READ_FILE;
        const READ_DIR = LANDLOCK_ACCESS_FS_READ_DIR;
        const REMOVE_DIR = LANDLOCK_ACCESS_FS_REMOVE_DIR;
        const REMOVE_FILE = LANDLOCK_ACCESS_FS_REMOVE_FILE;
        const MAKE_CHAR = LANDLOCK_ACCESS_FS_MAKE_CHAR;
        const MAKE_DIR = LANDLOCK_ACCESS_FS_MAKE_DIR;
        const MAKE_REG = LANDLOCK_ACCESS_FS_MAKE_REG;
        const MAKE_SOCK = LANDLOCK_ACCESS_FS_MAKE_SOCK;
        const MAKE_FIFO = LANDLOCK_ACCESS_FS_MAKE_FIFO;
        const MAKE_BLOCK = LANDLOCK_ACCESS_FS_MAKE_BLOCK;
        const MAKE_SYM = LANDLOCK_ACCESS_FS_MAKE_SYM;
        const REFER = LANDLOCK_ACCESS_FS_REFER;
        const TRUNCATE = LANDLOCK_ACCESS_FS_TRUNCATE;
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
struct RulesetAttr {
    pub handled_access_fs: u64,
}

/// This struct must be packed according to `include/uapi/linux/landlock.h` in the Linux kernel.
#[repr(C, packed)]
#[derive(Debug, Copy, Clone)]
struct PathBeneathAttr {
    pub allowed_access: u64,
    pub parent_fd: i32,
}

/// Safe wrapper for the `landlock_create_ruleset` system call to check the Landlock ABI version.
pub fn abi_version() -> Result<i32, Error> {
    check_retval(unsafe {
        syscall(
            SYS_landlock_create_ruleset,
            std::ptr::null::<RulesetAttr>(),
            0,
            LANDLOCK_CREATE_RULESET_VERSION,
        ) as i32
    })
    .map_err(|_| Error::UnsupportedByKernel)
}

/// Safe wrapper for the `landlock_create_ruleset` system call to create a ruleset.
pub fn create_ruleset(access_rights: LandlockAccessFs, flags: u32) -> Result<OwnedFd, Error> {
    let attr = RulesetAttr {
        handled_access_fs: access_rights.bits() as u64,
    };
    check_retval(unsafe {
        syscall(
            SYS_landlock_create_ruleset,
            std::ptr::addr_of!(attr),
            std::mem::size_of::<RulesetAttr>(),
            flags,
        )
    })
    .map(|ruleset_fd| unsafe { OwnedFd::from_raw_fd(ruleset_fd as RawFd) })
    .map_err(Error::FailedToCreateRuleset)
}

/// Safe wrapper for the `landlock_restrict_self` system call to restrict the current process and its future children.
fn restrict_self(ruleset_fd: OwnedFd) -> Result<(), Error> {
    check_retval(unsafe { syscall(SYS_landlock_restrict_self, ruleset_fd, 0) })
        .map_err(Error::FailedToEnforceRuleset)?;
    Ok(())
}

pub fn restrict_process(ruleset_fd: OwnedFd) -> Result<(), Error> {
    check_retval(unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) })
        .map_err(Error::FailedToRestrictPrivileges)?;
    restrict_self(ruleset_fd)
}

/// Safe wrapper for the `landlock_add_rule` system call to add Landlock rules to a ruleset.
pub fn add_rule(
    file: &str,
    access_rights: LandlockAccessFs,
    ruleset_fd: BorrowedFd,
) -> Result<(), Error> {
    let target = CString::new(file).unwrap();
    let parent_fd =
        check_retval(unsafe { libc::open(target.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) })
            .map_err(Error::FailedToOpenFile)?;
    let path_beneath = PathBeneathAttr {
        allowed_access: access_rights.bits() as u64,
        parent_fd,
    };
    check_retval(unsafe {
        syscall(
            SYS_landlock_add_rule,
            ruleset_fd.as_raw_fd(),
            LANDLOCK_RULE_PATH_BENEATH,
            std::ptr::addr_of!(path_beneath),
            0,
        )
    })
    .map_err(Error::FailedToUpdateRuleset)?;

    Ok(())
}
