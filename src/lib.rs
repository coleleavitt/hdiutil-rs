//! Minimal Linux `hdiutil`: attach / detach / create for HFS+ disk images.

pub mod error;
pub mod hfs;
pub mod loopdev;
pub mod udif;

use error::{format_err, Error, IoContext, Result};
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Default)]
pub struct AttachOptions {
    pub readonly: bool,
}

#[derive(Debug)]
pub struct Attached {
    pub loop_device: PathBuf,
    pub mountpoint: PathBuf,
}

/// `hdiutil attach -mountpoint <mountpoint> <image>`.
///
/// Raw images and uncompressed UDIFs are loop-mounted in place (writes reach the file).
/// Compressed UDIFs are expanded into an unlinked scratch file; changes are discarded on detach.
pub fn attach(image: &Path, mountpoint: &Path, opts: &AttachOptions) -> Result<Attached> {
    let (backing, base, total, in_place) = open_backing(image, opts.readonly)?;
    let region = hfs::find_volume(&mut hfs::Window { file: &backing, base }, total)
        .ok_or_else(|| format_err("no HFS+ volume found in image"))?;
    let read_only = opts.readonly || (in_place && !is_writable(image));

    let (dev, _hold) = loopdev::attach(&backing, base + region.offset, region.length, read_only)?;
    drop(backing);
    std::fs::create_dir_all(mountpoint).ctx(|| format!("create {}", mountpoint.display()))?;
    mount_hfsplus(&dev, mountpoint, read_only)?;
    Ok(Attached { loop_device: dev, mountpoint: mountpoint.to_path_buf() })
}

/// Returns (file, offset of disk data in file, disk length, whether writes reach `image`).
fn open_backing(image: &Path, readonly: bool) -> Result<(File, u64, u64, bool)> {
    if udif::is_udif(image)? {
        let mut u = udif::Udif::open(image)?;
        if let Some(off) = u.contiguous_raw() {
            let f = open_image(image, readonly)?;
            return Ok((f, off, u.size_bytes(), true));
        }
        let dir = scratch_dir();
        std::fs::create_dir_all(&dir).ctx(|| format!("create {}", dir.display()))?;
        let tmp = tempfile::tempfile_in(&dir).ctx(|| format!("scratch file in {}", dir.display()))?;
        let mut w = BufWriter::with_capacity(1 << 20, &tmp);
        u.expand_to(&mut w)?;
        w.flush().ctx(|| "flush scratch file".into())?;
        drop(w);
        return Ok((tmp, 0, u.size_bytes(), false));
    }
    let f = open_image(image, readonly)?;
    let len = f.metadata().ctx(|| "stat image".into())?.len();
    Ok((f, 0, len, true))
}

fn open_image(image: &Path, readonly: bool) -> Result<File> {
    let writable = !readonly && is_writable(image);
    OpenOptions::new()
        .read(true)
        .write(writable)
        .open(image)
        .ctx(|| format!("open {}", image.display()))
}

fn is_writable(p: &Path) -> bool {
    CString::new(p.as_os_str().as_encoded_bytes()).is_ok_and(|c| unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 })
}

/// Scratch space for expanded images: `$HDIUTIL_TMPDIR`, else `/var/tmp/hdiutil-rs`.
fn scratch_dir() -> PathBuf {
    std::env::var_os("HDIUTIL_TMPDIR").map(PathBuf::from).unwrap_or_else(|| "/var/tmp/hdiutil-rs".into())
}

fn mount_hfsplus(dev: &Path, target: &Path, read_only: bool) -> Result<()> {
    let c = |p: &Path| CString::new(p.as_os_str().as_encoded_bytes()).map_err(|_| Error::Usage("path contains NUL".into()));
    let (src, dst) = (c(dev)?, c(target)?);
    let fstype = CString::new("hfsplus").unwrap();
    let flags = if read_only { libc::MS_RDONLY } else { 0 };
    let rc = unsafe { libc::mount(src.as_ptr(), dst.as_ptr(), fstype.as_ptr(), flags, std::ptr::null()) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        let _ = loopdev::clear(&dev.to_string_lossy());
        return Err(err).ctx(|| format!("mount -t hfsplus {} {}", dev.display(), target.display()));
    }
    Ok(())
}

/// `hdiutil detach <mountpoint>`.
pub fn detach(mountpoint: &Path, force: bool) -> Result<()> {
    let target = mountpoint.canonicalize().ctx(|| format!("resolve {}", mountpoint.display()))?;
    let source = mount_source(&target)?.ok_or_else(|| Error::Usage(format!("{} is not a mount point", target.display())))?;
    let c = CString::new(target.as_os_str().as_encoded_bytes()).map_err(|_| Error::Usage("path contains NUL".into()))?;
    let rc = unsafe { libc::umount2(c.as_ptr(), if force { libc::MNT_FORCE } else { 0 }) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).ctx(|| format!("umount {}", target.display()));
    }
    // Loop devices are auto-clear, so this only matters if something else still held the device.
    if source.starts_with("/dev/loop") && Path::new(&format!("/sys/block/{}/loop", &source[5..])).exists() {
        let _ = loopdev::clear(&source);
    }
    Ok(())
}

fn mount_source(target: &Path) -> Result<Option<String>> {
    let info = std::fs::read_to_string("/proc/self/mountinfo").ctx(|| "read /proc/self/mountinfo".into())?;
    let want = target.to_string_lossy();
    // Later entries shadow earlier ones.
    Ok(info.lines().rev().find_map(|l| {
        let (pre, post) = l.split_once(" - ")?;
        if unescape_mountinfo(pre.split(' ').nth(4)?) != want {
            return None;
        }
        post.split(' ').nth(1).map(str::to_owned)
    }))
}

fn unescape_mountinfo(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(u8::is_ascii_digit) {
            out.push(u8::from_str_radix(&s[i + 1..i + 4], 8).unwrap_or(b'?'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fs {
    HfsPlus,
    Hfsx,
}

impl std::str::FromStr for Fs {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "hfs+" | "hfsj" => Ok(Self::HfsPlus),
            "hfsx" | "case-sensitive hfs+" | "jhfs+" => Ok(Self::Hfsx),
            other => Err(Error::Usage(format!("unsupported filesystem {other:?} (use HFS+ or HFSX)"))),
        }
    }
}

/// Parse an hdiutil size: a number with optional suffix b,k,m,g,t (bare number = 512-byte sectors).
pub fn parse_size(s: &str) -> Result<u64> {
    let s = s.trim().to_ascii_lowercase();
    let (digits, mult) = match s.chars().last() {
        Some(c) if c.is_ascii_alphabetic() => {
            let m: u64 = match c {
                'b' => 512,
                'k' => 1 << 10,
                'm' => 1 << 20,
                'g' => 1 << 30,
                't' => 1 << 40,
                _ => return Err(Error::Usage(format!("bad size suffix in {s:?}"))),
            };
            (&s[..s.len() - 1], m)
        }
        _ => (&s[..], 512),
    };
    let n: u64 = digits.parse().map_err(|_| Error::Usage(format!("bad size {s:?}")))?;
    let bytes = n.checked_mul(mult).ok_or_else(|| Error::Usage("size overflow".into()))?;
    Ok(bytes.div_ceil(512) * 512)
}

#[derive(Debug)]
pub struct CreateOptions {
    pub size: u64,
    pub fs: Fs,
    pub volname: String,
    pub overwrite: bool,
}

/// `hdiutil create -size <size> -fs HFS+ <path>`: a sparse file formatted with `mkfs.hfsplus`
/// and sealed with an uncompressed UDIF trailer (so it also opens on macOS).
/// Appends `.dmg` when the path has no extension.
pub fn create(path: &Path, opts: &CreateOptions) -> Result<PathBuf> {
    let path = if path.extension().is_none() { path.with_extension("dmg") } else { path.to_path_buf() };
    if opts.size < 1 << 20 {
        return Err(Error::Usage("size must be at least 1 MiB for HFS+".into()));
    }
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .create_new(!opts.overwrite)
        .truncate(true)
        .open(&path)
        .ctx(|| format!("create {}", path.display()))?;
    let result = (|| {
        f.set_len(opts.size).ctx(|| "allocate image".into())?;
        let mut cmd = Command::new("mkfs.hfsplus");
        cmd.arg("-v").arg(&opts.volname);
        if opts.fs == Fs::Hfsx {
            cmd.arg("-s");
        }
        let out = cmd.arg(&path).output().ctx(|| "run mkfs.hfsplus (install hfsprogs)".into())?;
        if !out.status.success() {
            return Err(Error::Tool("mkfs.hfsplus", String::from_utf8_lossy(&out.stderr).trim().to_owned()));
        }
        f.seek(SeekFrom::Start(0)).ctx(|| "seek".into())?;
        udif::append_raw_trailer(&mut f, &format!("disk image ({})", opts.volname))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&path);
    }
    result.map(|()| path)
}

/// Expand a UDIF to a raw disk image (`hdiutil convert -format UDRO`-ish, but raw).
pub fn convert_to_raw(image: &Path, out: &Path) -> Result<()> {
    let mut u = udif::Udif::open(image)?;
    let f = File::create(out).ctx(|| format!("create {}", out.display()))?;
    let mut w = BufWriter::with_capacity(1 << 20, f);
    u.expand_to(&mut w)?;
    w.flush().ctx(|| "flush output".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("10m").unwrap(), 10 << 20);
        assert_eq!(parse_size("2G").unwrap(), 2 << 30);
        assert_eq!(parse_size("2048").unwrap(), 2048 * 512);
        assert_eq!(parse_size("1k").unwrap(), 1024);
        assert!(parse_size("x").is_err());
    }

    #[test]
    fn mountinfo_escapes() {
        assert_eq!(unescape_mountinfo("/mnt/a\\040b"), "/mnt/a b");
    }
}
