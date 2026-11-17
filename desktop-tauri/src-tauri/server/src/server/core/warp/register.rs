//! Cloudflare WARP 设备注册
//!
//! 流程与 opencode2api 的 registrar 一致：创建设备、启用 WARP、回读配置
//! 注册接口会限流，429 的 Retry-After 只记录成暂停窗口供池子排期，
//! 不在注册路径里长时间等待
//!
//! 一个出口对应一个 WARP 设备；池化时必须注册多个设备，注册本身自动完成，
//! 用户不需要手动创建账号

use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use serde::Deserialize;
use serde_json::json;
use x25519_dalek::{PublicKey, StaticSecret};

const API_BASE: &str = "https://api.cloudflareclient.com/v0i1909051800";
const DEFAULT_PEER: &str = "bmXOC+F1FxEMF9dyiK2H5/1SUtzH0JuVo51h2wPfgyo=";
const DEFAULT_PAUSE: Duration = Duration::from_secs(12);
const MAX_PAUSE: Duration = Duration::from_secs(5 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const WIREGUARD_PORT: u16 = 2408;

/// 注册接口的兜底端点（与 opencode2api 的 fallbackEndpoints 同源）
const FALLBACK_ENDPOINTS: &[&str] = &[
    "162.159.192.1:2408",
    "[2606:4700:d0::a29f:c001]:2408",
    "162.159.193.1:2408",
    "engage.cloudflareclient.com:2408",
];

/// 一个 WARP 设备的注册结果
#[derive(Clone, Debug)]
pub struct WarpRegistration {
    pub id: String,
    pub token: String,
    pub private_key: String,
    pub public_key: String,
    pub address_v4: String,
    pub address_v6: String,
    pub peer_public_key: String,
    pub endpoints: Vec<String>,
}

/// WARP 设备注册器，池子共享一份以复用限流窗口
pub struct WarpRegistrar {
    client: reqwest::Client,
    base: String,
    paused_until: Mutex<Option<Instant>>,
}

impl WarpRegistrar {
    pub fn new() -> Result<Self, String> {
        let client = reqwest::Client::builder()
            // 注册接口直连，不跟随系统代理（与 opencode2api 的 Proxy: nil 同义）
            .no_proxy()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| format!("创建 WARP 注册客户端失败: {error}"))?;
        Ok(Self {
            client,
            base: API_BASE.to_string(),
            paused_until: Mutex::new(None),
        })
    }

    /// 距离下一次允许注册还要等多久（None = 现在就可以）
    pub fn paused_for(&self) -> Option<Duration> {
        let guard = match self.paused_until.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.and_then(|until| until.checked_duration_since(Instant::now()))
    }

    fn pause(&self, wait: Duration) {
        if wait.is_zero() {
            return;
        }
        let wait = wait.min(MAX_PAUSE);
        let mut guard = match self.paused_until.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let until = Instant::now() + wait;
        match guard.as_ref() {
            Some(current) if until <= *current => {}
            _ => *guard = Some(until),
        }
    }

    /// 注册并启用一个 WARP 设备
    pub async fn register(&self) -> Result<WarpRegistration, String> {        if let Some(wait) = self.paused_for() {
            return Err(format!(
                "Cloudflare WARP 注册限流中，{} 秒后可重试",
                wait.as_secs()
            ));
        }
        let (private_key, public_key) = wireguard_keypair()?;
        let body = json!({
            "key": public_key,
            "install_id": "",
            "fcm_token": "",
            "tos": "2021-09-14T17:00:00.000+00:00",
            "model": "PC",
            "serial_number": "",
            "locale": "en_US",
        });
        let (status, retry_after, text) =
            self.send(reqwest::Method::POST, "/reg", Some(&body), None).await?;
        if status == 429 {
            self.pause(retry_after.and_then(|value| parse_retry_after(&value)).unwrap_or(DEFAULT_PAUSE));
            return Err(format!("创建 WARP 设备被限流（HTTP 429）：{text}"));
        }
        if !(200..300).contains(&status) {
            return Err(format!("创建 WARP 设备失败（HTTP {status}）：{text}"));
        }
        let created: DeviceEnvelope = decode(&text)?;
        if created.result.id.is_empty() || created.result.token.is_empty() {
            return Err("创建 WARP 设备失败：响应缺少 id 或 token".to_string());
        }

        let enabled = json!({ "warp_enabled": true });
        let (status, _, text) = self
            .send(
                reqwest::Method::PATCH,
                &format!("/reg/{}", created.result.id),
                Some(&enabled),
                Some(&created.result.token),
            )
            .await?;
        if !(200..300).contains(&status) {
            return Err(format!("启用 WARP 设备失败（HTTP {status}）：{text}"));
        }

        let (status, _, text) = self
            .send(
                reqwest::Method::GET,
                &format!("/reg/{}", created.result.id),
                None,
                Some(&created.result.token),
            )
            .await?;
        if !(200..300).contains(&status) {
            return Err(format!("读取 WARP 设备失败（HTTP {status}）：{text}"));
        }
        let current: DeviceEnvelope = decode(&text)?;
        if !current.result.warp_enabled {
            return Err("读取 WARP 设备失败：WARP 未启用".to_string());
        }
        let addresses = current.result.config.interface.addresses;
        if addresses.v4.is_empty() && addresses.v6.is_empty() {
            return Err("读取 WARP 设备失败：配置缺少 v4 与 v6 地址".to_string());
        }
        let (peer_public_key, endpoints) = endpoints_from_peers(&current.result.config.peers);
        Ok(WarpRegistration {
            id: created.result.id,
            token: created.result.token,
            private_key,
            public_key,
            address_v4: addresses.v4,
            address_v6: addresses.v6,
            peer_public_key,
            endpoints,
        })
    }

    /// 注销设备；替换出口或设备失效时调用，失败只记日志
    pub async fn unregister(&self, registration: &WarpRegistration) {
        let result = self
            .send(
                reqwest::Method::DELETE,
                &format!("/reg/{}", registration.id),
                None,
                Some(&registration.token),
            )
            .await;
        if let Err(error) = result {
            crate::server::logging::verbose(
                "[WARP]",
                &format!("注销 WARP 设备失败（忽略）：{error}"),
            );
        }
    }

    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&serde_json::Value>,
        token: Option<&str>,
    ) -> Result<(u16, Option<String>, String), String> {
        let url = format!("{}{path}", self.base.trim_end_matches('/'));
        let mut request = self
            .client
            .request(method, &url)
            .header("Content-Type", "application/json")
            .header("CF-Client-Version", "a-6.10-2158")
            .header("User-Agent", "okhttp/3.12.1");
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        if let Some(body) = body {
            let payload = serde_json::to_vec(body)
                .map_err(|error| format!("序列化 WARP 注册请求失败: {error}"))?;
            request = request.body(payload);
        }
        let response = request
            .send()
            .await
            .map_err(|error| format!("请求 Cloudflare WARP 接口失败: {error}"))?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let text = response
            .text()
            .await
            .map_err(|error| format!("读取 Cloudflare WARP 响应失败: {error}"))?;
        Ok((status, retry_after, text))
    }
}

fn decode(text: &str) -> Result<DeviceEnvelope, String> {
    serde_json::from_str(text).map_err(|error| format!("解析 Cloudflare WARP 响应失败: {error}"))
}

/// 生成一对 WireGuard 密钥（X25519，base64 标准编码）
fn wireguard_keypair() -> Result<(String, String), String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|error| format!("生成 WARP 设备私钥失败: {error}"))?;
    let secret = StaticSecret::from(bytes);
    let public = PublicKey::from(&secret);
    let engine = base64::engine::general_purpose::STANDARD;
    Ok((engine.encode(secret.as_bytes()), engine.encode(public.as_bytes())))
}

fn parse_retry_after(value: &str) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    value.parse::<u64>().ok().filter(|seconds| *seconds > 0).map(Duration::from_secs)
}

/// 从 peer 配置构造出口端点列表（peer 的 v4 / v6 / host 与端口逐一组合）
fn endpoints_from_peers(peers: &[DevicePeer]) -> (String, Vec<String>) {
    let mut endpoints: Vec<String> = Vec::new();
    let mut peer_public_key = String::new();
    if let Some(peer) = peers.first() {
        peer_public_key = peer.public_key.trim().to_string();
        let mut ports = vec![WIREGUARD_PORT];
        for port in &peer.endpoint.ports {
            if let Ok(port) = u16::try_from(*port) {
                if port != WIREGUARD_PORT {
                    ports.push(port);
                }
            }
        }
        for base in [
            peer.endpoint.v4.as_str(),
            peer.endpoint.v6.as_str(),
            peer.endpoint.host.as_str(),
        ] {
            let base = base.trim();
            if base.is_empty() {
                continue;
            }
            for port in &ports {
                if let Some(endpoint) = replace_endpoint_port(base, *port) {
                    push_unique(&mut endpoints, endpoint);
                }
            }
            push_unique(&mut endpoints, base.to_string());
        }
    }
    if peer_public_key.is_empty() {
        peer_public_key = DEFAULT_PEER.to_string();
    }
    for fallback in FALLBACK_ENDPOINTS {
        push_unique(&mut endpoints, (*fallback).to_string());
    }
    (peer_public_key, endpoints)
}

fn push_unique(items: &mut Vec<String>, value: String) {
    if !value.is_empty() && !items.iter().any(|item| item == &value) {
        items.push(value);
    }
}

/// 把端点地址的端口替换成 `port`（裸 v6 地址补方括号）
fn replace_endpoint_port(endpoint: &str, port: u16) -> Option<String> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return None;
    }
    if let Some(rest) = endpoint.strip_prefix('[') {
        if let Some((host, _)) = rest.split_once(']') {
            return Some(format!("[{host}]:{port}"));
        }
    }
    let colons = endpoint.matches(':').count();
    if colons == 1 {
        if let Some((host, tail)) = endpoint.rsplit_once(':') {
            if tail.parse::<u16>().is_ok() {
                return Some(format!("{host}:{port}"));
            }
        }
    }
    if colons > 1 {
        return Some(format!("[{endpoint}]:{port}"));
    }
    Some(format!("{endpoint}:{port}"))
}

#[derive(Deserialize)]
struct DeviceEnvelope {
    #[serde(default)]
    result: DeviceResult,
}

#[derive(Default, Deserialize)]
struct DeviceResult {
    #[serde(default)]
    id: String,
    #[serde(default)]
    token: String,
    #[serde(default)]
    warp_enabled: bool,
    #[serde(default)]
    config: DeviceConfig,
}

#[derive(Default, Deserialize)]
struct DeviceConfig {
    #[serde(default)]
    interface: DeviceInterface,
    #[serde(default)]
    peers: Vec<DevicePeer>,
}

#[derive(Default, Deserialize)]
struct DeviceInterface {
    #[serde(default)]
    addresses: DeviceAddresses,
}

#[derive(Default, Deserialize)]
struct DeviceAddresses {
    #[serde(default)]
    v4: String,
    #[serde(default)]
    v6: String,
}

#[derive(Default, Deserialize)]
struct DevicePeer {
    #[serde(default)]
    public_key: String,
    #[serde(default)]
    endpoint: DeviceEndpoint,
}

#[derive(Default, Deserialize)]
struct DeviceEndpoint {
    #[serde(default)]
    v4: String,
    #[serde(default)]
    v6: String,
    #[serde(default)]
    host: String,
    #[serde(default)]
    ports: Vec<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keypair_is_base64_encoded_32_bytes() {
        let (private, public) = wireguard_keypair().expect("keypair");
        let engine = base64::engine::general_purpose::STANDARD;
        assert_eq!(engine.decode(private).expect("private").len(), 32);
        assert_eq!(engine.decode(public).expect("public").len(), 32);
    }

    #[test]
    fn endpoint_ports_are_normalized() {
        assert_eq!(replace_endpoint_port("162.159.192.1", 2408).as_deref(), Some("162.159.192.1:2408"));
        assert_eq!(replace_endpoint_port("162.159.192.1:500", 2408).as_deref(), Some("162.159.192.1:2408"));
        assert_eq!(
            replace_endpoint_port("2606:4700:d0::a29f:c001", 2408).as_deref(),
            Some("[2606:4700:d0::a29f:c001]:2408")
        );
        assert_eq!(
            replace_endpoint_port("[2606:4700:d0::a29f:c001]:500", 2408).as_deref(),
            Some("[2606:4700:d0::a29f:c001]:2408")
        );
    }

    #[test]
    fn parses_device_payload() {
        let text = r#"{
          "result": {
            "id": "device-1",
            "token": "token-1",
            "warp_enabled": true,
            "config": {
              "interface": { "addresses": { "v4": "172.16.0.2", "v6": "2606:4700:110:8a36::1" } },
              "peers": [{
                "public_key": "peer-key",
                "endpoint": { "v4": "162.159.192.1", "v6": "2606:4700:d0::a29f:c001", "host": "", "ports": [2408, 500] }
              }]
            }
          }
        }"#;
        let envelope: DeviceEnvelope = decode(text).expect("decode");
        assert_eq!(envelope.result.id, "device-1");
        assert!(envelope.result.warp_enabled);
        assert_eq!(envelope.result.config.interface.addresses.v4, "172.16.0.2");
        let (peer, endpoints) = endpoints_from_peers(&envelope.result.config.peers);
        assert_eq!(peer, "peer-key");
        assert!(endpoints.contains(&"162.159.192.1:2408".to_string()));
        assert!(endpoints.contains(&"[2606:4700:d0::a29f:c001]:2408".to_string()));
    }
}
