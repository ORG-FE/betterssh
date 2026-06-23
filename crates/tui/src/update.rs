use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

const REPO: &str = "ORG-FE/betterssh";

static LATEST_AVAILABLE: AtomicBool = AtomicBool::new(false);
static CHECKING: AtomicBool = AtomicBool::new(false);
static DOWNLOADING: AtomicBool = AtomicBool::new(false);
static INSTALL_DONE: AtomicBool = AtomicBool::new(false);
static INSTALL_ERR: Mutex<Option<String>> = Mutex::new(None);
static LATEST_VER: OnceLock<String> = OnceLock::new();

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn latest_version() -> Option<&'static str> {
    LATEST_VER.get().map(|s| s.as_str())
}

pub fn is_available() -> bool {
    LATEST_AVAILABLE.load(Ordering::SeqCst)
}

pub fn is_checking() -> bool {
    CHECKING.load(Ordering::SeqCst)
}

pub fn is_downloading() -> bool {
    DOWNLOADING.load(Ordering::SeqCst)
}

pub fn is_done() -> bool {
    INSTALL_DONE.load(Ordering::SeqCst)
}

pub fn error() -> Option<String> {
    INSTALL_ERR.lock().ok()?.clone()
}

pub fn check_latest() {
    if CHECKING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        let result = fetch_latest();
        match result {
            Ok(ver) => {
                if ver != current_version() {
                    let _ = LATEST_VER.set(ver);
                    LATEST_AVAILABLE.store(true, Ordering::SeqCst);
                }
            }
            Err(e) => {
                if let Ok(mut err) = INSTALL_ERR.lock() {
                    *err = Some(format!("check failed: {}", e));
                }
            }
        }
        CHECKING.store(false, Ordering::SeqCst);
    });
}

fn fetch_latest() -> Result<String, String> {
    let url = format!("https://api.github.com/repos/{}/releases/latest", REPO);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(5))
        .timeout_read(std::time::Duration::from_secs(10))
        .build();
    let resp = agent
        .get(&url)
        .set("Accept", "application/vnd.github.v3+json")
        .set("User-Agent", "betterssh-update")
        .call()
        .map_err(|e| {
            let kind = match &e {
                ureq::Error::Status(code, _) => format!("status {}", code),
                ureq::Error::Transport(t) => format!("transport: {}", t.kind()),
            };
            kind
        })?;
    let json: serde_json::Value = resp.into_json().map_err(|e| format!("json parse: {}", e))?;
    let tag = json["tag_name"]
        .as_str()
        .ok_or_else(|| String::from("no tag_name in response"))?
        .trim_start_matches('v')
        .to_string();
    Ok(tag)
}

pub fn do_install() {
    if DOWNLOADING.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Ok(mut err) = INSTALL_ERR.lock() {
        *err = None;
    }
    INSTALL_DONE.store(false, Ordering::SeqCst);

    std::thread::spawn(|| {
        let result = inner_install();
        match result {
            Ok(()) => {
                INSTALL_DONE.store(true, Ordering::SeqCst);
                LATEST_AVAILABLE.store(false, Ordering::SeqCst);
            }
            Err(e) => {
                if let Ok(mut err) = INSTALL_ERR.lock() {
                    *err = Some(e);
                }
            }
        }
        DOWNLOADING.store(false, Ordering::SeqCst);
    });
}

fn inner_install() -> Result<(), String> {
    let ver = LATEST_VER.get().ok_or_else(|| String::from("no version"))?;
    let target = target_triple()?;
    let current_exe = std::env::current_exe().map_err(|e| format!("current_exe: {}", e))?;

    let bin_name = if cfg!(windows) {
        format!("betterssh-{}.exe", target)
    } else {
        format!("betterssh-{}", target)
    };
    let url = format!(
        "https://github.com/{}/releases/download/v{}/{}",
        REPO, ver, bin_name
    );

    let temp_dir = current_exe
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join(".betterssh_update");
    let _ = std::fs::create_dir_all(&temp_dir);
    let bin_path = temp_dir.join(&bin_name);

    let resp = ureq::get(&url)
        .set("User-Agent", "betterssh-update")
        .call()
        .map_err(|e| format!("download: {}", e))?;
    let mut reader = resp.into_reader();
    let mut out = std::fs::File::create(&bin_path).map_err(|e| format!("create: {}", e))?;
    std::io::copy(&mut reader, &mut out).map_err(|e| format!("copy: {}", e))?;
    drop(out);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(&bin_path, PermissionsExt::from_mode(0o755))
            .map_err(|e| format!("chmod: {}", e))?;
        std::fs::rename(&bin_path, &current_exe).map_err(|e| format!("replace: {}", e))?;
    }

    #[cfg(windows)]
    {
        let bak = current_exe.with_extension("old.exe");
        std::fs::rename(&current_exe, &bak).map_err(|e| format!("backup: {}", e))?;
        std::fs::rename(&bin_path, &current_exe).map_err(|e| format!("replace: {}", e))?;
        let _ = std::fs::remove_file(&bak);
    }

    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(())
}

fn target_triple() -> Result<String, String> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    match (os, arch) {
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu".into()),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu".into()),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin".into()),
        ("macos", "aarch64") => Ok("aarch64-apple-darwin".into()),
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc".into()),
        _ => Err(format!("unsupported: {}-{}", os, arch)),
    }
}
