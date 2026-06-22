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
    let resp = ureq::get(&url)
        .set("Accept", "application/vnd.github.v3+json")
        .set("User-Agent", "betterssh-update")
        .call()
        .map_err(|e| format!("http: {}", e))?;
    let json: serde_json::Value = resp
        .into_json()
        .map_err(|e| format!("json: {}", e))?;
    let tag = json["tag_name"]
        .as_str()
        .ok_or_else(|| String::from("no tag_name"))?
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
    let archive_ext = if cfg!(windows) {
        "zip"
    } else {
        "tar.gz"
    };
    let url = format!(
        "https://github.com/{}/releases/download/v{}/betterssh-{}.{}",
        REPO, ver, target, archive_ext
    );

    let temp_dir = std::env::temp_dir().join(format!("betterssh_updt_{}", ver));
    let _ = std::fs::create_dir_all(&temp_dir);
    let archive_path = temp_dir.join(format!("archive.{}", archive_ext));

    let resp = ureq::get(&url)
        .set("User-Agent", "betterssh-update")
        .call()
        .map_err(|e| format!("download: {}", e))?;
    let mut reader = resp.into_reader();
    let mut out =
        std::fs::File::create(&archive_path).map_err(|e| format!("create: {}", e))?;
    std::io::copy(&mut reader, &mut out).map_err(|e| format!("copy: {}", e))?;
    drop(out);

    let binary_name = "betterssh";
    #[cfg(windows)]
    let binary_name = "betterssh.exe";

    #[cfg(unix)]
    let new_bin = extract_tar_gz(&archive_path, &temp_dir, binary_name)?;
    #[cfg(windows)]
    let new_bin = extract_zip(&archive_path, &temp_dir, binary_name)?;

    let current_exe =
        std::env::current_exe().map_err(|e| format!("current_exe: {}", e))?;

    #[cfg(unix)]
    {
        let data =
            std::fs::read(&new_bin).map_err(|e| format!("read new: {}", e))?;
        std::fs::write(&current_exe, &data).map_err(|e| format!("write: {}", e))?;
        std::fs::set_permissions(
            &current_exe,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .ok();
    }

    #[cfg(windows)]
    {
        let tmp = current_exe.with_extension("old.exe");
        std::fs::rename(&current_exe, &tmp).map_err(|e| format!("backup: {}", e))?;
        std::fs::rename(&new_bin, &current_exe)
            .map_err(|e| format!("replace: {}", e))?;
        let _ = std::fs::remove_file(&tmp);
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

#[cfg(unix)]
fn extract_tar_gz(
    archive: &std::path::Path,
    dest: &std::path::Path,
    binary: &str,
) -> Result<std::path::PathBuf, String> {
    let out = std::process::Command::new("tar")
        .args([
            "xzf",
            archive.to_str().unwrap(),
            "-C",
            dest.to_str().unwrap(),
            "--strip-components=1",
        ])
        .output()
        .map_err(|e| format!("tar: {}", e))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("extract failed: {}", stderr));
    }
    let bin_path = dest.join(binary);
    if !bin_path.exists() {
        return Err("binary not found after extract".into());
    }
    Ok(bin_path)
}

#[cfg(windows)]
fn extract_zip(
    archive: &std::path::Path,
    dest: &std::path::Path,
    binary: &str,
) -> Result<std::path::PathBuf, String> {
    let file =
        std::fs::File::open(archive).map_err(|e| format!("open zip: {}", e))?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| format!("read zip: {}", e))?;
    let mut found = false;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| format!("entry {}: {}", i, e))?;
        let name = entry.name().replace('\\', "/");
        if name.ends_with(&format!("/{}", binary)) || name == binary {
            let mut data = Vec::new();
            std::io::copy(&mut entry, &mut data)
                .map_err(|e| format!("extract: {}", e))?;
            let out_path = dest.join(binary);
            std::fs::write(&out_path, &data).map_err(|e| format!("write: {}", e))?;
            found = true;
            break;
        }
    }
    if !found {
        return Err("binary not found in zip".into());
    }
    Ok(dest.join(binary))
}
