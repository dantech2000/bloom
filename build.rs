// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Locates libmpv for linking. Set `MPV_LIB_DIR` to override detection.

use std::{path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-env-changed=MPV_LIB_DIR");
    if let Ok(dir) = std::env::var("MPV_LIB_DIR").map(|d| d.trim().to_string())
        && !dir.is_empty()
    {
        println!("cargo:rustc-link-search=native={dir}");
        return;
    }
    // The copy that `dev/vendor-mpv` makes in the project. The binary finds
    // it from target/<profile>/, and from an app bundle's Frameworks folder.
    let vendored = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vendor/mpv/lib");
    println!("cargo:rerun-if-changed={}", vendored.display());
    if vendored.join("libmpv.dylib").exists() {
        println!("cargo:rustc-link-search=native={}", vendored.display());
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../../vendor/mpv/lib");
        // Test binaries are one folder deeper, in target/<profile>/deps/.
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../../../vendor/mpv/lib");
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");
        return;
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(out) = Command::new("pkg-config")
        .args(["--variable=libdir", "mpv"])
        .output()
    {
        let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !dir.is_empty() {
            candidates.push(PathBuf::from(dir));
        }
    }
    // Sibling `lib` of the mpv binary (Nix, Homebrew, MSYS layouts).
    if let Ok(out) = Command::new("which").arg("mpv").output() {
        let bin = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if let Ok(real) = std::fs::canonicalize(&bin)
            && let Some(prefix) = real.parent().and_then(|p| p.parent())
        {
            candidates.push(prefix.join("lib"));
        }
    }
    candidates.extend(
        [
            "/opt/homebrew/lib",
            "/usr/local/lib",
            "/usr/lib",
            "/usr/lib/x86_64-linux-gnu",
        ]
        .iter()
        .map(PathBuf::from),
    );
    for dir in candidates {
        let has_lib = [
            "libmpv.dylib",
            "libmpv.so",
            "libmpv.2.dylib",
            "libmpv.so.2",
            "mpv.lib",
            "libmpv.dll.a",
        ]
        .iter()
        .any(|name| dir.join(name).exists());
        if has_lib {
            println!("cargo:rustc-link-search=native={}", dir.display());
            return;
        }
    }
    println!("cargo:warning=libmpv not found; set MPV_LIB_DIR to the directory containing libmpv");
}
