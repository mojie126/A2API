//! WARP 出口池的运行参数，存于 kv 的 `warpPool` 保留键
//!
//! 只保存期望出口数：设备本身不落盘（私钥绑定当前环境，重启后重新注册），
//! 所以这里记录的是「重启后按几台设备重建」这一口径，与设备身份无关。

use std::sync::OnceLock;

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