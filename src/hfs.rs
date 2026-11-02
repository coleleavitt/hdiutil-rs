//! Locate the HFS+ volume inside a raw disk image (bare volume, GPT or Apple partition map).

use std::io::{Read, Seek, SeekFrom};

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Region {
    pub offset: u64,
    pub length: u64,
}

/// A view of a file starting at `base`, so partition tables can be read at relative offsets.
pub struct Window<'a> {
    pub file: &'a std::fs::File,
    pub base: u64,
}

impl Read for Window<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(buf)
    }
}

impl Seek for Window<'_> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let r = match pos {
            SeekFrom::Start(n) => (&*self.file).seek(SeekFrom::Start(self.base + n))?,
            other => (&*self.file).seek(other)?,
        };
        Ok(r.saturating_sub(self.base))
    }
}

fn read_at<R: Read + Seek>(r: &mut R, off: u64, n: usize) -> Option<Vec<u8>> {
    let mut b = vec![0u8; n];
    r.seek(SeekFrom::Start(off)).ok()?;
    r.read_exact(&mut b).ok()?;
    Some(b)
}

/// HFS+ ("H+") and HFSX ("HX") volume headers sit 1024 bytes into the volume.
fn has_hfsplus<R: Read + Seek>(r: &mut R, off: u64) -> bool {
    matches!(read_at(r, off + 1024, 2).as_deref(), Some(b"H+") | Some(b"HX"))
}

/// Returns the first HFS+ volume found. `total` is the length of the disk image in bytes.
pub fn find_volume<R: Read + Seek>(r: &mut R, total: u64) -> Option<Region> {
    if has_hfsplus(r, 0) {
        return Some(Region { offset: 0, length: total });
    }
    find_gpt(r).or_else(|| find_apm(r)).and_then(|regions| {
        regions.into_iter().find(|reg| has_hfsplus(r, reg.offset))
    })
}

fn find_gpt<R: Read + Seek>(r: &mut R) -> Option<Vec<Region>> {
    let h = read_at(r, 512, 92)?;
    if &h[0..8] != b"EFI PART" {
        return None;
    }
    let entries_lba = u64::from_le_bytes(h[72..80].try_into().ok()?);
    let count = u32::from_le_bytes(h[80..84].try_into().ok()?) as usize;
    let size = u32::from_le_bytes(h[84..88].try_into().ok()?) as usize;
    if size < 48 || count > 4096 {
        return None;
    }
    let table = read_at(r, entries_lba * 512, count * size)?;
    Some(
        table
            .chunks_exact(size)
            .filter(|e| e[0..16].iter().any(|&b| b != 0))
            .map(|e| {
                let first = u64::from_le_bytes(e[32..40].try_into().unwrap());
                let last = u64::from_le_bytes(e[40..48].try_into().unwrap());
                Region { offset: first * 512, length: (last + 1 - first) * 512 }
            })
            .collect(),
    )
}

fn find_apm<R: Read + Seek>(r: &mut R) -> Option<Vec<Region>> {
    let ddm = read_at(r, 0, 4)?;
    if &ddm[0..2] != b"ER" {
        return None;
    }
    let block = u16::from_be_bytes([ddm[2], ddm[3]]) as u64;
    if block == 0 {
        return None;
    }
    let first = read_at(r, block, 12)?;
    if &first[0..2] != b"PM" {
        return None;
    }
    let n = u32::from_be_bytes(first[4..8].try_into().ok()?) as u64;
    let mut out = Vec::new();
    for i in 1..=n.min(256) {
        let e = read_at(r, i * block, 12)?;
        if &e[0..2] != b"PM" {
            break;
        }
        let start = u32::from_be_bytes(e[8..12].try_into().ok()?) as u64;
        let len = u32::from_be_bytes(read_at(r, i * block + 12, 4)?.try_into().ok()?) as u64;
        out.push(Region { offset: start * block, length: len * block });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn bare_volume() {
        let mut img = vec![0u8; 4096];
        img[1024..1026].copy_from_slice(b"H+");
        let got = find_volume(&mut Cursor::new(img), 4096);
        assert_eq!(got, Some(Region { offset: 0, length: 4096 }));
    }

    #[test]
    fn apm_partition() {
        let mut img = vec![0u8; 512 * 64];
        img[0..4].copy_from_slice(&[b'E', b'R', 2, 0]);
        let mut pm = |i: usize, start: u32, len: u32| {
            let e = &mut img[i * 512..];
            e[0..2].copy_from_slice(b"PM");
            e[4..8].copy_from_slice(&2u32.to_be_bytes());
            e[8..12].copy_from_slice(&start.to_be_bytes());
            e[12..16].copy_from_slice(&len.to_be_bytes());
        };
        pm(1, 1, 1);
        pm(2, 8, 32);
        img[8 * 512 + 1024..8 * 512 + 1026].copy_from_slice(b"HX");
        let got = find_volume(&mut Cursor::new(img), 512 * 64);
        assert_eq!(got, Some(Region { offset: 8 * 512, length: 32 * 512 }));
    }
}
