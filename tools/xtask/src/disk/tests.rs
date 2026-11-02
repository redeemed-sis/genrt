use super::*;
use crate::{
    block::Mbr,
    cli::{Cli, Commands},
};
use clap::Parser;
use std::io::{Read, Seek, SeekFrom};

#[test]
fn cli_defaults_and_size_limits_match_the_supported_disk_format() {
    let cli = Cli::try_parse_from(["xtask", "disk-create", "--output", "data.raw"]).unwrap();
    assert!(
        matches!(cli.command, Commands::DiskCreate { size_mib: 128, label, layout: None, .. } if label == "GENRT")
    );
    assert!(
        Cli::try_parse_from([
            "xtask",
            "disk-create",
            "--output",
            "data.raw",
            "--layout",
            "disk.toml"
        ])
        .is_ok()
    );
    assert!(
        Cli::try_parse_from([
            "xtask",
            "disk-create",
            "--output",
            "data.raw",
            "--layout",
            "disk.toml",
            "--label",
            "OTHER"
        ])
        .is_err()
    );
    assert_eq!(image_bytes(128).unwrap(), 128 * MIB);
    assert!(image_bytes(64).is_err());
    assert!(image_bytes(u64::MAX).is_err());
}

#[test]
fn created_image_has_matching_fat32_geometry_and_is_not_overwritten() {
    let root = Staging::new(&std::env::temp_dir()).unwrap();
    let output = root.0.join("data with spaces.raw");
    create(&output, 65, "GENRTDATA", None).unwrap();
    assert_eq!(fs::metadata(&output).unwrap().len(), 65 * MIB);
    let mut file = fs::File::open(&output).unwrap();
    let mut mbr = [0; 512];
    file.read_exact(&mut mbr).unwrap();
    let table = Mbr::parse(&mbr, 65 * MIB / SECTOR_SIZE).unwrap();
    let partition = table.first_fat32().unwrap();
    assert_eq!(table.entries().iter().flatten().count(), 1);
    let mut boot = [0; 512];
    file.seek(SeekFrom::Start(partition.start_lba * SECTOR_SIZE))
        .unwrap();
    file.read_exact(&mut boot).unwrap();
    assert_eq!(u16::from_le_bytes(boot[11..13].try_into().unwrap()), 512);
    assert_eq!(
        u32::from_le_bytes(boot[28..32].try_into().unwrap()) as u64,
        partition.start_lba
    );
    assert_eq!(
        u32::from_le_bytes(boot[32..36].try_into().unwrap()) as u64,
        partition.sectors
    );
    assert_eq!(&boot[71..82], b"GENRTDATA  ");
    assert_eq!(&boot[82..90], b"FAT32   ");
    assert!(create(&output, 128, "OTHER", None).is_err());
    assert_eq!(fs::metadata(&output).unwrap().len(), 65 * MIB);
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut unchanged = [0; 512];
    file.read_exact(&mut unchanged).unwrap();
    assert_eq!(unchanged, mbr);
    #[cfg(unix)]
    {
        let link = root.0.join("link.raw");
        std::os::unix::fs::symlink(&output, &link).unwrap();
        assert!(create(&link, 65, "OTHER", None).is_err());
        assert!(fs::symlink_metadata(link).unwrap().is_symlink());
    }
    let large = root.0.join("large.raw");
    create(&large, 1024, "GENRT", None).unwrap();
    let mut file = fs::File::open(large).unwrap();
    file.read_exact(&mut mbr).unwrap();
    let table = Mbr::parse(&mbr, 1024 * MIB / SECTOR_SIZE).unwrap();
    file.seek(SeekFrom::Start(
        table.first_fat32().unwrap().start_lba * SECTOR_SIZE,
    ))
    .unwrap();
    file.read_exact(&mut boot).unwrap();
    assert!(
        boot[13] > 1,
        "the formatter chooses larger clusters for this volume"
    );
}

#[test]
fn invalid_input_or_formatter_failure_never_publishes_a_disk() {
    let root = Staging::new(&std::env::temp_dir()).unwrap();
    let output = root.0.join("failed.raw");
    assert!(create(&output, 64, "GENRT", None).is_err());
    assert!(create(&output, 65, "", None).is_err());
    // A reserved FAT label character fails inside the real formatter.
    assert!(create(&output, 65, "BAD/LABEL", None).is_err());
    assert!(!output.exists());
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);

    let layout = root.0.join("disk.toml");
    for invalid in [
        "schema = 2\npartitions = []",
        "schema = 1\npartitions = []",
        "schema = 1\n[[partitions]]\nlabel = 'BOOT'\n[[partitions]]\nlabel = 'DATA'",
        "schema = 1\n[[partitions]]\nlabel = 'BOOT'\nsize_mib = 0",
        "schema = 1\n[[partitions]]\nlabel = 'BOOT'\nunknown = true",
    ] {
        fs::write(&layout, invalid).unwrap();
        assert!(create(&output, 129, "GENRT", Some(&layout)).is_err());
        assert!(!output.exists());
    }
    fs::create_dir(root.0.join("source")).unwrap();
    fs::write(root.0.join("source/Case"), b"one").unwrap();
    fs::write(root.0.join("source/case"), b"two").unwrap();
    fs::write(
        &layout,
        "schema = 1\n[[partitions]]\nlabel = 'BOOT'\nsource = 'source'\n",
    )
    .unwrap();
    assert!(create(&output, 65, "GENRT", Some(&layout)).is_err());
    assert!(!output.exists());
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 2);
}

#[test]
fn populated_partitions_preserve_trees_fstab_and_reproducible_bytes() {
    let root = Staging::new(&std::env::temp_dir()).unwrap();
    fs::create_dir_all(root.0.join("first/DATA")).unwrap();
    fs::create_dir_all(root.0.join("second/nested dir")).unwrap();
    let fstab =
        b"/dev/disk0p1 /mnt/disk vfat defaults 0 0\n/dev/disk0p2 /mnt/disk/DATA vfat ro 0 0\n";
    fs::write(root.0.join("first/fstab"), fstab).unwrap();
    fs::write(root.0.join("first/.hidden"), b"hidden\0bytes").unwrap();
    fs::write(
        root.0.join("second/nested dir/hello.txt"),
        b"second partition\n",
    )
    .unwrap();
    let layout = root.0.join("disk.toml");
    fs::write(&layout, "schema = 1\n[[partitions]]\nsize_mib = 64\nlabel = 'BOOT'\nsource = 'first'\n[[partitions]]\nlabel = 'DATA'\nsource = 'second'\n").unwrap();
    let output = root.0.join("data.raw");
    create(&output, 129, "GENRT", Some(&layout)).unwrap();
    let mut file = fs::File::open(&output).unwrap();
    let mut mbr = [0; 512];
    file.read_exact(&mut mbr).unwrap();
    let table = Mbr::parse(&mbr, 129 * MIB / SECTOR_SIZE).unwrap();
    assert_eq!(table.entries().iter().flatten().count(), 2);
    for (index, partition) in table.entries().iter().flatten().enumerate() {
        let image_spec = format!(
            "{}@@{}",
            output.display(),
            partition.start_lba * SECTOR_SIZE
        );
        let (path, expected): (&str, &[u8]) = if index == 0 {
            ("::/fstab", fstab)
        } else {
            ("::/nested dir/hello.txt", b"second partition\n")
        };
        let exported = root.0.join(format!("export-{index}"));
        run(
            "mcopy",
            &["-i", &image_spec, path, exported.to_str().unwrap()],
            None,
        )
        .unwrap();
        assert_eq!(fs::read(exported).unwrap(), expected);
        if index == 0 {
            run("mdir", &["-i", &image_spec, "::/DATA"], None).unwrap();
            let hidden = root.0.join("hidden");
            run(
                "mcopy",
                &["-i", &image_spec, "::/.hidden", hidden.to_str().unwrap()],
                None,
            )
            .unwrap();
            assert_eq!(fs::read(hidden).unwrap(), b"hidden\0bytes");
        }
        let volume = root.0.join(format!("volume-{index}.fat"));
        file.seek(SeekFrom::Start(partition.start_lba * SECTOR_SIZE))
            .unwrap();
        let copied = std::io::copy(
            &mut (&mut file).take(partition.sectors * SECTOR_SIZE),
            &mut fs::File::create(&volume).unwrap(),
        )
        .unwrap();
        assert_eq!(copied, partition.sectors * SECTOR_SIZE);
        run("fsck.fat", &["-n", volume.to_str().unwrap()], None).unwrap();
    }
    std::thread::sleep(std::time::Duration::from_secs(2));
    let repeated = root.0.join("repeated.raw");
    create(&repeated, 129, "GENRT", Some(&layout)).unwrap();
    run(
        "cmp",
        &[output.to_str().unwrap(), repeated.to_str().unwrap()],
        None,
    )
    .unwrap();
}
