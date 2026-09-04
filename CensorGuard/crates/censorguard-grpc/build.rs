// protoc is vendored so the build does not depend on a system installation.
// `env::set_var` is unsafe in edition 2024; build scripts are single-threaded here.
#![allow(unsafe_code)]

fn main() -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    }
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let api = manifest.join("../../api");
    tonic_build::configure()
        .build_server(true)
        .build_client(false)
        .compile_protos(&[api.join("censorguard/v1/censorguard.proto")], &[api])?;
    Ok(())
}
