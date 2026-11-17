//! WARP WireGuard 隧道（用户态）
//!
//! UDP 承载 WireGuard 包，boringtun 负责握手与加解密；解出的 IP 包交给
//! 用户态网络栈（`netstack`），不需要 TUN、root 或系统路由
//!
//! 一个出口 = 一个 WARP 设备 + 一条隧道；多出口由 `pool` 组合

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use boringtun::noise::{Tunn, TunnResult};
use boringtun::x25519::{PublicKey, StaticSecret};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, Mutex};

use super::register::WarpRegistration;

const MAX_IP_PACKET: usize = 1280;
const UDP_BUFFER: usize = 2048;
const INBOUND_DEPTH: usize = 256;

/// 隧道与网络栈之间的 IP 包通道
#[derive(Clone)]
pub struct TunnelIo {
    inbound: Arc<Mutex<mpsc::Receiver<Vec<u8>>>>,
    outbound: mpsc::UnboundedSender<Vec<u8>>,
}

impl TunnelIo {
    pub fn max_ip_packet(&self) -> usize {
        MAX_IP_PACKET
    }

    pub async fn recv_ip(&self) -> Result<Vec<u8>, String> {
        let mut receiver = self.inbound.lock().await;
        receiver.recv().await.ok_or_else(|| "WARP 隧道已停止".to_string())
    }

    pub fn send_ip(&self, packet: &[u8]) {
        let _ = self.outbound.send(packet.to_vec());
    }
}

/// 建立一条隧道并启动收发包任务
pub async fn connect(registration: &WarpRegistration, endpoint: &str) -> Result<TunnelIo, String> {
    let private = decode_key(&registration.private_key, "私钥")?;
    let peer = decode_key(&registration.peer_public_key, "peer 公钥")?;
    let secret = StaticSecret::from(private);
    let peer_public = PublicKey::from(peer);
    let mut tunnel = Tunn::new(secret, peer_public, None, Some(25), 0, None);

    let bind = endpoint_bind(endpoint);
    let socket = UdpSocket::bind(bind)
        .await
        .map_err(|error| format!("绑定 WARP UDP 套接字失败: {error}"))?;
    socket
        .connect(endpoint)
        .await
        .map_err(|error| format!("连接 WARP 端点 {endpoint} 失败: {error}"))?;

    let (inbound_tx, inbound_rx) = mpsc::channel(INBOUND_DEPTH);
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // 主动发起握手；没有 session 时 encapsulate 会产出握手包
    let mut buffer = vec![0u8; UDP_BUFFER];
    let initial = tunnel.encapsulate(&[], &mut buffer);
    send_or_queue(&socket, initial, &inbound_tx).await;

    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        let mut recv_buffer = vec![0u8; UDP_BUFFER];
        loop {
            tokio::select! {
                received = socket.recv(&mut recv_buffer) => {
                    if let Ok(size) = received {
                        let result = tunnel.decapsulate(None, &recv_buffer[..size], &mut buffer);
                        send_or_queue(&socket, result, &inbound_tx).await;
                        // 握手完成后把建链前排队的 IP 包吐出来，否则最先发出的
                        // SYN 会一直留在 boringtun 的队列里
                        flush_queued(&socket, &mut tunnel, &mut buffer, &inbound_tx).await;
                    }
                }
                packet = outbound_rx.recv() => {
                    let Some(packet) = packet else { break };
                    let result = tunnel.encapsulate(&packet, &mut buffer);
                    send_or_queue(&socket, result, &inbound_tx).await;
                }
                _ = tick.tick() => {
                    let result = tunnel.update_timers(&mut buffer);
                    send_or_queue(&socket, result, &inbound_tx).await;
                    flush_queued(&socket, &mut tunnel, &mut buffer, &inbound_tx).await;
                }
            }
        }
    });

    Ok(TunnelIo {
        inbound: Arc::new(Mutex::new(inbound_rx)),
        outbound: outbound_tx,
    })
}

fn decode_key(value: &str, what: &str) -> Result<[u8; 32], String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value.trim())
        .map_err(|error| format!("解码 WARP {what} 失败: {error}"))?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| format!("WARP {what} 长度不是 32 字节"))
}

fn endpoint_bind(endpoint: &str) -> &'static str {
    // 端点带方括号或包含多个冒号视为 IPv6；否则用 IPv4
    if endpoint.starts_with('[') || endpoint.matches(':').count() > 1 {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    }
}

async fn send_or_queue(
    socket: &UdpSocket,
    result: TunnResult<'_>,
    inbound: &mpsc::Sender<Vec<u8>>,
) {
    match result {
        TunnResult::WriteToNetwork(data) => {
            let _ = socket.send(data).await;
        }
        TunnResult::WriteToTunnelV4(data, _) | TunnResult::WriteToTunnelV6(data, _) => {
            let _ = inbound.send(data.to_vec()).await;
        }
        TunnResult::Err(error) => {
            // 会话轮换期间的 ConnectionExpired 是正常现象（旧连接的海量重传），
            // 不写日志，避免把运行日志刷满
            let detail = format!("{error:?}");
            if detail != "ConnectionExpired" {
                crate::server::logging::verbose(
                    "[WARP]",
                    &format!("隧道加解密错误: {detail}"),
                );
            }
        }
        TunnResult::Done => {}
    }
}

/// 倾空 boringtun 在会话建立前缓存的 IP 包
async fn flush_queued(
    socket: &UdpSocket,
    tunnel: &mut Tunn,
    buffer: &mut [u8],
    inbound: &mpsc::Sender<Vec<u8>>,
) {
    for _ in 0..32 {
        let result = tunnel.decapsulate(None, &[], buffer);
        let done = matches!(result, TunnResult::Done | TunnResult::Err(_));
        send_or_queue(socket, result, inbound).await;
        if done {
            break;
        }
    }
}

/// 解析端点字符串中的 IP（用于日志与出口信息，不参与路由）
pub fn endpoint_ip(endpoint: &str) -> Option<IpAddr> {
    let host = endpoint.trim_start_matches('[').split(']').next().unwrap_or(endpoint);
    let host = if host.contains(']') { host } else { host.split(':').next().unwrap_or(host) };
    host.parse().ok()
}
