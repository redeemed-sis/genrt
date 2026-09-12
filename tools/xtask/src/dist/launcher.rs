use std::path::PathBuf;

use crate::{
    artifacts::{Aarch64Artifacts, Profile},
    qemu,
};

/// Construct a production command using filenames relative to the bundle.
///
/// # Arguments
///
/// * `cpu_count` - Supported release topology in `1..=qemu::MAX_CPUS`.
///
/// # Returns
///
/// Returns an unlaunched, non-debugging reset-preserving configuration.
pub(super) fn config(cpu_count: usize) -> qemu::Config {
    let artifacts = Aarch64Artifacts::for_cpu_count(Profile::Release, cpu_count);
    qemu::Config {
        kernel: PathBuf::from("genrt-aarch64.elf"),
        dtb: PathBuf::from(artifacts.dtb().file_name().unwrap()),
        initramfs: PathBuf::from("initramfs.cpio"),
        cpu_count,
        wait_for_gdb: false,
        reset_behavior: qemu::ResetBehavior::Reset,
    }
}

/// Render a standalone POSIX launcher from canonical per-topology commands.
///
/// # Returns
///
/// Returns deterministic shell source; no command is executed or file written.
pub(super) fn script() -> String {
    let values = (1..=qemu::MAX_CPUS)
        .map(|count| count.to_string())
        .collect::<Vec<_>>()
        .join("|");
    let commands = (1..=qemu::MAX_CPUS)
        .map(|count| {
            let config = config(count);
            format!(
                "    {count})\n        dtb={}\n        set -- {}\n        ;;",
                config.dtb.display(),
                config.display().replace("\n  ", "\n            ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    include_str!("run.sh.in")
        .replace("@MAX_CPUS@", &qemu::MAX_CPUS.to_string())
        .replace("@CPU_VALUES@", &values)
        .replace("@COMMANDS@", &commands)
}

/// Render the standalone guide from the same commands as the launcher.
///
/// # Returns
///
/// Returns deterministic Markdown with default and maximum-topology examples.
pub(super) fn guide() -> String {
    let dtbs = (1..=qemu::MAX_CPUS)
        .map(|count| format!("`{}`", config(count).dtb.display()))
        .collect::<Vec<_>>()
        .join(", ");
    include_str!("RUN.md.in")
        .replace("@MAX_CPUS@", &qemu::MAX_CPUS.to_string())
        .replace("@DTB_LIST@", &dtbs)
        .replace("@DEFAULT_COMMAND@", &config(1).display())
        .replace("@SMP_COMMAND@", &config(qemu::MAX_CPUS).display())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt, process::Command};

    #[test]
    fn launcher_matches_canonical_commands_and_rejects_invalid_input() {
        let root = std::env::temp_dir().join(format!("genrt-launcher-{}", std::process::id()));
        // Exercise quoting without absolute loader paths (commas are QEMU syntax).
        let bundle = root.join("release 'space, $literal");
        let bin = root.join("bin");
        fs::create_dir_all(&bundle).unwrap();
        fs::create_dir_all(&bin).unwrap();
        let script_path = bundle.join("run.sh");
        fs::write(&script_path, script()).unwrap();
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();
        let stub = bin.join("qemu-system-aarch64");
        fs::write(
            &stub,
            "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\"\nexit 37\n",
        )
        .unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        let run = |args: &[&str]| {
            Command::new(&script_path)
                .args(args)
                .current_dir(&root)
                .env("PATH", &bin)
                .output()
                .unwrap()
        };
        // Help needs neither artifacts nor QEMU; shell syntax is checked too.
        assert!(
            Command::new("/bin/sh")
                .arg("-n")
                .arg(&script_path)
                .status()
                .unwrap()
                .success()
        );
        for flag in ["-h", "--help"] {
            let result = run(&[flag]);
            assert!(result.status.success());
            assert!(String::from_utf8_lossy(&result.stdout).contains("default: 1"));
        }
        for name in ["genrt-aarch64.elf", "initramfs.cpio"] {
            fs::write(bundle.join(name), "fixture").unwrap();
        }
        for count in 1..=qemu::MAX_CPUS {
            fs::write(bundle.join(config(count).dtb), "fixture").unwrap();
        }
        for count in 1..=qemu::MAX_CPUS {
            let value = count.to_string();
            for flag in ["-c", "--cpus"] {
                let result = run(&[flag, &value]);
                assert_eq!(result.status.code(), Some(37), "{result:?}");
                let command = config(count).command();
                let expected = std::iter::once(bundle.to_string_lossy().into_owned())
                    .chain(
                        command
                            .get_args()
                            .map(|arg| arg.to_string_lossy().into_owned()),
                    )
                    .collect::<Vec<_>>();
                assert_eq!(
                    String::from_utf8(result.stdout)
                        .unwrap()
                        .lines()
                        .collect::<Vec<_>>(),
                    expected
                );
            }
        }
        assert_eq!(run(&[]).stdout, run(&["-c", "1"]).stdout);
        for args in [
            vec!["-c"],
            vec!["--cpus"],
            vec!["-c", "0"],
            vec!["-c", "5"],
            vec!["-c", "-1"],
            vec!["-c", "+1"],
            vec!["-c", "01"],
            vec!["-c", "1.0"],
            vec!["-c", ""],
            vec!["-c", "99999999999999999999"],
            vec!["--cpus=2"],
            vec!["-c", "2", "--cpus", "3"],
            vec!["kernel.elf"],
            vec!["--"],
            vec!["-c", "2; exit 0"],
            vec!["--help", "--unknown"],
        ] {
            let result = run(&args);
            assert_eq!(result.status.code(), Some(2), "{args:?}: {result:?}");
            assert!(result.stdout.is_empty(), "must not launch QEMU: {args:?}");
            assert!(!result.stderr.is_empty());
        }
        for name in ["genrt-aarch64.elf", "initramfs.cpio", "qemu-virt.dtb"] {
            fs::remove_file(bundle.join(name)).unwrap();
            assert_eq!(run(&[]).status.code(), Some(2));
            fs::write(bundle.join(name), "").unwrap();
            assert_eq!(run(&[]).status.code(), Some(2));
            fs::write(bundle.join(name), "fixture").unwrap();
        }
        let secondary_dtb = bundle.join(config(qemu::MAX_CPUS).dtb);
        fs::remove_file(&secondary_dtb).unwrap();
        assert_eq!(
            run(&["-c", &qemu::MAX_CPUS.to_string()]).status.code(),
            Some(2)
        );
        assert_eq!(run(&[]).status.code(), Some(37));
        fs::remove_file(stub).unwrap();
        assert_eq!(run(&[]).status.code(), Some(127));
        assert!(run(&["--help"]).status.success());
        assert!(guide().contains(&config(1).display()));
        assert!(guide().contains(&config(qemu::MAX_CPUS).display()));
        for placeholder in ["@MAX_CPUS@", "@CPU_VALUES@", "@COMMANDS@"] {
            assert!(!script().contains(placeholder));
        }
        fs::remove_dir_all(root).unwrap();
    }
}
