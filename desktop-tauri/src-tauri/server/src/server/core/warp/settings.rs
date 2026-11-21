//! WARP 出口池的运行参数，存于 kv 的 `warpPool` 保留键
//!
//! 只保存期望出口数与「设备 id → 出口槽位」对照：设备本身不落盘（私钥绑定
//! 当前环境，重启后重新注册），对照表只用于让重启前保存的 `warp:<id>` 引用
//! 按槽位接回重建后的新设备

use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use crate::server::db::Db;

pub const KV_KEY: &str = "warpPool";
/// 默认出口数：兼顾启动速度与并发，与既有 `ZCODE_WARP_POOL_SIZE` 默认值一致
pub const DEFAULT_SIZE: usize = 2;
/// 允许删除到 0 台（池为空时 ZCode 请求回退账号原代理或直连）
pub const MIN_SIZE: usize = 0;
/// 上限对齐注册侧的多设备能力与 Cloudflare 限流窗口
pub const MAX_SIZE: usize = 32;

static DB: OnceLock<Option<Db>> = OnceLock::new();

pub fn install(db: Option<Db>) {
    let _ = DB.set(db);
}

fn read_raw() -> Value {
    let Some(db) = DB.get().and_then(Option::as_ref) else {
        return json!({});
    };
    db.with(|conn| {
        conn.query_row("SELECT value FROM kv WHERE key = ?1", [KV_KEY], |row| {
            row.get::<_, String>(0)
        }).ok()
    })
    .flatten()
    .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    .filter(Value::is_object)
    .unwrap_or_else(|| json!({}))
}

fn write_raw(value: &Value) -> Result<(), String> {
    let Some(db) = DB.get().and_then(Option::as_ref) else {
        return Err("设置数据库不可用".to_string());
    };
    let text = serde_json::to_string(value).map_err(|error| format!("编码 WARP 池设置失败: {error}"))?;
    db.with(|conn| {
        conn.execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![KV_KEY, text],
        )
        .map(|_| ())
        .map_err(|error| format!("保存 WARP 池设置失败: {error}"))
    })
    .ok_or_else(|| "设置数据库不可用".to_string())??;
    Ok(())
}

/// 期望出口数：kv 优先，其次环境变量 `ZCODE_WARP_POOL_SIZE`，最后默认值
pub fn target_size() -> usize {
    let stored = read_raw().get("size").and_then(Value::as_u64).map(|size| size as usize);
    let value = stored.unwrap_or_else(|| {
        std::env::var("ZCODE_WARP_POOL_SIZE")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(DEFAULT_SIZE)
    });
    value.clamp(MIN_SIZE, MAX_SIZE)
}

/// 记录期望出口数（增删设备后调用，重启时按此重建）
pub fn save_size(size: usize) -> Result<(), String> {
    let mut value = read_raw();
    value["size"] = json!(size.clamp(MIN_SIZE, MAX_SIZE));
    write_raw(&value)
}

/// 「设备 id → 出口槽位」对照表的读写锁
///
/// 出口由后台任务逐台补齐，多台同时完成时会并发读改写同一份 kv，
/// 不加锁会把先写进去的对照覆盖掉
static DEVICE_SLOTS: Mutex<()> = Mutex::new(());
/// 对照表的进程内缓存：解析账号代理是每个请求都要走的路，不能在热路径上
/// 反复读库；重启后的首次查询回落到 kv，命中后缓存下来
static SLOT_CACHE: Mutex<Option<std::collections::HashMap<String, usize>>> = Mutex::new(None);
/// 对照表最多保留的条数：每轮重建最多 `MAX_SIZE` 条，留两轮的历史
const MAX_DEVICE_SLOTS: usize = MAX_SIZE * 2;

fn cached_slots() -> Option<std::collections::HashMap<String, usize>> {
    SLOT_CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
}

/// 记录一台出口的槽位（设备注册完成时调用）
pub fn remember_device(id: &str, slot: usize) {
    if id.is_empty() {
        return;
    }
    {
        let mut cache = SLOT_CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let slots = cache.get_or_insert_with(std::collections::HashMap::new);
        // 超出上限时清空重来：旧槽位的 id 早已不再被引用，留着只会无限增长
        if slots.len() > MAX_DEVICE_SLOTS {
            slots.clear();
        }
        slots.insert(id.to_string(), slot);
    }
    let _guard = DEVICE_SLOTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut value = read_raw();
    let mut devices = value
        .get("devices")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    devices.insert(id.to_string(), json!(slot));
    if devices.len() > MAX_DEVICE_SLOTS {
        devices.clear();
        devices.insert(id.to_string(), json!(slot));
    }
    value["devices"] = Value::Object(devices);
    let _ = write_raw(&value);
}

/// 这台设备上次落在哪个槽位（重启前保存的引用靠它接回新设备）
pub fn device_slot(id: &str) -> Option<usize> {
    if id.is_empty() {
        return None;
    }
    if let Some(slots) = cached_slots() {
        if let Some(slot) = slots.get(id) {
            return Some(*slot);
        }
    }
    let slot = read_raw()
        .get("devices")
        .and_then(Value::as_object)
        .and_then(|devices| devices.get(id))
        .and_then(Value::as_u64)
        .map(|slot| slot as usize)?;
    let mut cache = SLOT_CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.get_or_insert_with(std::collections::HashMap::new)
        .insert(id.to_string(), slot);
    Some(slot)
}

#[cfg(test)]
mod tests {
    use super::{device_slot, install, remember_device, save_size, target_size};

    /// 设备 id → 槽位的对照要能跨「保存期望数量」保留：
    /// 重启后新设备按同一槽位接回旧引用，靠的就是这份对照
    #[test]
    fn device_slots_survive_rebuild() {
        let (db, _guard) = crate::server::db::test_temp::TempDb::open("warp-settings");
        install(Some(db));
        remember_device("old-a", 0);
        remember_device("old-b", 1);
        assert_eq!(device_slot("old-a"), Some(0));
        assert_eq!(device_slot("old-b"), Some(1));
        assert_eq!(device_slot("missing"), None);

        // 保存期望台数不应覆盖对照表
        save_size(2).expect("写入期望台数");
        assert_eq!(target_size(), 2);
        assert_eq!(device_slot("old-a"), Some(0));

        // 空 id 不记录、不命中
        remember_device("", 3);
        assert_eq!(device_slot(""), None);
    }
}
