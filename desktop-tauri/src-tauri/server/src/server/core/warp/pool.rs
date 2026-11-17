//! WARP 出口池
//!
//! 每个出口注册一个 WARP 设备、建立一条用户态隧道、启动一个本地 SOCKS5
//! 监听端口；网关按请求轮询出口。出口注册、隧道与网络栈全部在主进程内完成，
//! 不启动子进程
//!
//! 设备只在本次服务运行期间复用，不落盘：WARP 设备与设备密钥绑定当前环境，
//! 跨平台或跨设备搬移没有意义，也不应进入 WebDAV 备份与同步。服务重启后
//! 重新注册；遇到上游 429 时优先轮换到下一个出口，出口不足时重新注册新设备

use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::net::TcpListener;

use crate::server::core::proxies::ResolvedProxy;
use crate::server::logging;

use super::register::{WarpRegistration, WarpRegistrar};
use super::{netstack, socks, tunnel};

const DEFAULT_POOL_SIZE: usize = 2;
const MAX_POOL_SIZE: usize = 32;
const BUILD_RETRY: usize = 2;

/// 一个可用的 WARP 出口
#[derive(Clone)]
pub struct WarpExit {
    pub port: u16,
    egress_ip: Arc<Mutex<String>>,
    pub proxy: ResolvedProxy,
}

impl WarpExit {
    /// 出口 IP（启动时的 trace 探测；未完成或失败时为空串）
    pub fn egress_ip(&self) -> String {
        self.egress_ip
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default()
    }
}

/// 出口池：按请求轮询出口
pub struct WarpPool {
    exits: Mutex<Vec<Option<WarpExit>>>,
    cursor: AtomicUsize,
    registrar: Arc<WarpRegistrar>,
}

impl WarpPool {
    pub fn len(&self) -> usize {
        self.exits
            .lock()
            .map(|slots| slots.iter().filter(|slot| slot.is_some()).count())
            .unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 取下一个出口（轮询；跳过正在重建的槽位）
    pub fn pick(&self) -> Option<WarpExit> {
        let Ok(slots) = self.exits.lock() else {
            return None;
        };
        let count = slots.len();
        if count == 0 {
            return None;
        }
        for _ in 0..count {
            let index = self.cursor.fetch_add(1, Ordering::Relaxed) % count;
            if let Some(exit) = slots[index].clone() {
                return Some(exit);
            }
        }
        None
    }

    /// 上游 429 且池内出口不足时，注册一台新设备并追加到池中
    ///
    /// 保留现有出口：新设备注册失败时旧出口仍可用，不会把池子清空
    pub async fn add_exit(self: &Arc<Self>) -> Option<WarpExit> {
        let index = {
            let Ok(slots) = self.exits.lock() else {
                return None;
            };
            if slots.len() >= MAX_POOL_SIZE {
                return None;
            }
            slots.len()
        };
        match build_exit(&self.registrar, index).await {
            Ok(exit) => {
                if let Ok(mut slots) = self.exits.lock() {
                    if slots.len() < MAX_POOL_SIZE {
                        slots.push(Some(exit.clone()));
                    }
                }
                Some(exit)
            }
            Err(error) => {
                logging::log(
                    "[WARP]",
                    &format!("注册新的 WARP 出口失败：{error}"),
                );
                None
            }
        }
    }

    /// 后台重建一个槽位（隧道异常或替换失败时调用）
    pub fn rebuild(self: &Arc<Self>, index: usize) {
        let pool = Arc::clone(self);
        crate::spawn_task(async move {
            let mut delay = Duration::from_secs(10);
            for _ in 0..BUILD_RETRY {
                match build_exit(&pool.registrar, index).await {
                    Ok(exit) => {
                        if let Ok(mut slots) = pool.exits.lock() {
                            if let Some(slot) = slots.get_mut(index) {
                                *slot = Some(exit);
                            }
                        }
                        logging::log("[WARP]", &format!("WARP 出口 {} 已重建", index + 1));
                        return;
                    }
                    Err(error) => {
                        logging::log(
                            "[WARP]",
                            &format!("WARP 出口 {} 重建失败：{error}", index + 1),
                        );
                        tokio::time::sleep(delay).await;
                        delay = (delay * 2).min(Duration::from_secs(120));
                    }
                }
            }
        });
    }
}

static POOL: OnceLock<Arc<WarpPool>> = OnceLock::new();

/// 已初始化的出口池（None = 尚未完成初始化或初始化失败）
pub fn global() -> Option<Arc<WarpPool>> {
    POOL.get().cloned()
}

/// 后台初始化出口池（只执行一次；失败按指数退避重试）
pub fn spawn() {
    if POOL.get().is_some() {
        return;
    }
    crate::spawn_task(async move {
        let size = std::env::var("ZCODE_WARP_POOL_SIZE")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(DEFAULT_POOL_SIZE)
            .clamp(1, MAX_POOL_SIZE);
        let mut delay = Duration::from_secs(30);
        loop {
            match start(size).await {
                Ok(pool) => {
                    let count = pool.len();
                    let _ = POOL.set(pool);
                    logging::log(
                        "[WARP]",
                        &format!("WARP 出口池已就绪，共 {count} 个出口"),
                    );
                    return;
                }
                Err(error) => {
                    logging::log(
                        "[WARP]",
                        &format!(
                            "WARP 出口池初始化失败，{} 秒后重试：{error}",
                            delay.as_secs()
                        ),
                    );
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(300));
                }
            }
        }
    });
}

async fn start(count: usize) -> Result<Arc<WarpPool>, String> {
    let registrar = Arc::new(WarpRegistrar::new()?);
    // 第一个出口同步建立，成功后立即发布池子；其余出口后台补齐，
    // 避免某个设备注册或探测失败把整池启动拖长
    let first = build_exit(&registrar, 0).await?;
    let mut slots = vec![None; count];
    slots[0] = Some(first);
    let pool = Arc::new(WarpPool {
        exits: Mutex::new(slots),
        cursor: AtomicUsize::new(0),
        registrar,
    });

    for index in 1..count {
        let pool_weak = Arc::downgrade(&pool);
        let registrar = Arc::clone(&pool.registrar);
        crate::spawn_task(async move {
            let mut delay = Duration::from_secs(10);
            for _ in 0..BUILD_RETRY {
                match build_exit(&registrar, index).await {
                    Ok(exit) => {
                        if let Some(pool) = pool_weak.upgrade() {
                            if let Ok(mut slots) = pool.exits.lock() {
                                if let Some(slot) = slots.get_mut(index) {
                                    *slot = Some(exit);
                                }
                            }
                        }
                        return;
                    }
                    Err(error) => {
                        logging::log(
                            "[WARP]",
                            &format!("WARP 出口 {} 补齐失败：{error}", index + 1),
                        );
                        tokio::time::sleep(delay).await;
                        delay = (delay * 2).min(Duration::from_secs(120));
                    }
                }
            }
        });
    }

    Ok(pool)
}

async fn build_exit(registrar: &WarpRegistrar, index: usize) -> Result<WarpExit, String> {
    let mut registration = registrar.register().await?;
    let mut last_error = String::new();
    for attempt in 0..BUILD_RETRY {
        match build_tunnel(&registration, index).await {
            Ok(exit) => return Ok(exit),
            Err(error) => {
                last_error = error;
                if attempt + 1 == BUILD_RETRY {
                    break;
                }
                // 当前设备或端点不可用：注销后换一台新设备重试
                registrar.unregister(&registration).await;
                registration = registrar.register().await?;
            }
        }
    }
    Err(last_error)
}

async fn build_tunnel(registration: &WarpRegistration, index: usize) -> Result<WarpExit, String> {
    let assigned_v4 = parse_v4(&registration.address_v4)
        .ok_or_else(|| format!("WARP 注册结果缺少可用的 IPv4 地址：{}", registration.address_v4))?;
    let assigned_v6 = parse_v6(&registration.address_v6);
    let mut last_error = String::new();
    for endpoint in endpoint_order(registration) {
        match connect_endpoint(registration, &endpoint, assigned_v4, assigned_v6, index).await {
            Ok(exit) => return Ok(exit),
            Err(error) => {
                last_error = format!("{endpoint}: {error}");
                // 单个端点失败是选路过程，不是运行错误；只写详细日志，
                // 避免管理页把每个备用端点都报成错误
                logging::verbose(
                    "[WARP]",
                    &format!("WARP 出口 {} 端点不可用：{last_error}", index + 1),
                );
            }
        }
    }
    Err(last_error)
}

async fn connect_endpoint(
    registration: &WarpRegistration,
    endpoint: &str,
    assigned_v4: Ipv4Addr,
    assigned_v6: Option<Ipv6Addr>,
    index: usize,
) -> Result<WarpExit, String> {
    let io = tunnel::connect(registration, endpoint).await?;
    let net = netstack::spawn(io, assigned_v4, assigned_v6);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|error| format!("绑定本地 SOCKS5 端口失败: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("读取本地 SOCKS5 端口失败: {error}"))?
        .port();
    tokio::spawn(socks::serve(listener, net));

    let egress_ip = Arc::new(Mutex::new(String::new()));
    let mut probe_error = None;
    // 首次探测可能撞上 netstack 与 SOCKS 监听尚未就绪，短暂等待后重试
    for _ in 0..2 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        match probe_egress(port).await {
            Ok(ip) => {
                if let Ok(mut value) = egress_ip.lock() {
                    *value = ip;
                }
                probe_error = None;
                break;
            }
            Err(error) => probe_error = Some(error),
        }
    }
    if let Some(error) = probe_error {
        return Err(format!("出口探测失败: {error}"));
    }

    let ip = egress_ip.lock().map(|value| value.clone()).unwrap_or_default();
    logging::log(
        "[WARP]",
        &format!("WARP 出口 {} 本地 SOCKS5 127.0.0.1:{port}，出口 IP={ip}", index + 1),
    );

    let proxy = ResolvedProxy {
        source: "warp".to_string(),
        protocol: "socks5".to_string(),
        host: "127.0.0.1".to_string(),
        port: Some(port),
        username: String::new(),
        password: String::new(),
        label: format!("WARP 出口 {}", index + 1),
        // WARP 出口是本地 SOCKS5，走 chain::endpoint 的直通分支；
        // 标成 non_direct 会被当作需要系统代理中转的代理链，容器里会直接失败
        non_direct: false,
    };
    Ok(WarpExit {
        port,
        egress_ip,
        proxy,
    })
}

/// 端点顺序：默认 IPv4 优先，`ZCODE_WARP_IPV6=1` 时 IPv6 优先
///
/// WARP 的 IPv6 端点并非所有家宽都可达；默认先用已验证可用的 IPv4，
/// 需要 IPv6 出口区分度时再显式开启。每个协议最多保留两个端点
fn endpoint_order(registration: &WarpRegistration) -> Vec<String> {
    let prefer_ipv6 = std::env::var("ZCODE_WARP_IPV6")
        .map(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes"))
        .unwrap_or(false);
    let ipv4_preferred = [
        "162.159.192.1:2408",
        "162.159.193.1:2408",
        "engage.cloudflareclient.com:2408",
    ];
    let ipv6_preferred = ["[2606:4700:d0::a29f:c001]:2408"];
    let mut ordered: Vec<String> = Vec::new();
    // 先试注册结果里的标准端点：实测已知固定端点（.1 / .193）在本网络
    // 经常超时，注册端点通常更快可用；IPv6 端点仅在显式开启时加入
    for endpoint in &registration.endpoints {
        let Ok(addr) = endpoint.parse::<std::net::SocketAddr>() else {
            continue;
        };
        if addr.port() != 2408 {
            continue;
        }
        if addr.is_ipv6() && !prefer_ipv6 {
            continue;
        }
        push_unique(&mut ordered, endpoint);
    }
    let preferred: Vec<&str> = if prefer_ipv6 {
        ipv6_preferred.iter().chain(ipv4_preferred.iter()).copied().collect()
    } else {
        ipv4_preferred.iter().copied().collect()
    };
    for endpoint in preferred {
        push_unique(&mut ordered, endpoint);
    }
    ordered
}

fn push_unique(items: &mut Vec<String>, endpoint: &str) {
    let endpoint = endpoint.trim();
    if !endpoint.is_empty() && !items.iter().any(|item| item == endpoint) {
        items.push(endpoint.to_string());
    }
}

fn parse_v4(value: &str) -> Option<Ipv4Addr> {
    value.split('/').next()?.trim().parse().ok()
}

fn parse_v6(value: &str) -> Option<Ipv6Addr> {
    value.split('/').next()?.trim().parse().ok()
}

/// 用出口的本地 SOCKS5 端口请求 Cloudflare trace，读取实际出口 IP
async fn probe_egress(port: u16) -> Result<String, String> {
    let proxy = reqwest::Proxy::all(format!("socks5h://127.0.0.1:{port}"))
        .map_err(|error| format!("构造出口探测代理失败: {error}"))?;
    let client = reqwest::Client::builder()
        .proxy(proxy)
        .timeout(Duration::from_secs(4))
        .build()
        .map_err(|error| format!("创建出口探测客户端失败: {error}"))?;
    let text = client
        .get("https://www.cloudflare.com/cdn-cgi/trace")
        .send()
        .await
        .map_err(|error| format!("出口探测请求失败: {error}"))?
        .text()
        .await
        .map_err(|error| format!("读取出口探测响应失败: {error}"))?;
    text.lines()
        .find_map(|line| line.strip_prefix("ip="))
        .map(|ip| ip.trim().to_string())
        .filter(|ip| !ip.is_empty())
        .ok_or_else(|| "出口探测响应缺少 ip 字段".to_string())
}
