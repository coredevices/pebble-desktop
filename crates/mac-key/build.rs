use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=../../vendor/encrypt.s");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let source = fs::read_to_string("../../vendor/encrypt.s")
        .unwrap()
        .replace(
            ".global sub_ffffff8000ec7320",
            ".globl _sub_ffffff8000ec7320",
        )
        .replace("sub_ffffff8000ec7320:", "_sub_ffffff8000ec7320:")
        .replace(".section .data", ".data")
        .replace(".section .text", ".text")
        .replace(".align 0x100", ".p2align 8")
        .replace(".align 0x10", ".p2align 4");
    let asm = out.join("encrypt.s");
    let image = out.join("encrypt.dylib");
    fs::write(&asm, source).unwrap();
    let status = Command::new("xcrun")
        .args([
            "clang",
            "-arch",
            "x86_64",
            "-mmacosx-version-min=12.0",
            "-nostdlib",
            "-lSystem",
            "-dynamiclib",
            "-Wl,-no_uuid",
            "-Wl,-install_name,pebble-xnu-encrypt",
            "-o",
        ])
        .arg(&image)
        .arg(&asm)
        .status()
        .expect("Xcode command line tools are required");
    assert!(
        status.success(),
        "Could not assemble the upstream encryption routine"
    );
    let bytes = fs::read(image).unwrap();
    let mach = goblin::mach::MachO::parse(&bytes, 0).unwrap();
    let entry = mach
        .symbols()
        .map(Result::unwrap)
        .find(|(name, _)| *name == "_sub_ffffff8000ec7320")
        .unwrap()
        .1
        .n_value;
    let segments: Vec<_> = mach
        .segments
        .iter()
        .filter(|s| s.vmsize > 0)
        .map(|s| (s.vmaddr, s.vmsize, s.fileoff, s.filesize))
        .collect();
    fs::write(
        out.join("encryption_image.rs"),
        format!(
            "const ENTRY: u64 = {entry};\nconst SEGMENTS: &[(u64,u64,u64,u64)] = &{segments:?};\n"
        ),
    )
    .unwrap();
}
