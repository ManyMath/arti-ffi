use cbindgen::{Config, Language};
use std::env;
use std::path::PathBuf;
use glob::glob;

fn main() {
    // Check if we're building for Android on a Linux host.
    android_on_linux_check();

    // Generate C bindings using cbindgen.
    generate_c_bindings();
}

/// Checks if the build target is Android and the host OS is Linux, then configures the environment
/// accordingly.
fn android_on_linux_check() {
    let target = env::var("TARGET").unwrap_or_default();
    if target.contains("android") && cfg!(target_os = "linux") {
        let ndk_home = env::var("ANDROID_NDK_HOME").unwrap_or_else(|_| {
            println!("cargo:warning=ANDROID_NDK_HOME is not set. Trying to infer it from _CARGOKIT_NDK_LINK_CLANG.");
            let path = env::var("_CARGOKIT_NDK_LINK_CLANG")
                .expect("_CARGOKIT_NDK_LINK_CLANG not set. Unable to find NDK path.");

            // Attempt to extract the NDK path from the clang link command.
            path.split("/toolchains/")
                .next()
                .expect("Failed to parse path to get NDK home.")
                .to_string()
        });

        let os = match env::consts::OS {
            "macos" => "darwin",
            "windows" => "windows",
            _ => "linux",
        };

        let link_search_glob = format!(
            "{}/toolchains/llvm/prebuilt/{}-x86_64/lib/clang/**/lib/linux",
            ndk_home, os
        );

        // Find the correct library path for linking against the Android NDK.
        let link_search_path = glob(&link_search_glob)
            .expect("Failed to read glob pattern for link search path")
            .filter_map(Result::ok) // Only keep the Ok values, discard Err.
            .next()
            .expect("Failed to find link search path"); // Expect a valid path.

        println!("cargo:rustc-link-lib=static=clang_rt.builtins-x86_64-android");
        println!("cargo:rustc-link-search={}", link_search_path.display());
    }
}

/// Generates the C bindings for the Rust library using cbindgen.
fn generate_c_bindings() {
    let crate_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let output_file = target_dir().join("arti_ffi.h");

    let config = Config {
        language: Language::C,
        include_guard: Some("ARTI_FFI_H".to_string()), // Optional: Add an include guard.
        ..Default::default()
    };

    // Run cbindgen to generate the bindings.
    cbindgen::generate_with_config(crate_dir, config)
        .expect("Failed to generate C bindings with cbindgen")
        .write_to_file(output_file);
}

/// Determines the target directory for the build output.
fn target_dir() -> PathBuf {
    env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("target"))
}
