# hdiutil-rs

Minimal Apple `hdiutil` implementation for Linux, written in Rust.

Built by reverse-engineering Apple's `DiskImages.framework` (decompiled with [Kuna](https://github.com/Noelo-Lab/kuna)) to understand the UDIF (DMG) format.

## Features

- **create** - Make HFS+ DMG images (`hdiutil create -size 64m -fs HFS+ -volname X out.dmg`)
- **attach** - Mount DMGs via loop device (`hdiutil attach image.dmg -mountpoint /mnt`)
- **detach** - Unmount cleanly (`hdiutil detach /mnt`)
- **convert** - Expand UDIF to raw image (`hdiutil convert image.dmg -o image.raw`)
- **info** - Show DMG chunk table

## UDIF Compression Support

- Raw (uncompressed)
- ADC (Apple Data Compression)
- zlib
- bzip2
- LZFSE
- LZMA

## Usage

```bash
# Create a 64MB HFS+ image
hdiutil create myimage.dmg -size 64m -fs HFS+ -volname MyVolume

# Mount it (requires root for loop device)
sudo hdiutil attach myimage.dmg -mountpoint /mnt/myimage

# Use it...
sudo cp files/* /mnt/myimage/

# Unmount
sudo hdiutil detach /mnt/myimage

# Convert Apple DMG to raw (for dd, etc.)
hdiutil convert apple.dmg -o apple.raw
```

## Building

```bash
cargo build --release
# Binary at target/release/hdiutil
```

## Dependencies

- Linux kernel with HFS+ support (`CONFIG_HFSPLUS_FS`)
- `mkfs.hfsplus` (hfsprogs package)

## Limitations

- Journaled HFS+ volumes mount read-only (Linux kernel limitation)
- No encrypted image support
- No sparse bundle support
- UDIF checksums not verified

## License

MIT
