use std::process::Command;

fn main() {
    let git_hash = Command::new("git")
        .args(["rev-parse", "--short=10", "HEAD"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout).ok().map(|s| s.trim().to_string())
            } else {
                None
            }
        });
    let version = std::env::var("POLY_VERSION")
        .or_else(|_| std::env::var("SONORA_VERSION"))
        .ok()
        .or(git_hash)
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    println!("cargo:rustc-env=POLY_BUILD_VERSION={version}");
    println!("cargo:rerun-if-env-changed=POLY_VERSION");
    println!("cargo:rerun-if-env-changed=SONORA_VERSION");
}
