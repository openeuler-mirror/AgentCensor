//! Compiles the BPF program and exposes its object path to Rust code.

use std::env;
use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR must be set"));
    let object_path = out_dir.join("live_observation.bpf.o");
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").expect("target architecture");
    let bpf_arch = match target_arch.as_str() {
        "x86_64" => "-D__TARGET_ARCH_x86",
        "aarch64" => "-D__TARGET_ARCH_arm64",
        other => panic!("unsupported eBPF target architecture {other}"),
    };

    println!("cargo:rerun-if-changed=bpf/live_observation.bpf.c");
    println!("cargo:rerun-if-changed=bpf/censorscope_proc.h");
    println!("cargo:rerun-if-changed=bpf/censorscope_runtime.h");
    println!("cargo:rerun-if-changed=bpf/censorscope_helpers.h");
    println!("cargo:rerun-if-changed=bpf/include/censorscope_const.h");
    println!("cargo:rerun-if-env-changed=BPF_SYSTEM_INCLUDE");
    println!("cargo:rustc-env=EBPF_OBJECT={}", object_path.display());
    println!("cargo:rustc-env=EBPF_EVENT_TRANSPORT=ring-buffer");

    let mut clang_args = vec!["-I".to_string(), "bpf".to_string(), bpf_arch.to_string()];
    if let Some(include) = system_include(&target_arch) {
        clang_args.push(format!("-I{}", include.display()));
    }
    libbpf_cargo::SkeletonBuilder::new()
        .source("bpf/live_observation.bpf.c")
        .obj(&object_path)
        .clang_args(clang_args)
        .build()
        .expect("failed to compile CensorScope process lifecycle eBPF object");
}

fn system_include(target_arch: &str) -> Option<PathBuf> {
    if let Some(path) = env::var_os("BPF_SYSTEM_INCLUDE") {
        return Some(PathBuf::from(path));
    }
    let multiarch = match target_arch {
        "x86_64" => "x86_64-linux-gnu",
        "aarch64" => "aarch64-linux-gnu",
        _ => return None,
    };
    let path = PathBuf::from("/usr/include").join(multiarch);
    path.join("asm").is_dir().then_some(path)
}
