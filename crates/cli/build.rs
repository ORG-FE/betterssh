fn main() {
    if let Ok(ver) = std::env::var("RELEASE_VERSION") {
        println!(
            "cargo:rustc-env=CARGO_PKG_VERSION={}",
            ver.trim_start_matches('v')
        );
        return;
    }

    if let Some(ver) = git_describe() {
        println!("cargo:rustc-env=CARGO_PKG_VERSION={}", ver);
    }
}

fn git_describe() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let ver = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if ver.contains('.') {
        Some(ver.trim_start_matches('v').to_string())
    } else {
        None
    }
}
