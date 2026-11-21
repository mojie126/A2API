//! WARP 出口池
//!
//! 每个出口注册一个 WARP 设备、建立一条用户态隧道、启动一个本地 SOCKS5
//! 监听端口；网关按请求轮询出口。出口注册、隧道与网络栈全部在主进程内完成，
//! 不启动子进程
//!
//! 选路侧默认直连优先：只有直连出口已被占用、账号处于 WARP 临时绑定期或
//! 请求刚吃到 429 时才使用出口。需要 WARP 时按账号稳定落到同一个出口
//! （见 `pick_for`），并在短时间内保持这份绑定
//!
//! 设备只在本次服务运行期间复用，不落盘：WARP 设备与设备密钥绑定当前环境，
//! 跨平台或跨设备搬移没有意义，也不应进入 WebDAV 备份与同步。服务重启后
//! 重新注册；遇到上游 429 时优先轮换到下一个出口，出口不足时重新注册新设备

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::Notify;

use crate::server::core::proxies::ResolvedProxy;
use crate::server::logging;

use super::register::{WarpRegistration, WarpRegistrar};
use super::{netstack, settings, socks, tunnel};

const BUILD_RETRY: usize = 2;
/// 账号与 WARP 出口的临时绑定时间：会话粘滞期间同一账号始终走同一出口
const BIND_TTL_MS: i64 = 10 * 60 * 1000;

/// 清掉已过期的账号绑定（调用方持绑定表的锁）
fn prune_bindings(bindings: &mut HashMap<String, i64>, now: i64) {
    bindings.retain(|_, until| *until > now);
}

/// 单个出口的使用统计
///
/// 计数只在本进程内累计，设备重建后归零：重建换的是另一台设备，
/// 把它与新设备的读数混在一起没有意义
#[derive(Default)]
pub struct ExitStats {
    /// 被选路分配的请求数（每次 `pick` 记一次）
    assigned: AtomicU64,
    /// 当前在途的 SOCKS 连接数
    active: AtomicU64,
    /// 累计建立的 SOCKS 连接数
    connections: AtomicU64,
    /// 累计上传字节（客户端到上游）
    upload: AtomicU64,
    /// 累计下载字节（上游到客户端）
    download: AtomicU64,
    /// 最近一次被分配的毫秒时间戳（0 = 尚未使用）
    last_used_ms: AtomicU64,
}

impl ExitStats {
    fn on_assigned(&self) {
        self.assigned.fetch_add(1, Ordering::Relaxed);
        self.last_used_ms.store(logging::now_ms() as u64, Ordering::Relaxed);
    }

    /// 开一条连接：返回的守卫在连接结束时把在途计数减回去
    pub fn begin_connection(self: &Arc<Self>) -> ActiveGuard {
        self.connections.fetch_add(1, Ordering::Relaxed);
        self.active.fetch_add(1, Ordering::Relaxed);
        ActiveGuard { stats: Arc::clone(self) }
    }

    pub fn add_upload(&self, bytes: usize) {
        self.upload.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub fn add_download(&self, bytes: usize) {
        self.download.fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

/// 在途连接的守卫，Drop 时把在途计数减一
pub struct ActiveGuard {
    stats: Arc<ExitStats>,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.stats.active.fetch_sub(1, Ordering::Relaxed);
    }
}

/// 一个可用的 WARP 出口
#[derive(Clone)]
pub struct WarpExit {
    /// WARP 设备标识（注册接口返回的 id，管理页用它定位设备）
    pub id: String,
    pub port: u16,
    egress_ip: Arc<Mutex<String>>,
    pub proxy: ResolvedProxy,
    stats: Arc<ExitStats>,
    created_at: u64,
    /// 注册信息：删除出口时要按它注销设备
    ///
    /// 用 `Arc` 共享：出口在每次选路时都会克隆，注册信息里含私钥与令牌，
    /// 不该随请求复制
    registration: Arc<WarpRegistration>,
    /// 停止信号：删除出口时让本地 SOCKS5 服务退出并释放端口
    stop: Arc<Notify>,
}

impl WarpExit {
    /// 出口 IP（启动时的 trace 探测；未完成或失败时为空串）
    pub fn egress_ip(&self) -> String {
        self.egress_ip
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default()
    }

    /// 管理页展示用的一份读数（不含私钥与令牌）
    fn describe(&self, index: usize) -> Value {
        json!({
            "id": self.id,
            "index": index + 1,
            "label": format!("WARP 出口 {}", index + 1),
            "egressIp": self.egress_ip(),
            "port": self.port,
            "endpoint": format!("socks5://127.0.0.1:{}", self.port),
            "createdAt": self.created_at,
            "assigned": self.stats.assigned.load(Ordering::Relaxed),
            "active": self.stats.active.load(Ordering::Relaxed),
            "connections": self.stats.connections.load(Ordering::Relaxed),
            "uploadBytes": self.stats.upload.load(Ordering::Relaxed),
            "downloadBytes": self.stats.download.load(Ordering::Relaxed),
            "lastUsedAt": self.stats.last_used_ms.load(Ordering::Relaxed),
        })
    }
}

/// 出口池：按请求轮询出口
pub struct WarpPool {
    exits: Mutex<Vec<Option<WarpExit>>>,
    cursor: AtomicUsize,
    registrar: Arc<WarpRegistrar>,
    /// 账号 → 绑定到期时刻（账号一旦被分配到 WARP 出口，短时间内继续走 WARP）
    bindings: Mutex<HashMap<String, i64>>,
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
                exit.stats.on_assigned();
                return Some(exit);
            }
        }
        None
    }

    /// 按账号稳定选择一个出口：同一账号在池子不变时始终落到同一个出口，
    /// 与账号的会话粘滞配合形成「暂时绑定」；设备重建或增删后会自然重新分配
    pub fn pick_for(&self, account_id: &str) -> Option<WarpExit> {
        if account_id.is_empty() {
            return self.pick();
        }
        let Ok(slots) = self.exits.lock() else {
            return None;
        };
        let count = slots.len();
        if count == 0 {
            return None;
        }
        let mut hash = 0usize;
        for byte in account_id.bytes() {
            hash = hash.wrapping_mul(31).wrapping_add(byte as usize);
        }
        let start = hash % count;
        for offset in 0..count {
            let index = (start + offset) % count;
            if let Some(exit) = slots[index].clone() {
                exit.stats.on_assigned();
                drop(slots);
                let now = logging::now_ms();
                if let Ok(mut bindings) = self.bindings.lock() {
                    prune_bindings(&mut bindings, now);
                    bindings.insert(account_id.to_string(), now + BIND_TTL_MS);
                }
                return Some(exit);
            }
        }
        None
    }

    /// 按设备 id 取出口（账号代理引用指定设备时使用）
    ///
    /// 设备会随重启重建、id 会变，所以找不到时按「设备 id → 出口槽位」的对照
    /// 回落到同一槽位的新设备 —— 重启前保存的 `warp:<旧 id>` 因此不必重新选择
    ///
    /// 与 `pick` / `pick_for` 不同，这里不计入使用统计：解析账号出口是
    /// 展示与出网准备，不代表一次真实的出口分配
    pub fn find(&self, id: &str) -> Option<WarpExit> {
        if id.is_empty() {
            return None;
        }
        // 先按当前设备找；持锁只在查表这一小段，槽位对照回落下会读库，
        // 不能放在锁里
        let direct = self.exits.lock().ok()
            .and_then(|slots| slots.iter().flatten().find(|exit| exit.id == id).cloned());
        if direct.is_some() {
            return direct;
        }
        let slot = settings::device_slot(id)?;
        self.exits.lock().ok()
            .and_then(|slots| slots.get(slot).and_then(|entry| entry.clone()))
    }

    /// 该账号是否仍在 WARP 出口的临时绑定期内
    pub fn is_bound(&self, account_id: &str) -> bool {
        if account_id.is_empty() {
            return false;
        }
        let now = logging::now_ms();
        let Ok(mut bindings) = self.bindings.lock() else {
            return false;
        };
        // 顺手清掉过期项：绑定表只按账号增长，不清会一直留着历史账号
        prune_bindings(&mut bindings, now);
        bindings
            .get(account_id)
            .is_some_and(|until| *until > now)
    }

    /// 管理页读一份池状态（出口读数 + 期望数量）
    pub fn payload(&self) -> Value {
        let exits: Vec<Value> = match self.exits.lock() {
            Ok(slots) => slots.iter().enumerate()
                .filter_map(|(index, slot)| slot.as_ref().map(|exit| exit.describe(index)))
                .collect(),
            Err(_) => Vec::new(),
        };
        json!({
            "exits": exits,
            "ready": true,
            "targetSize": settings::target_size(),
            "maxSize": settings::MAX_SIZE,
        })
    }

    /// 上游 429 且池内出口不足时，注册一台新设备并追加到池中
    ///
    /// 保留现有出口：新设备注册失败时旧出口仍可用，不会把池子清空
    pub async fn add_exit(self: &Arc<Self>) -> Result<WarpExit, String> {
        let index = {
            let Ok(slots) = self.exits.lock() else {
                return Err("WARP 出口池状态不可用".to_string());
            };
            if slots.len() >= settings::MAX_SIZE {
                return Err(format!("WARP 出口数已达上限 {}", settings::MAX_SIZE));
            }
            slots.len()
        };
        let exit = build_exit(&self.registrar, index).await?;
        let size = {
            let Ok(mut slots) = self.exits.lock() else {
                return Err("WARP 出口池状态不可用".to_string());
            };
            if slots.len() >= settings::MAX_SIZE {
                return Err(format!("WARP 出口数已达上限 {}", settings::MAX_SIZE));
            }
            slots.push(Some(exit.clone()));
            slots.len()
        };
        // 期望数量跟着涨：重启后按同样的台数重建
        let _ = settings::save_size(size);
        logging::log("[WARP]", &format!("已添加 WARP 出口 {}，本地 SOCKS5 127.0.0.1:{}", index + 1, exit.port));
        Ok(exit)
    }

    /// 删除一台设备：停止本地 SOCKS5 服务、注销设备、从池中移除
    ///
    /// 在途连接会随本地服务停止而中断
    pub async fn remove_exit(self: &Arc<Self>, id: &str) -> Result<Value, String> {
        let (exit, size) = {
            let Ok(mut slots) = self.exits.lock() else {
                return Err("WARP 出口池状态不可用".to_string());
            };
            let Some(position) = slots.iter().position(|slot| {
                slot.as_ref().is_some_and(|exit| exit.id == id)
            }) else {
                return Err("WARP 设备不存在（可能已被重建或删除）".to_string());
            };
            let exit = slots.remove(position)
                .ok_or_else(|| "WARP 设备不存在（可能已被重建或删除）".to_string())?;
            (exit, slots.len())
        };
        // 先停本地服务再注销设备：Wi-Fi 侧断链后不必等隧道超时
        exit.stop.notify_waiters();
        self.registrar.unregister(&exit.registration).await;
        let _ = settings::save_size(size);
        logging::log("[WARP]", &format!("已删除 WARP 出口（设备 {}），剩余 {size} 台", exit.id));
        Ok(self.payload())
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

/// 等到这台设备可用（池已发布并且该设备或它对应的槽位已建好），超时返回 false
///
/// 重启后保存的 `warp:<设备 id>` 引用必须等设备重建完才有意义：早于池就绪
/// 去解析只会拿到「出口池尚未就绪」，而第一次检查更新正好落在这个窗口里
pub async fn wait_device(id: &str, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if global().and_then(|pool| pool.find(id)).is_some() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// 后台初始化出口池（只执行一次；失败按指数退避重试）
pub fn spawn() {
    if POOL.get().is_some() {
        return;
    }
    crate::spawn_task(async move {
        let size = settings::target_size();
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
    // 目标数量为 0 时不注册设备：管理页把出口删空后重启，应保持空池
    if count == 0 {
        return Ok(Arc::new(WarpPool {
            exits: Mutex::new(Vec::new()),
            cursor: AtomicUsize::new(0),
            registrar,
            bindings: Mutex::new(HashMap::new()),
        }));
    }
    // 第一个出口同步建立，成功后立即发布池子；其余出口后台补齐，
    // 避免某个设备注册或探测失败把整池启动拖长
    let first = build_exit(&registrar, 0).await?;
    let mut slots = vec![None; count];
    slots[0] = Some(first);
    let pool = Arc::new(WarpPool {
        exits: Mutex::new(slots),
        cursor: AtomicUsize::new(0),
        registrar,
        bindings: Mutex::new(HashMap::new()),
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
            Ok(exit) => {
                // 记下这台设备落在哪个槽位：重启后注册的是新设备、新 id，
                // 重启前保存的 `warp:<旧 id>` 引用要靠它接回来
                settings::remember_device(&exit.id, index);
                return Ok(exit)
            }
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
    let stats = Arc::new(ExitStats::default());
    let stop = Arc::new(Notify::new());
    tokio::spawn(socks::serve(listener, net, Arc::clone(&stats), Arc::clone(&stop)));

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
        id: registration.id.clone(),
        port,
        egress_ip,
        proxy,
        stats,
        created_at: logging::now_ms() as u64,
        registration: Arc::new(registration.clone()),
        stop,
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
