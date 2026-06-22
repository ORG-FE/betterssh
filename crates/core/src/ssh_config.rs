use crate::model::{Host, Identity};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub struct SshConfigEntry {
    pub pattern: Vec<String>,
    pub host_name: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_files: Vec<String>,
    pub proxy_jump: Option<String>,
    pub keepalive: Option<u16>,
}

pub fn parse_ssh_config<P: AsRef<Path>>(path: P) -> Result<Vec<SshConfigEntry>> {
    let raw = std::fs::read_to_string(path.as_ref())
        .with_context(|| format!("read {}", path.as_ref().display()))?;

    let mut entries: Vec<SshConfigEntry> = Vec::new();
    let mut current: Option<SshConfigEntry> = None;

    for line in raw.lines() {
        let line = trim_comment(line.trim());
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.splitn(2, |c: char| c.is_whitespace())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        if parts.len() < 2 {
            continue;
        }

        let keyword = parts[0].to_lowercase();
        let value = parts[1].trim();

        match keyword.as_str() {
            "host" => {
                if let Some(entry) = current.take() {
                    if !entry.pattern.is_empty() && !entry.pattern.iter().any(|p| p.contains('*') || p.contains('?')) {
                        entries.push(entry);
                    }
                }
                let patterns: Vec<String> = value.split_whitespace().map(|s| s.to_string()).collect();
                if !patterns.is_empty() {
                    current = Some(SshConfigEntry {
                        pattern: patterns,
                        host_name: None,
                        user: None,
                        port: None,
                        identity_files: Vec::new(),
                        proxy_jump: None,
                        keepalive: None,
                    });
                }
            }
            "hostname" => {
                if let Some(ref mut e) = current {
                    e.host_name = Some(value.to_string());
                }
            }
            "user" => {
                if let Some(ref mut e) = current {
                    e.user = Some(value.to_string());
                }
            }
            "port" => {
                if let Some(ref mut e) = current {
                    e.port = value.parse::<u16>().ok();
                }
            }
            "identityfile" => {
                if let Some(ref mut e) = current {
                    let expanded = expand_tilde(value);
                    e.identity_files.push(expanded.to_string_lossy().to_string());
                }
            }
            "proxyjump" => {
                if let Some(ref mut e) = current {
                    e.proxy_jump = Some(value.to_string());
                }
            }
            "serveraliveinterval" => {
                if let Some(ref mut e) = current {
                    e.keepalive = value.parse::<u16>().ok();
                }
            }
            "include" => {
                let include_path = expand_tilde(value);
                if let Ok(mut included) = parse_ssh_config(&include_path) {
                    entries.extend(included.drain(..));
                }
            }
            _ => {}
        }
    }

    if let Some(entry) = current {
        if !entry.pattern.is_empty() && !entry.pattern.iter().any(|p| p.contains('*') || p.contains('?')) {
            entries.push(entry);
        }
    }

    Ok(entries)
}

pub fn ssh_config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    PathBuf::from(home).join(".ssh").join("config")
}

pub fn entries_to_hosts(entries: &[SshConfigEntry]) -> Vec<Host> {
    entries.iter().map(|e| {
        let host_name = e.host_name.as_deref().unwrap_or("");
        let name = e.pattern.join(", ");
        let user = e.user.clone().unwrap_or_else(|| "root".into());
        let port = e.port.unwrap_or(22);
        let identity: Vec<Identity> = e.identity_files.iter().map(|p| {
            Identity::Key { path: p.clone(), passphrase: None }
        }).collect();

        Host {
            name: name.clone(),
            host: host_name.to_string(),
            port,
            user,
            identity,
            jump: e.proxy_jump.clone(),
            tags: vec![],
            group: None,
            keepalive: e.keepalive,
            on_connect: vec![],
            forwarding: vec![],
        }
    }).collect()
}

pub fn merge_hosts(existing: &[Host], imported: Vec<Host>) -> Vec<Host> {
    let mut merged = existing.to_vec();
    for h in imported {
        
        let dup = merged.iter().any(|x| {
            x.host == h.host && x.user == h.user && x.port == h.port
        });
        if !dup {
            merged.push(h);
        }
    }
    merged
}

fn trim_comment(line: &str) -> &str {
    let trimmed = line.trim();
    let mut in_quote = false;
    let mut comment_pos = None;
    for (i, c) in trimmed.char_indices() {
        if c == '"' {
            in_quote = !in_quote;
        } else if c == '#' && !in_quote {
            comment_pos = Some(i);
            break;
        }
    }
    if let Some(pos) = comment_pos {
        trimmed[..pos].trim()
    } else {
        trimmed
    }
}

fn expand_tilde(path: &str) -> PathBuf {
    if path.starts_with("~/") {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
        PathBuf::from(home).join(&path[2..])
    } else if path == "~" {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
        PathBuf::from(home)
    } else {
        PathBuf::from(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_config() {
        let config = "\
Host myserver
    HostName 192.168.1.1
    User root
    Port 2222

Host webserver
    HostName example.com
    User admin
";
        let entries = parse_ssh_config_from_str(config);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].pattern, vec!["myserver"]);
        assert_eq!(entries[0].host_name.as_deref(), Some("192.168.1.1"));
        assert_eq!(entries[0].user.as_deref(), Some("root"));
        assert_eq!(entries[0].port, Some(2222));
    }

    #[test]
    fn skip_wildcard_hosts() {
        let config = "\
Host *.example.com
    User admin

Host specific
    HostName 10.0.0.1
";
        let entries = parse_ssh_config_from_str(config);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].pattern, vec!["specific"]);
    }

    #[test]
    fn parse_identity_file() {
        let config = "\
Host server
    HostName 10.0.0.1
    IdentityFile ~/.ssh/id_ed25519
";
        let entries = parse_ssh_config_from_str(config);
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].identity_files.is_empty());
        assert!(entries[0].identity_files[0].contains(".ssh/id_ed25519"));
    }

    #[test]
    fn parse_multiple_patterns() {
        let config = "Host srv1 srv2 srv3\n    HostName 10.0.0.1\n";
        let entries = parse_ssh_config_from_str(config);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].pattern, vec!["srv1", "srv2", "srv3"]);
    }

    #[test]
    fn entries_to_hosts_conversion() {
        let config = "\
Host myserver
    HostName 10.0.0.1
    User root
    Port 2222
    IdentityFile ~/.ssh/id_rsa
";
        let entries = parse_ssh_config_from_str(config);
        let hosts = entries_to_hosts(&entries);
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].host, "10.0.0.1");
        assert_eq!(hosts[0].port, 2222);
        assert_eq!(hosts[0].user, "root");
    }

    fn parse_ssh_config_from_str(input: &str) -> Vec<SshConfigEntry> {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("betterssh_test_{}_{}", std::process::id(), id));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("ssh_config");
        std::fs::write(&path, input).expect("write test config");
        let result = parse_ssh_config(&path).expect("parse");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
        result
    }
}
