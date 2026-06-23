use crate::error::{SshError, SshResult};
use russh::client::{Handle, Msg};
use russh::keys::key::{self, PublicKey};
use russh::{client, Channel, ChannelMsg, Pty};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex as AsyncMutex};

pub type RemoteForwards =
    Arc<AsyncMutex<HashMap<String, mpsc::UnboundedSender<russh::Channel<Msg>>>>>;

pub async fn load_agent_keys() -> Vec<key::KeyPair> {
    if std::env::var("SSH_AUTH_SOCK").is_err() {
        return vec![];
    }

    vec![]
}

pub type EventRx = mpsc::UnboundedReceiver<SshEvent>;
pub type EventTx = mpsc::UnboundedSender<SshEvent>;

#[derive(Debug, Clone)]
pub enum SshEvent {
    Connected,
    Disconnected(String),
    Data(Vec<u8>),
    Exit(i32),
    Error(String),
    Log(String),
}

#[derive(Debug, Clone)]
pub struct ConnectOpts {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: Vec<AuthChoice>,
    pub term_cols: u16,
    pub term_rows: u16,
    pub term_type: String,
    pub keepalive_secs: Option<u16>,
    pub jump: Vec<ConnectOpts>,
    pub use_agent: bool,
}

#[derive(Debug, Clone)]
pub enum AuthChoice {
    KeyFile {
        path: String,
        passphrase: Option<String>,
    },
    Password(String),
}

impl AuthChoice {
    pub fn label(&self) -> String {
        match self {
            Self::KeyFile { path, .. } => format!("key:{}", path),
            Self::Password(_) => "password".into(),
        }
    }
}

pub struct ClientHandler {
    pub tx: EventTx,
    pub remote_forwards: RemoteForwards,
}

#[async_trait::async_trait]
impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn auth_banner(
        &mut self,
        banner: &str,
        _session: &mut client::Session,
    ) -> std::result::Result<(), Self::Error> {
        let _ = self.tx.send(SshEvent::Data(banner.as_bytes().to_vec()));
        Ok(())
    }

    async fn check_server_key(
        &mut self,
        server_key: &PublicKey,
    ) -> std::result::Result<bool, Self::Error> {
        let fp = server_key.fingerprint();
        tracing::info!("server key: {}", fp);
        let _ = self.tx.send(SshEvent::Log(format!("server key: {}", fp)));
        Ok(true)
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<Msg>,
        connected_address: &str,
        connected_port: u32,
        originator_address: &str,
        originator_port: u32,
        _session: &mut client::Session,
    ) -> std::result::Result<(), Self::Error> {
        let key = format!("{}:{}", connected_address, connected_port);
        let map = self.remote_forwards.lock().await;
        if let Some(tx) = map.get(&key) {
            let _ = tx.send(channel);
        } else {
            tracing::warn!(
                "no handler for remote forward {} from {}:{}",
                key,
                originator_address,
                originator_port
            );
        }
        Ok(())
    }
}

pub fn build_config(keepalive: Option<u16>) -> SshResult<Arc<client::Config>> {
    let mut cfg = client::Config::default();
    if let Some(secs) = keepalive {
        if secs > 0 {
            let interval = std::time::Duration::from_secs(secs as u64);
            cfg.keepalive_interval = Some(interval);
            cfg.keepalive_max = 3;
        }
    }
    Ok(Arc::new(cfg))
}

pub fn load_key(path: &str, passphrase: Option<&str>) -> SshResult<key::KeyPair> {
    russh::keys::load_secret_key(path, passphrase).map_err(SshError::from)
}

pub async fn connect(
    opts: &ConnectOpts,
) -> SshResult<(Handle<ClientHandler>, EventRx, RemoteForwards)> {
    if !opts.jump.is_empty() {
        return connect_through_jump(opts).await;
    }
    let (tx, rx) = mpsc::unbounded_channel::<SshEvent>();
    let rf = Arc::new(AsyncMutex::new(HashMap::new()));
    let handler = ClientHandler {
        tx,
        remote_forwards: rf.clone(),
    };
    let cfg = build_config(opts.keepalive_secs)?;

    let mut session = client::connect(cfg, (opts.host.as_str(), opts.port), handler).await?;

    let agent_keys = if opts.use_agent {
        load_agent_keys().await
    } else {
        vec![]
    };
    let key_auth_ok = try_key_auth(&mut session, &opts.user, &opts.auth, &agent_keys).await?;
    if !key_auth_ok {
        return Err(SshError::Auth {
            user: opts.user.clone(),
            host: format!("{}:{}", opts.host, opts.port),
        });
    }

    Ok((session, rx, rf))
}

pub async fn connect_with_password<F>(
    opts: &ConnectOpts,
    mut ask_password: F,
) -> SshResult<(Handle<ClientHandler>, EventRx, RemoteForwards)>
where
    F: FnMut() -> Option<String>,
{
    if !opts.jump.is_empty() {
        return connect_through_jump(opts).await;
    }
    let (tx, rx) = mpsc::unbounded_channel::<SshEvent>();
    let rf = Arc::new(AsyncMutex::new(HashMap::new()));
    let handler = ClientHandler {
        tx,
        remote_forwards: rf.clone(),
    };
    let cfg = build_config(opts.keepalive_secs)?;

    let mut session = client::connect(cfg, (opts.host.as_str(), opts.port), handler).await?;

    let agent_keys = if opts.use_agent {
        load_agent_keys().await
    } else {
        vec![]
    };
    let key_auth_ok = try_key_auth(&mut session, &opts.user, &opts.auth, &agent_keys).await?;
    tracing::debug!(key_auth_ok, auth_count = opts.auth.len());
    if key_auth_ok {
        return Ok((session, rx, rf));
    }

    for choice in &opts.auth {
        if let AuthChoice::Password(pw) = choice {
            tracing::debug!("trying stored password");
            if let Ok(true) = session.authenticate_password(&opts.user, pw.clone()).await {
                return Ok((session, rx, rf.clone()));
            }
        }
    }

    if let Some(pw) = ask_password() {
        tracing::debug!(pw_len = pw.len(), "trying interactive password");
        match session.authenticate_password(&opts.user, pw).await {
            Ok(true) => {
                tracing::debug!("password auth ok");
                return Ok((session, rx, rf));
            }
            Ok(false) => {
                tracing::warn!("password rejected by server");
            }
            Err(e) => {
                tracing::error!(%e, "password auth error");
                return Err(e.into());
            }
        }
    }

    Err(SshError::Auth {
        user: opts.user.clone(),
        host: format!("{}:{}", opts.host, opts.port),
    })
}

async fn connect_through_jump(
    opts: &ConnectOpts,
) -> SshResult<(Handle<ClientHandler>, EventRx, RemoteForwards)> {
    let (tx, rx) = mpsc::unbounded_channel::<SshEvent>();
    let rf = Arc::new(AsyncMutex::new(HashMap::new()));
    let handler = ClientHandler {
        tx: tx.clone(),
        remote_forwards: rf.clone(),
    };
    let cfg = build_config(opts.keepalive_secs)?;

    let jump = &opts.jump[0];
    let jump_tx = mpsc::unbounded_channel::<SshEvent>().0;
    let mut jh = client::connect(
        cfg.clone(),
        (jump.host.as_str(), jump.port),
        ClientHandler {
            tx: jump_tx,
            remote_forwards: Arc::new(AsyncMutex::new(HashMap::new())),
        },
    )
    .await?;

    let jump_agent = if jump.use_agent {
        load_agent_keys().await
    } else {
        vec![]
    };
    let jump_key_ok = try_key_auth(&mut jh, &jump.user, &jump.auth, &jump_agent).await?;
    if !jump_key_ok {
        for choice in &jump.auth {
            if let AuthChoice::Password(pw) = choice {
                if jh
                    .authenticate_password(&jump.user, pw.clone())
                    .await
                    .unwrap_or(false)
                {
                    tracing::debug!("jump password auth ok");
                    break;
                }
            }
        }
    }

    let ch = jh
        .channel_open_direct_tcpip(opts.host.as_str(), opts.port as u32, "127.0.0.1", 0)
        .await
        .map_err(|e| SshError::Other(format!("jump channel: {}", e)))?;
    let stream = ch.into_stream();

    let mut session = client::connect_stream(cfg, stream, handler)
        .await
        .map_err(|e| SshError::Other(format!("jump target connect: {}", e)))?;

    let agent_keys = if opts.use_agent {
        load_agent_keys().await
    } else {
        vec![]
    };
    let key_auth_ok = try_key_auth(&mut session, &opts.user, &opts.auth, &agent_keys).await?;
    if !key_auth_ok {
        return Err(SshError::Auth {
            user: opts.user.clone(),
            host: format!("{}:{} via jump", opts.host, opts.port),
        });
    }

    Ok((session, rx, rf))
}

async fn try_key_auth(
    session: &mut Handle<ClientHandler>,
    user: &str,
    auth: &[AuthChoice],
    agent_keys: &[key::KeyPair],
) -> SshResult<bool> {
    let mut tried_any = false;

    for choice in auth {
        if let AuthChoice::KeyFile { path, passphrase } = choice {
            if path.is_empty() || path == "(none)" {
                continue;
            }
            tried_any = true;
            tracing::debug!(%path, "trying key");
            let key = match load_key(path, passphrase.as_deref()) {
                Ok(k) => k,
                Err(e) => {
                    tracing::debug!(%path, %e, "skip key");
                    continue;
                }
            };
            if session.authenticate_publickey(user, Arc::new(key)).await? {
                return Ok(true);
            }
        }
    }

    for key in agent_keys {
        tried_any = true;
        tracing::debug!("trying agent key");
        if session
            .authenticate_publickey(user, Arc::new(key.clone()))
            .await?
        {
            return Ok(true);
        }
    }

    Ok(tried_any)
}

pub async fn open_shell(
    session: &Handle<ClientHandler>,
    opts: &ConnectOpts,
) -> SshResult<Channel<Msg>> {
    let ch = session.channel_open_session().await?;
    let modes: &[(Pty, u32)] = &[];
    ch.request_pty(
        false,
        &opts.term_type,
        opts.term_cols as u32,
        opts.term_rows as u32,
        0,
        0,
        modes,
    )
    .await?;
    ch.request_shell(false).await?;
    Ok(ch)
}

pub async fn exec(handle: &Handle<ClientHandler>, command: &str) -> SshResult<String> {
    let mut ch = handle.channel_open_session().await?;
    ch.exec(true, command.as_bytes()).await?;
    let mut stdout = Vec::new();
    while let Some(msg) = ch.wait().await {
        match msg {
            ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
            ChannelMsg::ExtendedData { data, .. } => stdout.extend_from_slice(&data),
            ChannelMsg::ExitStatus { .. } | ChannelMsg::Eof | ChannelMsg::Close => break,
            _ => {}
        }
    }
    let output = String::from_utf8_lossy(&stdout).to_string();
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn build_config_default() {
        let cfg = build_config(None).unwrap();
        assert!(cfg.keepalive_interval.is_none());
    }

    #[test]
    fn build_config_with_keepalive() {
        let cfg = build_config(Some(30)).unwrap();
        assert_eq!(cfg.keepalive_interval, Some(Duration::from_secs(30)));
        assert_eq!(cfg.keepalive_max, 3);
    }

    #[test]
    fn build_config_zero_keepalive() {
        let cfg = build_config(Some(0)).unwrap();
        assert!(cfg.keepalive_interval.is_none());
    }

    #[test]
    fn auth_choice_label_key() {
        let a = AuthChoice::KeyFile {
            path: "/home/user/.ssh/id_rsa".into(),
            passphrase: None,
        };
        assert_eq!(a.label(), "key:/home/user/.ssh/id_rsa");
    }

    #[test]
    fn auth_choice_label_password() {
        let a = AuthChoice::Password("secret".into());
        assert_eq!(a.label(), "password");
    }

    #[test]
    fn connect_opts_defaults() {
        let opts = ConnectOpts {
            host: "10.0.0.1".into(),
            port: 22,
            user: "root".into(),
            auth: vec![],
            term_cols: 80,
            term_rows: 24,
            term_type: "xterm-256color".into(),
            keepalive_secs: None,
            jump: vec![],
            use_agent: false,
        };
        assert_eq!(opts.host, "10.0.0.1");
        assert_eq!(opts.port, 22);
        assert_eq!(opts.user, "root");
        assert!(opts.auth.is_empty());
        assert!(opts.jump.is_empty());
        assert!(!opts.use_agent);
    }

    #[test]
    fn ssh_error_display() {
        let err = SshError::Auth {
            user: "test".into(),
            host: "example.com:22".into(),
        };
        assert_eq!(format!("{}", err), "auth failed for test@example.com:22");
    }
}
