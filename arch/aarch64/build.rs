use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=GENRT_AARCH64_DTB_PATH");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR must be set"));
    let embedded_dtb = out_dir.join("embedded-qemu-virt.dtb");

    match env::var("GENRT_AARCH64_DTB_PATH") {
        Ok(src) => {
            let src_path = PathBuf::from(src);
            println!("cargo:rerun-if-changed={}", src_path.display());
            fs::copy(&src_path, &embedded_dtb).expect("failed to copy generated QEMU DTB");
        }
        Err(_) => {
            fs::write(&embedded_dtb, []).expect("failed to create empty embedded DTB placeholder");
        }
    }

    println!(
        "cargo:rustc-env=GENRT_AARCH64_EMBEDDED_DTB={}",
        embedded_dtb.display()
    );
}
