use crate::client::ClientHandler;
use crate::error::SshResult;
use russh::client::Handle;
use russh_sftp::client::SftpSession;
use std::path::Path;
use tokio::io::AsyncWriteExt;

pub struct RemoteFs {
    pub sftp: SftpSession,
}

pub struct RemoteEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<u64>,
    pub perms: Option<u32>,
}

impl RemoteFs {
    pub async fn open(handle: &Handle<ClientHandler>) -> SshResult<Self> {
        let ch = handle.channel_open_session().await?;
        ch.request_subsystem(true, "sftp").await?;
        let stream = ch.into_stream();
        let sftp = SftpSession::new(stream).await?;
        Ok(Self { sftp })
    }

    pub async fn list(&self, path: &str) -> SshResult<Vec<RemoteEntry>> {
        let mut entries = Vec::new();
        let mut rd = self.sftp.read_dir(path).await?;
        for e in rd.by_ref() {
            let meta = e.metadata();
            let name = e.file_name();
            entries.push(RemoteEntry {
                name,
                is_dir: meta.is_dir(),
                size: meta.size.unwrap_or(0),
                modified: meta.mtime.map(|t| t as u64),
                perms: meta.permissions,
            });
        }
        Ok(entries)
    }

    pub async fn is_dir(&self, path: &str) -> bool {
        match self.sftp.metadata(path).await {
            Ok(m) => m.is_dir(),
            Err(_) => false,
        }
    }

    pub async fn remove(&self, path: &str) -> SshResult<()> {
        if self.is_dir(path).await {
            self.sftp.remove_dir(path).await?;
        } else {
            self.sftp.remove_file(path).await?;
        }
        Ok(())
    }

    pub async fn mkdir(&self, path: &str) -> SshResult<()> {
        self.sftp.create_dir(path).await?;
        Ok(())
    }

    pub async fn rename(&self, from: &str, to: &str) -> SshResult<()> {
        self.sftp.rename(from, to).await?;
        Ok(())
    }

    pub async fn read_file(&self, path: &str) -> SshResult<Vec<u8>> {
        let data = self.sftp.read(path).await?;
        Ok(data)
    }

    pub async fn write_file(&self, path: &str, data: &[u8]) -> SshResult<()> {
        let mut file = self.sftp.create(path).await?;
        file.write_all(data).await?;
        Ok(())
    }

    pub async fn read_to_local(&self, remote: &str, local: &Path) -> SshResult<()> {
        let data = self.read_file(remote).await?;
        if let Some(parent) = local.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(local, &data).await?;
        Ok(())
    }

    pub async fn write_from_local(&self, local: &Path, remote: &str) -> SshResult<()> {
        let data = tokio::fs::read(local).await?;
        self.write_file(remote, &data).await?;
        Ok(())
    }
}
