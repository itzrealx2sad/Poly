use std::path::Path;
use std::process::Command;

fn main() {
    if let Ok(git_dir) = Command::new("git").args(["rev-parse", "--git-dir"]).output() {
        if git_dir.status.success() {
            if let Ok(dir_str) = String::from_utf8(git_dir.stdout) {
                let git_path = Path::new(dir_str.trim());
                let head = git_path.join("HEAD");
                if head.exists() {
                    println!("cargo:rerun-if-changed={}", head.display());
                    if let Ok(content) = std::fs::read_to_string(&head) {
                        if let Some(ref_path) = content.strip_prefix("ref: ") {
                            let ref_file = git_path.join(ref_path.trim());
                            if ref_file.exists() {
                                println!("cargo:rerun-if-changed={}", ref_file.display());
                            }
                        }
                    }
                }
                let packed = git_path.join("packed-refs");
                if packed.exists() {
                    println!("cargo:rerun-if-changed={}", packed.display());
                }
            }
        }
    }

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
