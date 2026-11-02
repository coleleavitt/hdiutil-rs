//! Loop device management through /dev/loop-control and LOOP_CONFIGURE.

use crate::error::{IoContext, Result};
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::PathBuf;

const LOOP_CTL_GET_FREE: libc::c_ulong = 0x4C82;
const LOOP_CONFIGURE: libc::c_ulong = 0x4C0A;
const LOOP_CLR_FD: libc::c_ulong = 0x4C01;
const LO_FLAGS_READ_ONLY: u32 = 1;
const LO_FLAGS_AUTOCLEAR: u32 = 4;

#[repr(C)]
struct LoopInfo64 {
    device: u64,
    inode: u64,
    rdevice: u64,
    offset: u64,
    sizelimit: u64,
    number: u32,
    encrypt_type: u32,
    encrypt_key_size: u32,
    flags: u32,
    file_name: [u8; 64],
    crypt_name: [u8; 64],
    encrypt_key: [u8; 32],
    init: [u64; 2],
}

#[repr(C)]
struct LoopConfig {
    fd: u32,
    block_size: u32,
    info: LoopInfo64,
    reserved: [u64; 8],
}

/// Attach `backing` (from `offset`, at most `sizelimit` bytes; 0 = rest) to a free loop device.
/// The device is set to auto-clear: it goes away when the last user (the mount) lets go.
/// The returned `File` must be kept open until the device is mounted.
pub fn attach(backing: &File, offset: u64, sizelimit: u64, read_only: bool) -> Result<(PathBuf, File)> {
    let ctl = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/loop-control")
        .ctx(|| "open /dev/loop-control (are you root? is the loop module loaded?)".into())?;
    let n = unsafe { libc::ioctl(ctl.as_raw_fd(), LOOP_CTL_GET_FREE) };
    if n < 0 {
        return Err(std::io::Error::last_os_error()).ctx(|| "LOOP_CTL_GET_FREE".into());
    }
    let path = PathBuf::from(format!("/dev/loop{n}"));
    let dev = OpenOptions::new()
        .read(true)
        .write(!read_only)
        .open(&path)
        .ctx(|| format!("open {}", path.display()))?;
    // All-zero is a valid value for these plain-integer C structs.
    let mut cfg: LoopConfig = unsafe { std::mem::zeroed() };
    cfg.fd = backing.as_raw_fd() as u32;
    cfg.info.offset = offset;
    cfg.info.sizelimit = sizelimit;
    cfg.info.flags = LO_FLAGS_AUTOCLEAR | if read_only { LO_FLAGS_READ_ONLY } else { 0 };
    let rc = unsafe { libc::ioctl(dev.as_raw_fd(), LOOP_CONFIGURE, &cfg) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error()).ctx(|| format!("LOOP_CONFIGURE {}", path.display()));
    }
    Ok((path, dev))
}

/// Release a loop device (deferred until its last user closes it).
pub fn clear(dev: &str) -> Result<()> {
    let f = OpenOptions::new().read(true).open(dev).ctx(|| format!("open {dev}"))?;
    let rc = unsafe { libc::ioctl(f.as_raw_fd(), LOOP_CLR_FD) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error()).ctx(|| format!("LOOP_CLR_FD {dev}"));
    }
    Ok(())
}
