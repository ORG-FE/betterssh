use crate::client::ClientHandler;
use crate::error::SshResult;
use betterssh_core::{ForwardDirection, PortForward};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex as AsyncMutex};

type SshHandle = russh::client::Handle<ClientHandler>;
use russh::client::Msg;
type FwChannel = russh::Channel<Msg>;

pub struct ForwardEvent {
    pub id: u64,
    pub status: String,
}

pub async fn start_forward(
    handle: &Arc<AsyncMutex<SshHandle>>,
    remote_forwards: &Arc<AsyncMutex<HashMap<String, mpsc::UnboundedSender<FwChannel>>>>,
    fw: &PortForward,
    status_tx: mpsc::UnboundedSender<ForwardEvent>,
) -> SshResult<()> {
    match fw.direction {
        ForwardDirection::Local => start_local(handle, fw, status_tx).await,
        ForwardDirection::Remote => start_remote(handle, remote_forwards, fw, status_tx).await,
        ForwardDirection::Dynamic => start_dynamic(handle, fw, status_tx).await,
    }
}

async fn start_local(
    handle: &Arc<AsyncMutex<SshHandle>>,
    fw: &PortForward,
    status_tx: mpsc::UnboundedSender<ForwardEvent>,
) -> SshResult<()> {
    let addr = format!("{}:{}", fw.listen_addr, fw.listen_port);
    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|e| crate::error::SshError::Other(format!("forward bind {}: {}", addr, e)))?;

    let target_host = fw.target_host.clone();
    let target_port = fw.target_port;
    let id = fw.id;

    let _ = status_tx.send(ForwardEvent {
        id,
        status: format!("listening on {}", addr),
    });

    let handle = Arc::clone(handle);

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((conn, peer)) => {
                    let handle = Arc::clone(&handle);
                    let target_host = target_host.clone();
                    let peer_addr = peer.to_string();
                    tokio::spawn(async move {
                        tracing::debug!("forward {}: connection from {}", id, peer_addr);
                        let ch = {
                            let h = handle.lock().await;
                            h.channel_open_direct_tcpip(
                                &target_host,
                                target_port as u32,
                                &peer_addr,
                                0,
                            )
                            .await
                        };
                        match ch {
                            Ok(ch) => {
                                let mut ch_stream = ch.into_stream();
                                let mut tcp_stream = conn;
                                let _ = io::copy_bidirectional(&mut ch_stream, &mut tcp_stream)
                                    .await;
                            }
                            Err(e) => {
                                tracing::error!("forward {}: channel open: {:?}", id, e);
                            }
                        }
                    });
                }
                Err(e) => {
                    tracing::error!("forward {}: accept: {:?}", id, e);
                    let _ = status_tx.send(ForwardEvent {
                        id,
                        status: format!("error: {}", e),
                    });
                    break;
                }
            }
        }
    });

    Ok(())
}

async fn start_remote(
    handle: &Arc<AsyncMutex<SshHandle>>,
    remote_forwards: &Arc<AsyncMutex<HashMap<String, mpsc::UnboundedSender<FwChannel>>>>,
    fw: &PortForward,
    status_tx: mpsc::UnboundedSender<ForwardEvent>,
) -> SshResult<()> {
    let id = fw.id;
    let listen_addr = fw.listen_addr.clone();
    let listen_port = fw.listen_port;
    let target_host = fw.target_host.clone();
    let target_port = fw.target_port;

    
    {
        let mut h = handle.lock().await;
        h.tcpip_forward(&listen_addr, listen_port as u32)
            .await
            .map_err(|e| {
                crate::error::SshError::Other(format!(
                    "tcpip_forward {}:{}: {}",
                    listen_addr, listen_port, e
                ))
            })?;
    }

    let _ = status_tx.send(ForwardEvent {
        id,
        status: format!("remote {}:{} -> {}:{}", listen_addr, listen_port, target_host, target_port),
    });

    
    let key = format!("{}:{}", listen_addr, listen_port);
    let (incoming_tx, mut incoming_rx) = mpsc::unbounded_channel();

    {
        let mut map = remote_forwards.lock().await;
        map.insert(key.clone(), incoming_tx);
    }

    let _handle = Arc::clone(handle);

    tokio::spawn(async move {
        while let Some(ch) = incoming_rx.recv().await {
            let target_host = target_host.clone();
            tokio::spawn(async move {
                let mut ch_stream = ch.into_stream();
                
                let target_addr = format!("{}:{}", target_host, target_port);
                match TcpStream::connect(&target_addr).await {
                    Ok(tcp) => {
                        let mut tcp_stream = tcp;
                        let _ = io::copy_bidirectional(&mut ch_stream, &mut tcp_stream).await;
                    }
                    Err(e) => {
                        tracing::error!(
                            "remote forward {}: connect to {}: {}",
                            id, target_addr, e
                        );
                    }
                }
            });
        }
    });

    Ok(())
}

async fn start_dynamic(
    handle: &Arc<AsyncMutex<SshHandle>>,
    fw: &PortForward,
    status_tx: mpsc::UnboundedSender<ForwardEvent>,
) -> SshResult<()> {
    let addr = format!("{}:{}", fw.listen_addr, fw.listen_port);
    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|e| crate::error::SshError::Other(format!("socks bind {}: {}", addr, e)))?;

    let id = fw.id;

    let _ = status_tx.send(ForwardEvent {
        id,
        status: format!("SOCKS on {}", addr),
    });

    let handle = Arc::clone(handle);

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((conn, peer)) => {
                    let handle = Arc::clone(&handle);
                    tokio::spawn(async move {
                        tracing::debug!("dynamic {}: SOCKS from {}", id, peer.to_string());
                        if let Err(e) = handle_socks(conn, &handle).await {
                            tracing::error!("dynamic {}: socks error: {}", id, e);
                        }
                    });
                }
                Err(e) => {
                    tracing::error!("dynamic {}: accept: {:?}", id, e);
                    break;
                }
            }
        }
    });

    Ok(())
}

async fn handle_socks(
    mut conn: TcpStream,
    handle: &Arc<AsyncMutex<SshHandle>>,
) -> Result<(), Box<dyn std::error::Error>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut buf = [0u8; 2];
    conn.read_exact(&mut buf).await?;
    let n_methods = buf[1] as usize;
    let mut methods = vec![0u8; n_methods];
    conn.read_exact(&mut methods).await?;

    conn.write_all(&[5u8, 0]).await?;

    let mut hdr = [0u8; 4];
    conn.read_exact(&mut hdr).await?;

    let addr = match hdr[3] {
        1 => {
            let mut ip = [0u8; 4];
            conn.read_exact(&mut ip).await?;
            format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
        }
        3 => {
            let mut len = [0u8; 1];
            conn.read_exact(&mut len).await?;
            let mut domain = vec![0u8; len[0] as usize];
            conn.read_exact(&mut domain).await?;
            String::from_utf8_lossy(&domain).to_string()
        }
        4 => {
            let mut ip = [0u8; 16];
            conn.read_exact(&mut ip).await?;
            format!(
                "[{}]",
                ip.iter()
                    .map(|b| format!("{:02x}", b))
                    .collect::<Vec<_>>()
                    .join(":")
            )
        }
        _ => return Err("unsupported address type".into()),
    };

    let mut port_buf = [0u8; 2];
    conn.read_exact(&mut port_buf).await?;
    let port = u16::from_be_bytes(port_buf);

    let ch = {
        let h = handle.lock().await;
        h.channel_open_direct_tcpip(&addr, port as u32, "127.0.0.1", 0)
            .await
    };

    match ch {
        Ok(ch) => {
            conn.write_all(&[5u8, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            let mut ch_stream = ch.into_stream();
            let _ = io::copy_bidirectional(&mut ch_stream, &mut conn).await;
        }
        Err(e) => {
            tracing::error!("socks channel: {:?}", e);
            let _ = conn.write_all(&[5u8, 1, 0, 1, 0, 0, 0, 0, 0, 0]).await;
        }
    }

    Ok(())
}
