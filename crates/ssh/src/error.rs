use thiserror::Error;

#[derive(Debug, Error)]
pub enum SshError {
    #[error("ssh: {0}")]
    Ssh(#[from] russh::Error),

    #[error("keys: {0}")]
    Keys(#[from] russh_keys::Error),

    #[error("sftp: {0}")]
    Sftp(#[from] russh_sftp::client::error::Error),

    #[error("auth failed for {user}@{host}")]
    Auth { user: String, host: String },

    #[error("key auth tried but failed, need password")]
    NeedPassword,

    #[error("channel: {0}")]
    Channel(String),

    #[error("not connected")]
    NotConnected,

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

pub type SshResult<T> = std::result::Result<T, SshError>;
