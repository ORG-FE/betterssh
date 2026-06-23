use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Host {
    pub name: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub identity: Vec<Identity>,
    #[serde(default)]
    pub jump: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub keepalive: Option<u16>,
    #[serde(default)]
    pub on_connect: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forwarding: Vec<PortForward>,
}

fn default_port() -> u16 {
    22
}

impl Host {
    pub fn addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    pub fn resolve(&self) -> anyhow::Result<SocketAddr> {
        use std::net::ToSocketAddrs;
        self.addr()
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| anyhow::anyhow!("no addr for {}", self.addr()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Identity {
    Key { path: String, passphrase: Option<String> },
    Password { from_agent: Option<bool> },
    Agent,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub host: Vec<Host>,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub snippets: Vec<Snippet>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snippet {
    pub name: String,
    pub cmd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            keepalive_secs: default_keepalive(),
            term_cols: default_term_cols(),
            term_rows: default_term_rows(),
            term_type: default_term(),
            log_lines: default_log_lines(),
            theme: default_theme(),
            default_user: default_default_user(),
            ping_check: default_ping_check(),
            auto_reconnect: default_auto_reconnect(),
            scrollback: default_scrollback(),
            mouse: default_mouse(),
            show_metrics: default_show_metrics(),
            keybindings: default_keybindings(),
            macros: default_macros(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Macro {
    pub name: String,
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub enum ForwardDirection {
    #[default]
    #[serde(rename = "local")]
    Local,
    #[serde(rename = "remote")]
    Remote,
    #[serde(rename = "dynamic")]
    Dynamic,
}

impl std::fmt::Display for ForwardDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForwardDirection::Local => write!(f, "-L"),
            ForwardDirection::Remote => write!(f, "-R"),
            ForwardDirection::Dynamic => write!(f, "-D"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortForward {
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub direction: ForwardDirection,
    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,
    #[serde(default = "default_listen_port")]
    pub listen_port: u16,
    #[serde(default)]
    pub target_host: String,
    #[serde(default)]
    pub target_port: u16,
    #[serde(default)]
    pub active: bool,
}

fn default_listen_addr() -> String { "127.0.0.1".into() }
fn default_listen_port() -> u16 { 8080 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_keepalive")]
    pub keepalive_secs: u16,
    #[serde(default = "default_term_cols")]
    pub term_cols: u16,
    #[serde(default = "default_term_rows")]
    pub term_rows: u16,
    #[serde(default = "default_term")]
    pub term_type: String,
    #[serde(default = "default_log_lines")]
    pub log_lines: usize,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_default_user")]
    pub default_user: String,
    #[serde(default = "default_ping_check")]
    pub ping_check: bool,
    #[serde(default = "default_auto_reconnect")]
    pub auto_reconnect: bool,
    #[serde(default = "default_scrollback")]
    pub scrollback: usize,
    #[serde(default = "default_mouse")]
    pub mouse: bool,
    #[serde(default = "default_show_metrics")]
    pub show_metrics: bool,
    #[serde(default)]
    pub keybindings: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub macros: Vec<Macro>,
}

fn default_keepalive() -> u16 { 30 }
fn default_term_cols() -> u16 { 120 }
fn default_term_rows() -> u16 { 32 }
fn default_term() -> String { "xterm-256color".into() }
fn default_log_lines() -> usize { 1000 }
fn default_theme() -> String { "default".into() }
fn default_default_user() -> String { "root".into() }
fn default_ping_check() -> bool { true }
fn default_auto_reconnect() -> bool { false }
fn default_scrollback() -> usize { 5000 }
fn default_mouse() -> bool { false }
fn default_show_metrics() -> bool { true }
fn default_keybindings() -> HashMap<String, String> { HashMap::new() }
fn default_macros() -> Vec<Macro> { Vec::new() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_addr() {
        let h = Host {
            name: "srv".into(),
            host: "1.2.3.4".into(),
            port: 2222,
            user: "root".into(),
            identity: vec![],
            jump: None,
            tags: vec![],
            group: None,
            keepalive: None,
            on_connect: vec![],
            forwarding: vec![],
        };
        assert_eq!(h.addr(), "1.2.3.4:2222");
    }

    #[test]
    fn host_default_port() {
        let toml_str = r#"
name = "test"
host = "example.com"
user = "root"
"#;
        let h: Host = toml::from_str(toml_str).unwrap();
        assert_eq!(h.port, 22);
    }

    #[test]
    fn config_defaults() {
        let cfg = Config::default();
        assert!(cfg.host.is_empty());
        assert!(cfg.snippets.is_empty());
        assert_eq!(cfg.settings.keepalive_secs, 30);
        assert_eq!(cfg.settings.term_cols, 120);
        assert_eq!(cfg.settings.term_rows, 32);
    }

    #[test]
    fn config_toml_roundtrip() {
        let original = Config {
            host: vec![Host {
                name: "test".into(),
                host: "10.0.0.1".into(),
                port: 22,
                user: "root".into(),
                identity: vec![Identity::Key {
                    path: "~/.ssh/id_rsa".into(),
                    passphrase: None,
                }],
                jump: Some("jump-host".into()),
                tags: vec!["prod".into()],
                group: Some("servers".into()),
                keepalive: Some(60),
                on_connect: vec![],
                forwarding: vec![],
            }],
            settings: Settings::default(),
            snippets: vec![Snippet {
                name: "update".into(),
                cmd: "apt update".into(),
                key: None,
            }],
        };

        let toml_str = toml::to_string_pretty(&original).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();

        assert_eq!(parsed.host.len(), 1);
        assert_eq!(parsed.host[0].name, "test");
        assert_eq!(parsed.host[0].host, "10.0.0.1");
        assert_eq!(parsed.host[0].port, 22);
        assert_eq!(parsed.host[0].user, "root");
        assert_eq!(parsed.host[0].jump.as_deref(), Some("jump-host"));
        assert_eq!(parsed.host[0].tags, vec!["prod"]);
        assert_eq!(parsed.host[0].group.as_deref(), Some("servers"));
        assert_eq!(parsed.host[0].keepalive, Some(60));
        match &parsed.host[0].identity[0] {
            Identity::Key { path, passphrase } => {
                assert_eq!(path, "~/.ssh/id_rsa");
                assert!(passphrase.is_none());
            }
            _ => panic!("expected Key identity"),
        }
        assert_eq!(parsed.snippets[0].name, "update");
        assert_eq!(parsed.snippets[0].cmd, "apt update");
    }

    #[test]
    fn identity_password_toml() {
        let toml_str = r#"
name = "srv"
host = "1.2.3.4"
user = "root"

[[identity]]
type = "password"
"#;
        let h: Host = toml::from_str(toml_str).unwrap();
        assert_eq!(h.identity.len(), 1);
        assert!(matches!(&h.identity[0], Identity::Password { .. }));
    }

    #[test]
    fn identity_agent_toml() {
        let toml_str = r#"
name = "srv"
host = "1.2.3.4"
user = "root"

[[identity]]
type = "agent"
"#;
        let h: Host = toml::from_str(toml_str).unwrap();
        assert_eq!(h.identity.len(), 1);
        assert!(matches!(&h.identity[0], Identity::Agent));
    }

    #[test]
    fn settings_toml_defaults() {
        let toml_str = r#"
[settings]
keepalive_secs = 60
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.settings.keepalive_secs, 60);
        assert_eq!(cfg.settings.term_cols, 120);
    }

    #[test]
    fn empty_config_toml() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.host.is_empty());
    }
}
