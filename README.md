# hdiutil-rs

Minimal Linux `hdiutil` for building SSH ramdisks. Format knowledge comes from DiskImages.framework
(`CUDIFEncoding::headerIsUDIF`, `_DI_logUDIFFileHeader`, `CUDIFDiskImage::isValidBLKX`).

    hdiutil create -size 64m -fs HFS+ -volname ramdisk ramdisk   # -> ramdisk.dmg (needs mkfs.hfsplus)
    sudo hdiutil attach -mountpoint /mnt/rd ramdisk.dmg          # loop device + mount -t hfsplus
    sudo hdiutil detach /mnt/rd
    hdiutil convert in.dmg -o out.raw    # expand UDIF to raw
    hdiutil info in.dmg                  # chunk table

Single-dash hdiutil options (`-mountpoint`, `-size`, `-fs`, `-volname`) are accepted.

- UDIF chunk types: zero, raw, ignore, ADC, zlib, bzip2, LZFSE, LZMA.
- Raw images and single-run uncompressed UDIFs are loop-mounted in place (writes persist).
- Compressed UDIFs are expanded into an unlinked scratch file (`$HDIUTIL_TMPDIR`, default
  `/var/tmp/hdiutil-rs`); changes are lost on detach. Use `convert` to get a raw image to modify.
- HFS+ is found at offset 0, or inside a GPT or Apple partition map.
- The kernel hfsplus driver mounts journaled volumes read-only.
- Not implemented: encrypted images, sparse bundles, resize, writing compressed images.
