use crate::model::Config;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub fn config_dir() -> Result<PathBuf> {
    let base = dirs::config_dir().context("no config dir on this platform")?;
    Ok(base.join("betterssh"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("hosts.toml"))
}

pub fn themes_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join("themes"))
}

pub fn ensure_dir() -> Result<()> {
    std::fs::create_dir_all(config_dir()?).context("create config dir")?;
    Ok(())
}

pub fn load<P: AsRef<Path>>(path: P) -> Result<Config> {
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("read config {}", path.as_ref().display()))?;
    let cfg: Config = toml::from_str(&raw).context("parse hosts.toml")?;
    Ok(cfg)
}

pub fn save<P: AsRef<Path>>(path: P, cfg: &Config) -> Result<()> {
    ensure_dir()?;
    let body = toml::to_string_pretty(cfg).context("serialize config")?;
    let tmp = path.as_ref().with_extension("toml.tmp");
    std::fs::write(&tmp, body).context("write tmp config")?;
    std::fs::rename(&tmp, path.as_ref()).context("rename config")?;
    Ok(())
}

pub fn load_default() -> Result<Config> {
    let p = config_path()?;
    if !p.exists() {
        return Ok(Config::default());
    }
    load(p)
}
