fn main() {
    // Register rustc cfg for switching between mount implementations.
    // When fuser MSRV is updated to v1.77 or above, we should switch from 'cargo:' to 'cargo::' syntax.
    println!("cargo:rustc-check-cfg=cfg(fuser_mount_impl, values(\"pure-rust\", \"libfuse2\", \"libfuse3\"))");

    // Build scripts are compiled for the host, so `cfg(target_os)` is wrong while
    // cross-compiling from Windows. Cargo provides the actual target explicitly.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    #[cfg(not(feature = "libfuse"))]
    if target_os != "linux" {
        unimplemented!("Building without libfuse is only supported on Linux");
    }

    #[cfg(not(feature = "libfuse"))]
    {
        println!("cargo:rustc-cfg=fuser_mount_impl=\"pure-rust\"");
    }
    #[cfg(feature = "libfuse")]
    {
        if target_os == "macos" {
            if pkg_config::Config::new()
                .atleast_version("2.6.0")
                .probe("fuse") // for macFUSE 4.x
                .map_err(|e| eprintln!("{}", e))
                .is_ok()
            {
                println!("cargo:rustc-cfg=fuser_mount_impl=\"libfuse2\"");
                println!("cargo:rustc-cfg=feature=\"macfuse-4-compat\"");
            } else {
                pkg_config::Config::new()
                    .atleast_version("2.6.0")
                    .probe("osxfuse") // for osxfuse 3.x
                    .map_err(|e| eprintln!("{}", e))
                    .unwrap();
                println!("cargo:rustc-cfg=fuser_mount_impl=\"libfuse2\"");
            }
        } else {
            // First try to link with libfuse3
            if pkg_config::Config::new()
                .atleast_version("3.0.0")
                .probe("fuse3")
                .map_err(|e| eprintln!("{e}"))
                .is_ok()
            {
                println!("cargo:rustc-cfg=fuser_mount_impl=\"libfuse3\"");
            } else {
                // Fallback to libfuse
                pkg_config::Config::new()
                    .atleast_version("2.6.0")
                    .probe("fuse")
                    .map_err(|e| eprintln!("{e}"))
                    .unwrap();
                println!("cargo:rustc-cfg=fuser_mount_impl=\"libfuse2\"");
            }
        }
    }
}
