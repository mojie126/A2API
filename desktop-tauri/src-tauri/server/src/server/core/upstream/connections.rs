//! 账号级活跃连接计数（账号页「连接数」列的数据源）。
//!
//! ── 计数口径：一次下游请求 = 一条连接 ──────────────────────────
//! 数的是「此刻正在使用这个账号的请求数」，不是 TCP 连接数、也不是尝试次数：
//!   · 同一请求在账号间轮换（429 / 401 后顺延）时计数**从旧账号移到新账号**
//!     （[`ConnectionGuard::rebind`]）—— 任意时刻一个请求只占一个账号；
//!   · 一次请求内对上游的重试（11128 退避、401 刷新后重发）不额外计数，
//!     它还是同一个请求；
//!   · 流式请求计到**字节下发完**为止：凭证由 `ForwardStream` 持有，生命周期
//!     与去重槽位（`InFlightGuard`）完全一致。客户端断开、上游断开都靠 drop
//!     传播收尾，所以这里没有、也不需要显式的 release 调用点。
//!
//! ── 为什么单独成文件 ────────────────────────────────────────
//! 与 `rotate.rs` / `usage.rs` 同样的理由：转发编排（`provider_loop`）里只该出现
//! 「选谁去发」，而「谁正在发」是一份自洽的进程内状态 —— 一张表 + 两个原子动作
//! + 一个 Drop 凭证。摊在编排里会让这两件事互相淹没。
//!
//! ── 与 OmniProxy 上游管理页「连接」列的对应关系 ────────────────
//! 口径与视觉（有连接时显示数字、为 0 时留空）对齐那一列，仅把计数单位从
//! provider 换成 account —— 本项目的转发链路上「谁去发」的答案是账号
//! （provider 只是账号的属性，见 `provider_loop` 的模块头）。
//!
//! ── 并发模型 ──────────────────────────────────────────────
//! 一把 `Mutex` 包住一张 `HashMap`：增减都只是内存操作（不读盘、不出网），
//! 持锁时间在纳秒级，不会阻塞任何管理 API。锁中毒不致命，与 `mod.rs` 的
//! `lock_table` 同一策略（取回内部值继续用）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// 在途计数表的一项：在途请求数与该账号的提供商
///
/// provider 由每次选路带下来：它只是账号的属性，记在这里是为了让
/// 「有没有同一家的请求在途」这类判定不必再反查账号库
struct Entry {
    count: usize,
    provider: String,
}

/// 账号 id → 当前正在使用该账号的下游请求数（0 的账号不留在表里）。
///
/// `Clone` 是浅拷贝（内部 `Arc`）：`ServerState` 的 handler 克隆状态后，
/// 拿到的仍是同一份计数。
#[derive(Clone)]
pub struct Connections {
    inner: Arc<Mutex<HashMap<String, Entry>>>,
    // 上游 beta.2 新增：计数变化时唤醒等待方（面板事件推送的连接数也走这条）
    changed: Arc<tokio::sync::Notify>,
}

impl Connections {
    pub fn new() -> Self {
        Self { inner: Arc::new(Mutex::new(HashMap::new())), changed: Arc::new(tokio::sync::Notify::new()) }
    }

    /// 全部**非零**计数（account_id → count），按 id 排序保证输出稳定。
    ///
    /// 计数为 0 的账号不出现在结果里：调用方（界面）按「缺失即 0」处理，
    /// 于是接口不必为几十个空闲账号各送一个 0。
    pub fn snapshot(&self) -> Vec<(String, usize)> {
        let guard = lock_counts(&self.inner);
        let mut entries: Vec<(String, usize)> = guard
            .iter()
            .map(|(id, entry)| (id.clone(), entry.count))
            .collect();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries
    }

    /// 是否存在满足条件的在途账号（回调收到账号 id、提供商与该账号的在途数）
    ///
    /// 供「按提供商统计在途」一类判定使用（例如 ZCode 直连出口是否被占用）；
    /// 带上在途数是因为调用方可能要把**自己**从判定里刨掉 —— 请求在容量准入
    /// 阶段就已预占当前账号，只看「有在途」会把自身误判成「别人占着出口」，
    /// 与 [`Self::snapshot`] 一样只读一次锁
    pub fn any_in_flight(&self, mut predicate: impl FnMut(&str, &str, usize) -> bool) -> bool {
        let guard = lock_counts(&self.inner);
        guard.iter().any(|(id, entry)| predicate(id, &entry.provider, entry.count))
    }

    /// 计数 +1（空 id 是「没有账号」的转发，不计数）
    fn add(&self, account_id: &str, provider: &str) {
        if account_id.is_empty() {
            return;
        }
        let mut guard = lock_counts(&self.inner);
        let entry = guard.entry(account_id.to_string()).or_insert_with(|| Entry {
            count: 0,
            provider: provider.to_string(),
        });
        entry.count += 1;
        // 账号的提供商不随时间变化，这里同步一次只是防止老记录缺字段
        entry.provider = provider.to_string();
        drop(guard);
        self.changed.notify_waiters();
        crate::server::core::panel_events::notify(crate::server::core::panel_events::Change::Connections);
    }

    /// 计数 -1（减到 0 就删项；无记录时是空操作 —— 幂等，重复释放不会变负数）
    fn sub(&self, account_id: &str) {
        if account_id.is_empty() {
            return;
        }
        let mut guard = lock_counts(&self.inner);
        match guard.get_mut(account_id) {
            Some(entry) if entry.count > 1 => entry.count -= 1,
            Some(_) => {
                guard.remove(account_id);
            }
            None => {}
        }
        drop(guard);
        self.changed.notify_waiters();
        crate::server::core::panel_events::notify(crate::server::core::panel_events::Change::Connections);
    }

    pub(super) fn changed_notify(&self) -> Arc<tokio::sync::Notify> { self.changed.clone() }
}

impl Default for Connections {
    fn default() -> Self {
        Self::new()
    }
}

/// 取计数表锁；锁中毒不致命（与 `mod.rs` 的 `lock_table` 同一策略）
fn lock_counts<'a>(
    inner: &'a Mutex<HashMap<String, Entry>>,
) -> std::sync::MutexGuard<'a, HashMap<String, Entry>> {
    match inner.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// 一条在途请求的计数凭证：**drop 即释放**，并可在轮换时**改绑**到另一个账号。
///
/// 由 `attempt_queue` 在循环外创建、循环内按选中的账号 rebind，成功转流式时
/// 被 move 进响应流 —— 于是「请求还在跑」这件事在两种形态（流式 / 非流式）
/// 上有同一个释放点：凭证析构。错误路径上的 `?` 提前返回也由 Drop 兜底，
/// 不需要每个分支各写一次释放。
///
/// 可见性是 `pub`（而非 `pub(super)`）：`ForwardStream::new` 的入参里有它，
/// 而那个构造函数在 `core` 内是公开的 —— 类型比调用面窄会触发
/// `private_interfaces` 告警。
pub struct ConnectionGuard {
    connections: Connections,
    /// 当前占用的账号（None = 这一次尝试没有指定账号，如环境变量旁路）
    account_id: Option<String>,
}

impl ConnectionGuard {
    pub(super) fn new(connections: Connections) -> Self {
        Self { connections, account_id: None }
    }

    /// 改绑到另一个账号：先放掉旧的，再给新的 +1（同一个账号则原样不动）。
    ///
    /// 空字符串与 None 等价（都表示「没有账号」），这样调用方可以直接把
    /// `RouteTarget::account_id` 传进来，不必先做一次归一。
    /// `provider` 随计数一起记录，供「按家统计在途」的判定使用
    pub(super) fn rebind(&mut self, account_id: Option<String>, provider: &str) {
        let next = account_id.filter(|id| !id.is_empty());
        if self.account_id == next {
            return;
        }
        if let Some(previous) = self.account_id.as_deref() {
            self.connections.sub(previous);
        }
        if let Some(current) = next.as_deref() {
            self.connections.add(current, provider);
        }
        self.account_id = next;
    }

    pub(super) fn try_rebind(&mut self, account_id: Option<String>, provider: &str, limit: u64) -> bool {
        let next = account_id.filter(|id| !id.is_empty());
        if self.account_id == next { return true; }
        let mut counts = lock_counts(&self.connections.inner);
        if let Some(id) = &next {
            if limit > 0 && counts.get(id).map(|entry| entry.count).unwrap_or(0) >= limit as usize { return false; }
        }
        if let Some(previous) = self.account_id.as_deref() {
            if let Some(entry) = counts.get_mut(previous) {
                entry.count = entry.count.saturating_sub(1);
                if entry.count == 0 { counts.remove(previous); }
            }
        }
        if let Some(id) = &next {
            let entry = counts.entry(id.clone()).or_insert_with(|| Entry { count: 0, provider: provider.to_string() });
            entry.count += 1;
            entry.provider = provider.to_string();
        }
        self.account_id = next;
        drop(counts);
        self.connections.changed.notify_waiters();
        crate::server::core::panel_events::notify(crate::server::core::panel_events::Change::Connections);
        true
    }

    /// 把计数移交给响应流：返回一个持有当前账号的新凭证，自身转为已释放。
    ///
    /// 流式请求的成功路径要把计数活到「字节下发完」，而那时 `attempt_queue`
    /// 的栈帧早就退出了 —— 凭证必须被 move 进流对象。**不是** clone：同一个账号
    /// 被两份凭证各 +1 会让计数翻倍，所以这里用 `take()` 转移所有权，
    /// 本对象的 `Drop` 随即变成空操作。
    ///
    /// 可见性是 `pub(in crate::server::core)`（而非 `pub(super)`）：自定义
    /// 提供商的转发入口（`providers::custom::forward`）在成功转流式时同样要
    /// 移交计数 —— 它在 `core` 的后代模块里，`pub(super)`（只到 `upstream`
    /// 子树）够不到。调用面与 `attempt_queue` 的无状态路径逐字同构。
    pub(in crate::server::core) fn handoff(&mut self) -> Self {
        Self {
            connections: self.connections.clone(),
            account_id: self.account_id.take(),
        }
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        if let Some(account_id) = self.account_id.as_deref() {
            self.connections.sub(account_id);
        }
    }
}
