fn main() {
    if let Ok(ver) = std::env::var("RELEASE_VERSION") {
        println!("cargo:rustc-env=CARGO_PKG_VERSION={}", ver.trim_start_matches('v'));
        return;
    }

    if let Ok(output) = std::process::Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
    {
        if output.status.success() {
            let ver = String::from_utf8_lossy(&output.stdout).trim().to_string();
            println!("cargo:rustc-env=CARGO_PKG_VERSION={}", ver.trim_start_matches('v'));
            return;
        }
    }
}
