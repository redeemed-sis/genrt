//! Shared-kernel hardware builds; artifacts stay separate from QEMU.

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, ensure};

use crate::artifacts::{AARCH64_TARGET, verify_kernel_composition};

const BOARD_NAME: &str = "raspberrypi-3b-plus";
const KERNEL_ENTRY: usize = 0x0200_0000;
const FIT_UPLOAD: usize = 0x0100_0000;
const MAX_IMAGE_SIZE: usize = 8 * 1024 * 1024;
const _: () = assert!(KERNEL_ENTRY + MAX_IMAGE_SIZE < 0x0800_0000);
const ARM64_HEADER_SIZE: usize = 64;
const ARM64_PLACEMENT_FLAGS: u64 = 8;

/// Build the complete Raspberry Pi kernel, DTB, and initramfs FIT.
///
/// # Arguments
///
/// * `wake_profile` - Opt in to diagnostic code in an isolated output directory.
///
/// # Returns
///
/// Returns `Ok(())` after building the release staticlib in a separate Cargo
/// target directory, linking without section GC, verifying boot autonomy, and
/// packaging kernel, board DTB, and production initramfs in one deterministic
/// FIT. Prints the
/// upload path and boot command. No serial port is opened and no board/storage
/// state is modified.
///
/// # Errors
///
/// Returns an error on tool/filesystem failure, missing runtime symbols, invalid
/// image metadata or size, nonidentical repeated FITs, or a failed post-link
/// check. Requires Cargo, LLD, LLVM objcopy/nm/objdump, readelf, dtc, fdtget,
/// mkimage, and dumpimage.
pub(crate) fn build(wake_profile: bool) -> Result<()> {
    let name = BOARD_NAME;
    let directory = format!(
        "target/{name}{}",
        if wake_profile { "/wake-profile" } else { "" }
    );
    let source = format!("arch/aarch64/platform/{name}");
    fs::create_dir_all(&directory)?;
    let root = Path::new(&directory);
    let elf = root.join(format!("genrt-{name}.elf"));
    let binary = elf.with_extension("bin");
    let dtb = elf.with_extension("dtb");
    let image = elf.with_extension("itb");

    run(Command::new("cargo")
        .args(["build", "--locked", "--release", "-p", "genrt-arch-aarch64"])
        .args(["--target", AARCH64_TARGET, "--no-default-features"])
        .args([
            "--features",
            &format!(
                "platform-{name}{}",
                if wake_profile { ",wake-profile" } else { "" }
            ),
        ])
        .arg("--target-dir")
        .arg(root.join("cargo")))?;
    let library = root
        .join("cargo")
        .join(AARCH64_TARGET)
        .join("release/libgenrt_arch_aarch64.a");
    run(Command::new("ld.lld")
        .arg("--build-id=none")
        .arg(format!(
            "--defsym=GENRT_BOOT_PHYS_BASE=0x{:x}",
            KERNEL_ENTRY
        ))
        .args(["-T", "arch/aarch64/link/kernel.ld", "-e", "_start", "-o"])
        .arg(&elf)
        .arg("--whole-archive")
        .arg(&library)
        .arg("--no-whole-archive"))?;
    crate::workflow::verify_aarch64_boot_text_autonomy(&elf)?;
    verify_kernel_composition(&elf, wake_profile)?;
    let symbols = output(
        Command::new("llvm-nm")
            .args(["--defined-only", "-C"])
            .arg(&elf),
    )?;
    for symbol in [
        "kernel_main",
        "rust_entry",
        "boot_build_page_tables",
        "__vectors",
        "kernel::syscall::",
        "kernel::sched::",
    ] {
        ensure!(symbols.contains(symbol), "full kernel missing {symbol}");
    }

    run(Command::new("llvm-objcopy")
        .args(["-O", "binary"])
        .arg(&elf)
        .arg(&binary))?;
    let mut payload = fs::read(&binary)?;
    let memory_size = payload_memory_size(&payload)?;
    // Include trailing NOBITS storage in the loaded range, not just the ARM64
    // size field: bootm's LMB reservation must also cover the kernel's BSS.
    payload.resize(memory_size, 0);
    verify_payload(&payload)?;
    fs::write(&binary, &payload)?;

    let initramfs_path = crate::workflow::build_initramfs(
        crate::artifacts::Profile::Release,
        None,
        None,
        Some(root.join("initramfs.cpio")),
        false,
    )?;
    let initramfs = fs::read(initramfs_path)?;

    run(Command::new("dtc")
        .args(["-I", "dts", "-O", "dtb", "-o"])
        .arg(&dtb)
        .arg(format!("{source}/board.dts")))?;
    verify_board(&dtb)?;
    fs::copy(format!("{source}/image.its"), root.join("image.its"))?;
    build_fit(root, &format!("genrt-{name}.itb"))?;
    let tree = fs::read(&dtb)?;
    verify_fit(&image, &payload, &tree, &initramfs)?;
    build_fit(root, "repeat.itb")?;
    ensure!(
        fs::read(&image)? == fs::read(root.join("repeat.itb"))?,
        "FIT serialization is not deterministic"
    );
    fs::remove_file(root.join("repeat.itb"))?;
    println!("built {}", elf.display());
    println!("UART transfer: {} -> 0x{:08x}", image.display(), FIT_UPLOAD);
    println!("U-Boot: bootm {:08x}", FIT_UPLOAD);
    println!("Experimental Pi SMP runtime: FIT includes production /init.");
    println!("Procedure: {source}/README.md");
    Ok(())
}

fn run(command: &mut Command) -> Result<()> {
    let name = command.get_program().to_string_lossy().into_owned();
    let status = command
        .status()
        .with_context(|| format!("cannot run {name}"))?;
    ensure!(status.success(), "{name} failed for hardware build");
    Ok(())
}

fn output(command: &mut Command) -> Result<String> {
    let result = command
        .output()
        .context("cannot inspect hardware artifact")?;
    ensure!(
        result.status.success(),
        "artifact inspection failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(String::from_utf8(result.stdout)?.trim().to_owned())
}

fn property(path: &Path, node: &str, name: &str, kind: &str) -> Result<String> {
    output(
        Command::new("fdtget")
            .args(["-t", kind])
            .arg(path)
            .args([node, name]),
    )
}

fn build_fit(root: &Path, name: &str) -> Result<()> {
    run(Command::new("mkimage")
        .current_dir(root)
        .env("SOURCE_DATE_EPOCH", "0")
        .args(["-f", "image.its", name]))
}

fn verify_board(path: &Path) -> Result<()> {
    let properties: &[(&str, &str, &str, &str)] = &[
        (
            "/",
            "compatible",
            "s",
            "raspberrypi,3-model-b-plus brcm,bcm2837",
        ),
        ("/memory@0", "reg", "x", "0 0 0 8000000"),
        (
            "/soc@7e000000",
            "ranges",
            "x",
            "7e000000 0 3f000000 1000000",
        ),
        ("/soc@7e000000/gpio@7e200000", "reg", "x", "7e200000 b4"),
        ("/soc@7e000000/aux@7e215000", "reg", "x", "7e215000 100"),
        ("/soc@7e000000/serial@7e201000", "reg", "x", "7e201000 200"),
        ("/soc@7e000000/serial@7e215040", "reg", "x", "7e215040 40"),
    ];
    for &(node, name, kind, expected) in properties {
        ensure!(
            property(path, node, name, kind)? == expected,
            "wrong board {node}/{name}"
        );
    }
    Ok(())
}

fn verify_fit(path: &Path, payload: &[u8], dtb: &[u8], initramfs: &[u8]) -> Result<()> {
    let size = fs::metadata(path)?.len() as usize;
    validate_upload_size(size)?;
    let config = format!("/configurations/{BOARD_NAME}");
    let entry = format!("{KERNEL_ENTRY:x}");
    for (node, name, kind, expected) in [
        ("/", "timestamp", "x", "0"),
        ("/configurations", "default", "s", BOARD_NAME),
        (&config, "kernel", "s", "kernel"),
        (&config, "fdt", "s", "fdt"),
        ("/images/kernel", "type", "s", "kernel"),
        ("/images/kernel", "arch", "s", "arm64"),
        ("/images/kernel", "os", "s", "linux"),
        ("/images/kernel", "compression", "s", "none"),
        ("/images/kernel", "load", "x", entry.as_str()),
        ("/images/kernel", "entry", "x", entry.as_str()),
        ("/images/fdt", "type", "s", "flat_dt"),
        ("/images/fdt", "arch", "s", "arm64"),
        ("/images/fdt", "compression", "s", "none"),
    ] {
        ensure!(
            property(path, node, name, kind)? == expected,
            "wrong FIT {node}/{name}"
        );
    }
    for (node, key, kind, expected) in [
        (config.as_str(), "ramdisk", "s", "ramdisk"),
        ("/images/ramdisk", "type", "s", "ramdisk"),
        ("/images/ramdisk", "arch", "s", "arm64"),
        ("/images/ramdisk", "os", "s", "linux"),
        ("/images/ramdisk", "compression", "s", "none"),
        ("/images/ramdisk", "load", "x", "4000000"),
    ] {
        ensure!(
            property(path, node, key, kind)? == expected,
            "wrong FIT {node}/{key}"
        );
    }
    for (index, (name, expected)) in [("kernel", payload), ("fdt", dtb), ("ramdisk", initramfs)]
        .into_iter()
        .enumerate()
    {
        let extracted = path.with_file_name(format!("verified-{name}.bin"));
        run(Command::new("dumpimage")
            .args(["-T", "flat_dt", "-p", &index.to_string(), "-o"])
            .arg(&extracted)
            .arg(path))?;
        ensure!(
            fs::read(&extracted)? == expected,
            "FIT {name} differs from build input"
        );
        fs::remove_file(extracted)?;
        let node = format!("/images/{name}/hash-1");
        let algorithm = property(path, &node, "algo", "s")?;
        let hash = property(path, &node, "value", "bx")?
            .split_whitespace()
            .map(|word| u8::from_str_radix(word, 16))
            .collect::<Result<Vec<_>, _>>()?;
        verify_fit_hash(&algorithm, &hash, expected)
            .with_context(|| format!("invalid FIT {name} hash"))?;
    }
    Ok(())
}

fn verify_fit_hash(algorithm: &str, hash: &[u8], data: &[u8]) -> Result<()> {
    ensure!(algorithm == "crc32", "wrong FIT hash algorithm");
    let mut crc = flate2::Crc::new();
    crc.update(data);
    // FIT encodes the IEEE CRC32 value as a big-endian DT cell.
    ensure!(hash == crc.sum().to_be_bytes(), "FIT hash mismatch");
    Ok(())
}

fn validate_upload_size(size: usize) -> Result<()> {
    ensure!(
        size > 0 && size < KERNEL_ENTRY - FIT_UPLOAD,
        "FIT overlaps kernel load window"
    );
    Ok(())
}

// Before padding, the header's memory extent may exceed the serialized
// sections. Reject overflow, truncation, and excessive allocation first.
fn payload_memory_size(payload: &[u8]) -> Result<usize> {
    ensure!(payload.len() > ARM64_HEADER_SIZE, "truncated ARM64 Image");
    let size = usize::try_from(u64::from_le_bytes(payload[16..24].try_into().unwrap()))?;
    ensure!(
        size >= payload.len() && size <= MAX_IMAGE_SIZE,
        "invalid ARM64 memory size"
    );
    Ok(size)
}

// U-Boot calls booti_setup even for a kernel stored inside a FIT.
fn verify_payload(payload: &[u8]) -> Result<()> {
    let size = payload_memory_size(payload)?;
    ensure!(size == payload.len(), "ARM64 BSS must be serialized");
    let word = |offset| u64::from_le_bytes(payload[offset..offset + 8].try_into().unwrap());
    ensure!(
        payload[..4] == 0x1400_0010u32.to_le_bytes(),
        "entry must skip ARM64 header"
    );
    ensure!(payload[4..8] == [0; 4], "unexpected second entry word");
    ensure!(word(8) == 0, "unexpected ARM64 text offset");
    ensure!(
        word(24) == ARM64_PLACEMENT_FLAGS,
        "wrong ARM64 placement flags"
    );
    ensure!(payload[32..56] == [0; 24], "nonzero reserved ARM64 fields");
    ensure!(payload[56..60] == *b"ARM\x64", "bad ARM64 Image magic");
    ensure!(payload[60..64] == [0; 4], "unexpected PE/COFF offset");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_hash_policy_checks_algorithm_bytes_and_payload() {
        let data = b"123456789";
        let crc = 0xcbf4_3926u32;
        verify_fit_hash("crc32", &crc.to_be_bytes(), data).unwrap();
        verify_fit_hash("crc32", &[0; 4], b"").unwrap();
        assert!(verify_fit_hash("sha256", &crc.to_be_bytes(), data).is_err());
        assert!(verify_fit_hash("crc32", &crc.to_le_bytes(), data).is_err());
        assert!(verify_fit_hash("crc32", &crc.to_be_bytes()[..3], data).is_err());
        assert!(verify_fit_hash("crc32", &crc.to_be_bytes(), b"123456788").is_err());
    }

    #[test]
    fn bcm2837_backends_with_mock_registers() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let directory =
            std::env::temp_dir().join(format!("genrt-bcm2837-test-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        for source in ["bcm2837_uart", "bcm2837_interrupts"] {
            let executable = directory.join(source);
            run(Command::new("rustc")
                .current_dir(root)
                .args([
                    "--edition=2024",
                    "--test",
                    "--cfg",
                    "feature=\"platform-raspberrypi-3b-plus\"",
                ])
                .arg(format!("tests/arch/{source}.rs"))
                .arg("-o")
                .arg(&executable))
            .unwrap();
            run(&mut Command::new(&executable)).unwrap();
        }
        fs::remove_dir_all(directory).unwrap();
    }

    fn valid_payload() -> Vec<u8> {
        let mut payload = vec![0; ARM64_HEADER_SIZE + 16];
        let size = payload.len() as u64;
        payload[..4].copy_from_slice(&0x1400_0010u32.to_le_bytes());
        payload[16..24].copy_from_slice(&size.to_le_bytes());
        payload[24..32].copy_from_slice(&ARM64_PLACEMENT_FLAGS.to_le_bytes());
        payload[56..60].copy_from_slice(b"ARM\x64");
        payload
    }

    #[test]
    fn arm64_contract_rejects_flat_code_and_invalid_header() {
        let payload = valid_payload();
        verify_payload(&payload).unwrap();
        assert!(verify_payload(&vec![0; 570]).is_err());
        for size in 0..=ARM64_HEADER_SIZE {
            assert!(verify_payload(&payload[..size]).is_err());
        }
        for offset in 0..ARM64_HEADER_SIZE {
            let mut bad = payload.clone();
            bad[offset] ^= 1;
            assert!(verify_payload(&bad).is_err(), "offset={offset}");
        }
    }

    #[test]
    fn memory_extent_and_upload_are_bounded() {
        let mut payload = valid_payload();
        payload[16..24].copy_from_slice(&128u64.to_le_bytes());
        assert_eq!(payload_memory_size(&payload).unwrap(), 128);
        assert!(verify_payload(&payload).is_err());
        payload.resize(128, 0);
        verify_payload(&payload).unwrap();
        for size in [0, 64, (MAX_IMAGE_SIZE + 1) as u64, u64::MAX] {
            payload[16..24].copy_from_slice(&size.to_le_bytes());
            assert!(payload_memory_size(&payload).is_err());
        }
        assert!(validate_upload_size(MAX_IMAGE_SIZE).is_ok());
        assert_eq!(KERNEL_ENTRY & 0x1fffff, 0);
        for size in [0, KERNEL_ENTRY - FIT_UPLOAD, usize::MAX] {
            assert!(validate_upload_size(size).is_err());
        }
    }

    #[test]
    fn cli_build_needs_no_options() {
        use crate::cli::{Cli, Commands};
        use clap::Parser;

        let pi = Cli::try_parse_from(["xtask", "build-raspberrypi-3b-plus"]).unwrap();
        assert!(matches!(
            pi.command,
            Commands::BuildRaspberrypi3bPlus {
                wake_profile: false
            }
        ));
    }
}
