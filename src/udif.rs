//! UDIF (Apple `.dmg`) parsing and expansion.
//!
//! Layout (confirmed against `CUDIFEncoding::headerIsUDIF`, `_DI_logUDIFFileHeader` and
//! `CUDIFDiskImage::isValidBLKX` in DiskImages.framework): a 512-byte big-endian `koly` trailer
//! at the end of the file points at an XML plist. The plist holds `resource-fork/blkx` entries,
//! each a base64 `mish` table of 40-byte chunk descriptors.

use crate::error::{format_err, Error, IoContext, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const SECTOR: u64 = 512;
const KOLY_MAGIC: &[u8; 4] = b"koly";
const KOLY_SIZE: usize = 512;
const MISH_MAGIC: &[u8; 4] = b"mish";
const MISH_HEADER: usize = 0xCC;
const CHUNK_SIZE: usize = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChunkKind {
    Zero,
    Raw,
    Ignore,
    Adc,
    Zlib,
    Bzip2,
    Lzfse,
    Lzma,
}

impl ChunkKind {
    fn from_u32(v: u32) -> Result<Option<Self>> {
        Ok(Some(match v {
            0 => Self::Zero,
            1 => Self::Raw,
            2 => Self::Ignore,
            0x8000_0004 => Self::Adc,
            0x8000_0005 => Self::Zlib,
            0x8000_0006 => Self::Bzip2,
            0x8000_0007 => Self::Lzfse,
            0x8000_0008 => Self::Lzma,
            0x7fff_fffe | 0xffff_ffff => return Ok(None), // comment, terminator
            other => return Err(format_err(format!("unknown chunk type 0x{other:08X}"))),
        }))
    }
}

#[derive(Clone, Debug)]
pub struct Chunk {
    pub kind: ChunkKind,
    pub sector: u64,
    pub sectors: u64,
    /// Absolute offset in the dmg file.
    pub offset: u64,
    pub length: u64,
}

#[derive(Debug)]
pub struct Udif {
    file: File,
    pub data_fork_offset: u64,
    pub data_fork_length: u64,
    pub sector_count: u64,
    pub chunks: Vec<Chunk>,
}

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes(b[o..o + 4].try_into().unwrap())
}
fn be64(b: &[u8], o: usize) -> u64 {
    u64::from_be_bytes(b[o..o + 8].try_into().unwrap())
}

/// True when the file ends in a valid `koly` trailer.
pub fn is_udif(path: &Path) -> Result<bool> {
    let mut f = File::open(path).ctx(|| format!("open {}", path.display()))?;
    Ok(read_koly(&mut f)?.is_some())
}

fn read_koly(f: &mut File) -> Result<Option<[u8; KOLY_SIZE]>> {
    let len = f.metadata().ctx(|| "stat".into())?.len();
    if len < KOLY_SIZE as u64 {
        return Ok(None);
    }
    let mut buf = [0u8; KOLY_SIZE];
    f.seek(SeekFrom::Start(len - KOLY_SIZE as u64)).ctx(|| "seek".into())?;
    f.read_exact(&mut buf).ctx(|| "read koly".into())?;
    // headerIsUDIF: signature 'koly' and version 4.
    Ok((&buf[0..4] == KOLY_MAGIC && be32(&buf, 4) == 4).then_some(buf))
}

impl Udif {
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path).ctx(|| format!("open {}", path.display()))?;
        let koly = read_koly(&mut file)?.ok_or_else(|| format_err("no UDIF koly trailer"))?;
        let data_fork_offset = be64(&koly, 0x18);
        let data_fork_length = be64(&koly, 0x20);
        let xml_off = be64(&koly, 0xd8);
        let xml_len = be64(&koly, 0xe0);
        let sector_count = be64(&koly, 0x1ec);
        if xml_len == 0 {
            return Err(format_err("image has no XML property list"));
        }
        let mut xml = vec![0u8; xml_len as usize];
        file.seek(SeekFrom::Start(xml_off)).ctx(|| "seek xml".into())?;
        file.read_exact(&mut xml).ctx(|| "read xml".into())?;
        let chunks = parse_blkx(&xml, data_fork_offset)?;
        Ok(Self { file, data_fork_offset, data_fork_length, sector_count, chunks })
    }

    pub fn size_bytes(&self) -> u64 {
        self.sector_count * SECTOR
    }

    /// If the whole image is one contiguous run of uncompressed data, return its absolute
    /// file offset, so it can be loop-mounted in place without expansion.
    pub fn contiguous_raw(&self) -> Option<u64> {
        let first = self.chunks.first()?;
        let mut next_sector = 0;
        let mut next_off = first.offset;
        for c in &self.chunks {
            if c.kind != ChunkKind::Raw || c.sector != next_sector || c.offset != next_off {
                return None;
            }
            next_sector += c.sectors;
            next_off += c.sectors * SECTOR;
        }
        (next_sector == self.sector_count).then_some(first.offset)
    }

    /// Decompress every chunk, in order, into `out` (gaps are zero-filled).
    pub fn expand_to(&mut self, out: &mut impl Write) -> Result<()> {
        let mut pos = 0u64; // sectors written
        let mut comp = Vec::new();
        let mut plain = Vec::new();
        let zeros = vec![0u8; 1 << 20];
        for c in self.chunks.clone() {
            if c.sector < pos {
                return Err(format_err("chunks out of order"));
            }
            write_zeros(out, (c.sector - pos) * SECTOR, &zeros)?;
            let want = (c.sectors * SECTOR) as usize;
            match c.kind {
                ChunkKind::Zero | ChunkKind::Ignore => write_zeros(out, want as u64, &zeros)?,
                _ => {
                    comp.resize(c.length as usize, 0);
                    self.file.seek(SeekFrom::Start(c.offset)).ctx(|| "seek chunk".into())?;
                    self.file.read_exact(&mut comp).ctx(|| "read chunk".into())?;
                    decompress(c.kind, &comp, want, &mut plain)?;
                    out.write_all(&plain).ctx(|| "write output".into())?;
                }
            }
            pos = c.sector + c.sectors;
        }
        write_zeros(out, self.sector_count.saturating_sub(pos) * SECTOR, &zeros)
    }
}

fn write_zeros(out: &mut impl Write, mut n: u64, zeros: &[u8]) -> Result<()> {
    while n > 0 {
        let k = n.min(zeros.len() as u64) as usize;
        out.write_all(&zeros[..k]).ctx(|| "write output".into())?;
        n -= k as u64;
    }
    Ok(())
}

fn parse_blkx(xml: &[u8], data_fork_offset: u64) -> Result<Vec<Chunk>> {
    let plist: plist::Value = plist::from_bytes(xml).map_err(|e| format_err(format!("plist: {e}")))?;
    let blkx = plist
        .as_dictionary()
        .and_then(|d| d.get("resource-fork"))
        .and_then(|v| v.as_dictionary())
        .and_then(|d| d.get("blkx"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| format_err("plist has no resource-fork/blkx"))?;
    let mut chunks = Vec::new();
    for entry in blkx {
        let data = entry
            .as_dictionary()
            .and_then(|d| d.get("Data"))
            .and_then(|v| v.as_data())
            .ok_or_else(|| format_err("blkx entry without Data"))?;
        parse_mish(data, data_fork_offset, &mut chunks)?;
    }
    chunks.sort_by_key(|c| c.sector);
    Ok(chunks)
}

fn parse_mish(m: &[u8], data_fork_offset: u64, out: &mut Vec<Chunk>) -> Result<()> {
    if m.len() < MISH_HEADER || &m[0..4] != MISH_MAGIC || be32(m, 4) != 1 {
        return Err(format_err("bad blkx (mish) header"));
    }
    let first_sector = be64(m, 0x08);
    let data_start = be64(m, 0x18);
    let count = be32(m, 0xc8) as usize;
    if m.len() < MISH_HEADER + count * CHUNK_SIZE {
        return Err(format_err("blkx table truncated"));
    }
    for i in 0..count {
        let e = &m[MISH_HEADER + i * CHUNK_SIZE..][..CHUNK_SIZE];
        let Some(kind) = ChunkKind::from_u32(be32(e, 0))? else { continue };
        out.push(Chunk {
            kind,
            sector: first_sector + be64(e, 8),
            sectors: be64(e, 16),
            offset: data_fork_offset + data_start + be64(e, 24),
            length: be64(e, 32),
        });
    }
    Ok(())
}

fn decompress(kind: ChunkKind, src: &[u8], want: usize, out: &mut Vec<u8>) -> Result<()> {
    out.clear();
    out.reserve(want);
    let io = |what: &str, e: std::io::Error| format_err(format!("{what}: {e}"));
    match kind {
        ChunkKind::Raw => out.extend_from_slice(src),
        ChunkKind::Zlib => {
            flate2::read::ZlibDecoder::new(src).read_to_end(out).map_err(|e| io("zlib", e))?;
        }
        ChunkKind::Bzip2 => {
            bzip2::read::BzDecoder::new(src).read_to_end(out).map_err(|e| io("bzip2", e))?;
        }
        ChunkKind::Lzma => {
            lzma_rs::xz_decompress(&mut &src[..], out).map_err(|e| format_err(format!("lzma: {e}")))?;
        }
        ChunkKind::Lzfse => {
            lzfse_rust::LzfseDecoder::default()
                .decode_bytes(src, out)
                .map_err(|e| format_err(format!("lzfse: {e}")))?;
        }
        ChunkKind::Adc => adc_decode(src, out)?,
        ChunkKind::Zero | ChunkKind::Ignore => unreachable!(),
    }
    if out.len() != want {
        return Err(format_err(format!("chunk expanded to {} bytes, expected {want}", out.len())));
    }
    Ok(())
}

/// Apple Data Compression (type 0x80000004): literal runs and back-references.
pub fn adc_decode(src: &[u8], out: &mut Vec<u8>) -> Result<()> {
    let bad = || format_err("truncated or corrupt ADC stream");
    let mut i = 0;
    while i < src.len() {
        let c = src[i];
        i += 1;
        if c & 0x80 != 0 {
            let n = (c & 0x7f) as usize + 1;
            out.extend_from_slice(src.get(i..i + n).ok_or_else(bad)?);
            i += n;
            continue;
        }
        let (len, off) = if c & 0x40 != 0 {
            let b = src.get(i..i + 2).ok_or_else(bad)?;
            i += 2;
            ((c & 0x3f) as usize + 4, u16::from_be_bytes([b[0], b[1]]) as usize + 1)
        } else {
            let b = *src.get(i).ok_or_else(bad)?;
            i += 1;
            (((c & 0x3c) >> 2) as usize + 3, (((c & 3) as usize) << 8 | b as usize) + 1)
        };
        if off > out.len() {
            return Err(bad());
        }
        for _ in 0..len {
            out.push(out[out.len() - off]);
        }
    }
    Ok(())
}

/// Append an uncompressed (UDRW-style) UDIF trailer to a raw image file whose data starts at 0.
pub fn append_raw_trailer(file: &mut File, name: &str) -> Result<()> {
    let len = file.metadata().ctx(|| "stat".into())?.len();
    if len % SECTOR != 0 {
        return Err(Error::Usage("image size must be a multiple of 512".into()));
    }
    let sectors = len / SECTOR;

    let mut mish = vec![0u8; MISH_HEADER + 2 * CHUNK_SIZE];
    mish[0..4].copy_from_slice(MISH_MAGIC);
    mish[4..8].copy_from_slice(&1u32.to_be_bytes());
    mish[0x10..0x18].copy_from_slice(&sectors.to_be_bytes());
    mish[0x20..0x24].copy_from_slice(&2048u32.to_be_bytes());
    mish[0x24..0x28].copy_from_slice(&0xffff_ffffu32.to_be_bytes());
    mish[0xc8..0xcc].copy_from_slice(&2u32.to_be_bytes());
    let mut put = |idx: usize, kind: u32, sector: u64, count: u64, off: u64, clen: u64| {
        let e = &mut mish[MISH_HEADER + idx * CHUNK_SIZE..][..CHUNK_SIZE];
        e[0..4].copy_from_slice(&kind.to_be_bytes());
        e[8..16].copy_from_slice(&sector.to_be_bytes());
        e[16..24].copy_from_slice(&count.to_be_bytes());
        e[24..32].copy_from_slice(&off.to_be_bytes());
        e[32..40].copy_from_slice(&clen.to_be_bytes());
    };
    put(0, 1, 0, sectors, 0, len);
    put(1, 0xffff_ffff, sectors, 0, len, 0);

    let mut entry = plist::Dictionary::new();
    entry.insert("Attributes".into(), "0x0050".into());
    entry.insert("CFName".into(), name.into());
    entry.insert("Data".into(), plist::Value::Data(mish));
    entry.insert("ID".into(), "0".into());
    entry.insert("Name".into(), name.into());
    let mut rf = plist::Dictionary::new();
    rf.insert("blkx".into(), plist::Value::Array(vec![plist::Value::Dictionary(entry)]));
    let mut top = plist::Dictionary::new();
    top.insert("resource-fork".into(), plist::Value::Dictionary(rf));
    let mut xml = Vec::new();
    plist::to_writer_xml(&mut xml, &plist::Value::Dictionary(top)).map_err(|e| format_err(format!("plist: {e}")))?;

    let mut koly = [0u8; KOLY_SIZE];
    koly[0..4].copy_from_slice(KOLY_MAGIC);
    koly[4..8].copy_from_slice(&4u32.to_be_bytes());
    koly[8..12].copy_from_slice(&(KOLY_SIZE as u32).to_be_bytes());
    koly[0x0c..0x10].copy_from_slice(&1u32.to_be_bytes()); // kUDIFFlagsFlattened
    koly[0x20..0x28].copy_from_slice(&len.to_be_bytes());
    koly[0x3c..0x40].copy_from_slice(&1u32.to_be_bytes());
    koly[0xd8..0xe0].copy_from_slice(&len.to_be_bytes());
    koly[0xe0..0xe8].copy_from_slice(&(xml.len() as u64).to_be_bytes());
    koly[0x1e8..0x1ec].copy_from_slice(&1u32.to_be_bytes()); // device image
    koly[0x1ec..0x1f4].copy_from_slice(&sectors.to_be_bytes());

    file.seek(SeekFrom::Start(len)).ctx(|| "seek".into())?;
    file.write_all(&xml).ctx(|| "write xml".into())?;
    file.write_all(&koly).ctx(|| "write koly".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a UDIF in memory: data fork of the given (type, sectors, payload) chunks.
    fn build(chunks: &[(u32, u64, Vec<u8>)]) -> tempfile::NamedTempFile {
        let mut fork = Vec::new();
        let mut mish = vec![0u8; MISH_HEADER + (chunks.len() + 1) * CHUNK_SIZE];
        mish[0..4].copy_from_slice(MISH_MAGIC);
        mish[4..8].copy_from_slice(&1u32.to_be_bytes());
        let total: u64 = chunks.iter().map(|c| c.1).sum();
        mish[0x10..0x18].copy_from_slice(&total.to_be_bytes());
        mish[0xc8..0xcc].copy_from_slice(&((chunks.len() + 1) as u32).to_be_bytes());
        let mut sector = 0;
        let mut entries: Vec<_> = chunks.iter().map(|(k, n, p)| {
            let e = (*k, sector, *n, fork.len() as u64, p.len() as u64);
            fork.extend_from_slice(p);
            sector += n;
            e
        }).collect();
        entries.push((0xffff_ffff, sector, 0, fork.len() as u64, 0));
        for (i, (k, s, n, o, l)) in entries.into_iter().enumerate() {
            let e = &mut mish[MISH_HEADER + i * CHUNK_SIZE..][..CHUNK_SIZE];
            e[0..4].copy_from_slice(&k.to_be_bytes());
            e[8..16].copy_from_slice(&s.to_be_bytes());
            e[16..24].copy_from_slice(&n.to_be_bytes());
            e[24..32].copy_from_slice(&o.to_be_bytes());
            e[32..40].copy_from_slice(&l.to_be_bytes());
        }
        let mut entry = plist::Dictionary::new();
        entry.insert("Data".into(), plist::Value::Data(mish));
        let mut rf = plist::Dictionary::new();
        rf.insert("blkx".into(), plist::Value::Array(vec![plist::Value::Dictionary(entry)]));
        let mut top = plist::Dictionary::new();
        top.insert("resource-fork".into(), plist::Value::Dictionary(rf));
        let mut xml = Vec::new();
        plist::to_writer_xml(&mut xml, &plist::Value::Dictionary(top)).unwrap();
        let mut koly = [0u8; KOLY_SIZE];
        koly[0..4].copy_from_slice(KOLY_MAGIC);
        koly[4..8].copy_from_slice(&4u32.to_be_bytes());
        koly[0x20..0x28].copy_from_slice(&(fork.len() as u64).to_be_bytes());
        koly[0xd8..0xe0].copy_from_slice(&(fork.len() as u64).to_be_bytes());
        koly[0xe0..0xe8].copy_from_slice(&(xml.len() as u64).to_be_bytes());
        koly[0x1ec..0x1f4].copy_from_slice(&total.to_be_bytes());
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(&fork).unwrap();
        f.write_all(&xml).unwrap();
        f.write_all(&koly).unwrap();
        f
    }

    #[test]
    fn expands_every_chunk_type() {
        let pattern: Vec<u8> = (0..1024u32).map(|i| (i * 7 % 251) as u8).collect();
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&pattern).unwrap();
        let zlib = z.finish().unwrap();
        let mut b = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
        b.write_all(&pattern).unwrap();
        let bz = b.finish().unwrap();
        // ADC: 4 literal bytes, then a 3-byte-form match of 8 bytes at distance 4.
        let adc = vec![0x83, 1, 2, 3, 4, 0x40 | (8 - 4), 0, 3];
        let adc_plain = [1u8, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4];
        let mut adc_padded = adc.clone();
        // pad to a full sector with a literal run of zeros (128 + 128 + ...): 500 zero bytes
        for n in [128usize, 128, 128, 116] {
            adc_padded.push(0x80 | (n as u8 - 1));
            adc_padded.extend(std::iter::repeat_n(0u8, n));
        }
        let mut want_adc = adc_plain.to_vec();
        want_adc.resize(512, 0);

        let f = build(&[
            (1, 2, pattern.clone()),
            (0x8000_0005, 2, zlib),
            (0x8000_0006, 2, bz),
            (0, 1, vec![]),
            (0x8000_0004, 1, adc_padded),
        ]);
        let mut u = Udif::open(f.path()).unwrap();
        assert_eq!(u.contiguous_raw(), None);
        let mut out = Vec::new();
        u.expand_to(&mut out).unwrap();
        assert_eq!(out.len(), 8 * 512);
        assert_eq!(&out[..1024], &pattern[..]);
        assert_eq!(&out[1024..2048], &pattern[..]);
        assert_eq!(&out[2048..3072], &pattern[..]);
        assert!(out[3072..3584].iter().all(|&b| b == 0));
        assert_eq!(&out[3584..], &want_adc[..]);
    }

    #[test]
    fn single_raw_chunk_is_contiguous() {
        let f = build(&[(1, 1, vec![7; 512])]);
        let u = Udif::open(f.path()).unwrap();
        assert_eq!(u.contiguous_raw(), Some(0));
    }
}
