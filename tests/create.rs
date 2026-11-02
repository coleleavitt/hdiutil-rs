use hdiutil::{CreateOptions, Fs, create, udif};

#[test]
fn create_formats_hfsplus_and_seals_udif() {
    if std::process::Command::new("mkfs.hfsplus").arg("-h").output().is_err() {
        eprintln!("mkfs.hfsplus missing, skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let opts = CreateOptions { size: 8 << 20, fs: Fs::HfsPlus, volname: "ramdisk".into(), overwrite: false };
    let path = create(&dir.path().join("rd"), &opts).unwrap();
    assert_eq!(path.extension().unwrap(), "dmg");

    assert!(udif::is_udif(&path).unwrap());
    let u = udif::Udif::open(&path).unwrap();
    assert_eq!(u.size_bytes(), 8 << 20);
    assert_eq!(u.contiguous_raw(), Some(0));

    let raw = dir.path().join("out.raw");
    hdiutil::convert_to_raw(&path, &raw).unwrap();
    let bytes = std::fs::read(&raw).unwrap();
    assert_eq!(bytes.len(), 8 << 20);
    assert_eq!(&bytes[1024..1026], b"H+");
}
