use std::env;
use std::error::Error;
use std::io;
use std::path::PathBuf;
use std::process::Command;

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=src/native.c");
    let output = PathBuf::from(
        env::var_os("OUT_DIR")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Cargo did not set OUT_DIR"))?,
    );
    let object = output.join("censorguard_native.o");
    let archive = output.join("libcensorguard_native.a");

    run(
        Command::new("clang")
            .args(["-std=c11", "-O2", "-g", "-Wall", "-Wextra", "-Werror", "-c"])
            .arg("src/native.c")
            .arg("-o")
            .arg(&object),
        "compile native libbpf shim",
    )?;
    run(
        Command::new("ar").arg("crs").arg(&archive).arg(&object),
        "archive native libbpf shim",
    )?;

    println!("cargo:rustc-link-search=native={}", output.display());
    println!("cargo:rustc-link-lib=static=censorguard_native");
    println!("cargo:rustc-link-lib=dylib=bpf");
    println!("cargo:rustc-link-lib=dylib=elf");
    println!("cargo:rustc-link-lib=dylib=z");
    Ok(())
}

fn run(command: &mut Command, purpose: &str) -> Result<(), io::Error> {
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("failed to {purpose}: {status}")))
    }
}
