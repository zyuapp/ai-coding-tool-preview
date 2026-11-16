//! Builds the simulator panel's native helper on macOS, the way `build-simulator-helper.mts` does.
//! Without Xcode there is nothing to build, and the panel says the simulator is unavailable.

use std::path::PathBuf;
use std::process::Command;

const FRAMEWORKS: [&str; 9] = ["Foundation", "AppKit", "CoreAudio", "CoreImage", "CoreMedia", "CoreVideo", "ImageIO", "IOSurface", "VideoToolbox"];

fn xcrun(args: &[&str]) -> Option<String> {
    let output = Command::new("xcrun").args(["--sdk", "macosx"]).args(args).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn main() {
    let source = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("native/simulator/aic-simulator.m");
    println!("cargo:rerun-if-changed={}", source.display());
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let output = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("aic-simulator");
    let (Some(clang), Some(sdk)) = (xcrun(&["-f", "clang"]), xcrun(&["--show-sdk-path"])) else {
        println!("cargo:warning=Skipping the simulator helper: Xcode is not installed.");
        return;
    };
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x86_64",
        _ => "arm64",
    };
    let mut command = Command::new(clang);
    command.args(["-isysroot", &sdk, "-arch", arch, "-mmacosx-version-min=13.0", "-fobjc-arc", "-O2", "-Wall", "-Werror"]);
    for framework in FRAMEWORKS {
        command.args(["-framework", framework]);
    }
    command.arg(&source).arg("-o").arg(&output);
    match command.status() {
        Ok(status) if status.success() => println!("cargo:rustc-env=ACT_SIMULATOR_HELPER_BUILT={}", output.display()),
        _ => println!("cargo:warning=The simulator helper did not build."),
    }
}
