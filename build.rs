use std::env;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CC");
    // The macOS ONNX Runtime checks the system version with `@available`,
    // which calls into clang's runtime library. ort-sys 2.0.0-rc.15 stopped
    // linking it, so the binary would not link.
    if env::var_os("CARGO_FEATURE_FASTEMBED").is_none()
        || env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos")
    {
        return;
    }
    let cc = env::var("CC").unwrap_or_else(|_| "clang".to_string());
    let Ok(output) = Command::new(&cc).arg("--print-resource-dir").output() else {
        println!("cargo:warning=could not run {cc} to find clang's runtime library");
        return;
    };
    let dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || dir.is_empty() {
        println!("cargo:warning={cc} --print-resource-dir gave no directory");
        return;
    }
    println!("cargo:rustc-link-search=native={dir}/lib/darwin");
    println!("cargo:rustc-link-lib=static=clang_rt.osx");
}
