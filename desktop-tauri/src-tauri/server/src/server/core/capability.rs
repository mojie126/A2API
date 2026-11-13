//! 模型能力位的共享知识：六个键名、值的归一，以及把覆盖写进目录条目。
//!
//! ── 这六个键是什么 ──────────────────────────────────────────
//! 它们是 `list_item`（`/v1/models` 的条目）向**下游客户端**声明的那组字段：
//!
//!   maxInputTokens     上下文窗口（客户端按它估算截断）
//!   maxOutputTokens    单次回复的输出上限
//!   supportsToolCall   支持工具调用
//!   supportsImages     支持图片输入
//!   supportsVideo      支持视频输入
//!   supportsReasoning  支持思考
//!
//! 值来自各家的目录清单（代码里的静态表 / 上游远程目录）。**它们可以是错的**
//! —— 上游目录撒谎、网关的静态表跟不上上游调整时，下游会按错的能力构造请求
//! （给不支持的模型发图片、按虚高的窗口堆历史）。此前纠正的唯一手段是改代码；
//! 现在用户在模型管理页里改（覆盖层），这里就是那层覆盖的**共享形状**。
//!
//! ── 为什么键名用驼峰（与目录条目同形），而不是出口的下划线 ────
//! 覆盖直接以「目录条目风格」写回条目（`apply_overrides`），于是 `list_item`
//! 一行不用改就能透出；出口的下划线命名（`max_input_tokens` 等）仍由
//! `list_item` 一处决定。若覆盖层另用一套名字，出口处就多一次映射，而那份
//! 映射是「改了一个字段、另一处忘了改」的经典来源。
//!
//! ── 两份存储共用本模块 ──────────────────────────────────────
//! 内置家的覆盖存 `modelRules.capabilities`（见 `model_rules`），自定义家存
//! 提供商记录的 `models[].capabilities` 里。两边的读侧容错、写侧校验都调
//! 这里，保证同一份覆盖在两处存储的判定口径不会分叉。
//!
//! ── 覆盖层里还有一项不是「能力位」───────────────────────────
//! [`REASONING_LEVELS_KEY`]（`reasoningLevels`）是**思考档位表**：它回答的是
//! 「这个模型认识哪几个档位」，值是字符串数组而不是布尔 / 数值，出口键名也由
//! `list_item` 另定（`reasoning_efforts`）。它仍然放在同一张覆盖表里 —— 一条
//! `(provider, id)` 的覆盖只有一处存储，档位与能力位同生共死（见 `model_rules`
//! 的模块头）。它**不在** [`KEYS`] 里：那个数组是「六个能力位」的定义，管理页的
//! 六项展示、`/v1/models` 的 `supports_*` 与 `capSources` 的来源标记都按它跑，
//! 混进一个数组键会让那些按单值写的分支多出一堆特例。
//! 档位表**只覆盖出口声明**，不参与转发决策：它换的是「对下游说这个模型支持
//! 哪几档」，而「这条通道收不收档位字段」另由 `reasoningBindingSupported` 回答
//!（见 `model_rules::reasoning` 的模块头）。

use serde_json::{Map, Value};

/// 六个能力键。顺序即管理页弹窗字段与能力徽章的展示顺序（前端按同一顺序
/// 渲染，`overridden_keys` 也按它排）。
pub const KEYS: [&str; 6] = [
    "maxInputTokens",
    "maxOutputTokens",
    "supportsToolCall",
    "supportsImages",
    "supportsVideo",
    "supportsReasoning",
];

/// 思考档位表在覆盖层里用的键名（与目录条目的出口键名同形）。
///
/// 与 [`KEYS`] 里那些键的关系见模块头那段；用同一个名字是为了让「存储里手写的
/// 键」与「清单条目上的生效键」逐字一致 —— 读配置的人一眼能对上界面上的那一栏。
pub const REASONING_LEVELS_KEY: &str = "reasoningLevels";

/// 档位表的条数上限与单条字符上限。
///
/// 条数上限 32：现实里上游最多六七档，留出余量后 32 已经远超需要，而它挡住的是
/// 「把覆盖层当任意数据位置塞东西」。单条 32 字符与映射里绑定等级的上限
///（`model_rules::reasoning` 的 `REASONING_MAX_CHARS`）同口径。
pub const MAX_REASONING_LEVELS: usize = 32;
pub const MAX_REASONING_LEVEL_CHARS: usize = 32;

/// token 数值键的上限（1 亿）：再大就是手滑多敲了几个零，而虚高的上下文窗口
/// 会让下游按错误的量级堆积历史 —— 那正是这层覆盖要修的毛病之一，不该由它
/// 引入一个新的。下限是 1（0 / 负数对下游没有意义）。
pub const MAX_TOKEN_VALUE: f64 = 100_000_000.0;

/// 该键是不是思考档位表（[`REASONING_LEVELS_KEY`]）。
pub fn is_levels_key(key: &str) -> bool {
    key == REASONING_LEVELS_KEY
}

/// 该键是不是覆盖层认得的键（六个能力位加档位表）。
///
/// 写侧（两个 HTTP 接口与提供商记录的整表保存）用它做键名白名单，读侧不必 ——
/// 读侧按 [`normalize_object`] 逐键取值，不认的键自然不出现。
pub fn is_known_key(key: &str) -> bool {
    KEYS.contains(&key) || is_levels_key(key)
}

/// 该键是不是 token 数值键（`maxInputTokens` / `maxOutputTokens`）。
pub fn is_token_key(key: &str) -> bool {
    key == "maxInputTokens" || key == "maxOutputTokens"
}

/// 该键是不是布尔能力键。
pub fn is_bool_key(key: &str) -> bool {
    key == "supportsToolCall"
        || key == "supportsImages"
        || key == "supportsVideo"
        || key == "supportsReasoning"
}

/// 单值归一：token 键收正整数（浮点写法取整，如 `200000.0`），布尔键收 bool；
/// 其余键名 / 非法值一律 `None`（调用方按「丢弃」处理）。
pub fn normalize_value(key: &str, value: &Value) -> Option<Value> {
    if is_levels_key(key) {
        return Some(Value::Array(
            normalize_levels(value)?
                .into_iter()
                .map(Value::String)
                .collect(),
        ));
    }
    if is_token_key(key) {
        let number = value.as_f64()?;
        if !number.is_finite() || number < 1.0 || number > MAX_TOKEN_VALUE {
            return None;
        }
        return Some(Value::from(number.trunc() as u64));
    }
    if is_bool_key(key) {
        return value.as_bool().map(Value::Bool);
    }
    None
}

/// 归一思考档位表：原样保留用户写的拼写与顺序，逐项去空白、忽略大小写去重
///（保留首次出现的写法）。
///
/// 非数组、含非字符串项、含空白项、超长项、超量 → `None`（非法，调用方按
/// 「丢弃 / 报错」处理）；**空数组给空表而不是 `None`** —— 调用方据此区分
/// 「没给这一项」（非法值）与「给了但一项不剩」（清除覆盖，界面把档位删到一行
/// 不剩就是这个意思，见 `model_rules::set_capabilities`）。
pub fn normalize_levels(value: &Value) -> Option<Vec<String>> {
    let items = value.as_array()?;
    if items.len() > MAX_REASONING_LEVELS {
        return None;
    }
    let mut levels: Vec<String> = Vec::new();
    for item in items {
        let Some(text) = item.as_str() else {
            return None;
        };
        let text = text.trim();
        if text.is_empty() || text.chars().count() > MAX_REASONING_LEVEL_CHARS {
            return None;
        }
        if levels.iter().any(|level| level.eq_ignore_ascii_case(text)) {
            continue;
        }
        levels.push(text.to_string());
    }
    Some(levels)
}

/// 从任意 JSON 对象里挑出合法能力键，返回**稀疏表**（没给的键不出现；非法
/// 键名 / 非法值丢弃；null 值丢弃 —— 存储层不存在「显式 null」这一档，
/// 清除覆盖靠删键表达）。
pub fn normalize_object(value: Option<&Value>) -> Map<String, Value> {
    let mut result = Map::new();
    let Some(object) = value.and_then(Value::as_object) else {
        return result;
    };
    for key in KEYS.iter().copied().chain(std::iter::once(REASONING_LEVELS_KEY)) {
        let Some(found) = object.get(key).and_then(|item| normalize_value(key, item)) else {
            continue;
        };
        // 空档位表 = 没有覆盖（见 normalize_levels 的说明）：落一条空表只会让
        // 「这一项被覆盖过吗」在存储、管理页与出口三处各多一个无意义的分支
        if found.as_array().is_some_and(|items| items.is_empty()) {
            continue;
        }
        result.insert(key.to_string(), found);
    }
    result
}

/// 从覆盖表里取出用户手写的思考档位表（没有这一项 / 值非法 → `None`）。
///
/// 调用方是清单成型处：拿到 `Some` 就用它覆盖条目上的档位声明，见
/// `model_meta::apply_user_reasoning_levels`。
pub fn levels_from(overrides: &Map<String, Value>) -> Option<Vec<String>> {
    overrides
        .get(REASONING_LEVELS_KEY)
        .and_then(normalize_levels)
        .filter(|levels| !levels.is_empty())
}

/// 把稀疏覆盖写进目录条目（原地）。覆盖表里不会出现 null（见 `normalize_object`），
/// 所以这里只做「有则改写」。
pub fn apply_overrides(item: &mut Value, overrides: &Map<String, Value>) {
    if overrides.is_empty() {
        return;
    }
    let Some(object) = item.as_object_mut() else {
        return;
    };
    for (key, value) in overrides {
        // 档位表不在这里写：换的是整份档位声明，还要连带摘掉固定档、校验默认档、
        // 记来源标记，统一由 `model_meta::apply_user_reasoning_levels` 在
        // models.dev 补齐**之后**应用（理由见那个函数）
        if is_levels_key(key) {
            continue;
        }
        object.insert(key.clone(), value.clone());
    }
}

pub fn images_for_channel(provider: &str, item: &Value) -> Option<bool> {
    let user = item.pointer("/capSources/supportsImages").and_then(Value::as_str) == Some("user")
        || item.pointer("/capabilities/supportsImages").and_then(Value::as_bool).is_some();
    // CodeArts 的福利目录固定填 false，缺字段的 agent 目录也会补 false；它们不是拒图证据。
    if provider == "codearts" && !user && item.get("supportsImages").and_then(Value::as_bool) == Some(false) { return None; }
    item.get("supportsImages").and_then(Value::as_bool)
}

/// 条目的生效能力：六个键齐全，未声明给 null，管理页不能把未知当成不支持。
pub fn effective(item: &Value) -> Value {
    let mut result = Map::new();
    for key in KEYS {
        let value = item.get(key).filter(|value| !value.is_null()).cloned();
        result.insert(key.to_string(), value.unwrap_or(Value::Null));
    }
    Value::Object(result)
}

/// 生效值里被覆盖的键（按 [`KEYS`] 的顺序）。
///
/// 只列六个能力位：档位表不在 [`KEYS`] 里，管理页判断它是否被覆盖读的是
/// `reasoningSource == "user"`（见 `model_meta` 的 `apply_user_reasoning_levels`）。
pub fn overridden_keys(overrides: &Map<String, Value>) -> Vec<String> {
    KEYS.iter()
        .filter(|key| overrides.contains_key(**key))
        .map(|key| (*key).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 档位表归一：原样保留用户的拼写与顺序，去空白、忽略大小写去重，非法项即失败
    #[test]
    fn levels_normalization_keeps_user_spelling_and_order() {
        assert_eq!(
            normalize_levels(&json!(["low", " HIGH ", "low"])).unwrap(),
            vec!["low".to_string(), "HIGH".to_string()]
        );
        // 空数组是「一项不剩」（清除覆盖），不是非法值
        assert_eq!(normalize_levels(&json!([])).unwrap(), Vec::<String>::new());
        assert!(normalize_levels(&json!(["", "low"])).is_none());
        assert!(normalize_levels(&json!(["low", 1])).is_none());
        assert!(normalize_levels(&json!("low")).is_none());
        assert!(normalize_levels(&json!(["x".repeat(MAX_REASONING_LEVEL_CHARS + 1)])).is_none());
        let too_many = (0..=MAX_REASONING_LEVELS)
            .map(|index| format!("level-{index}"))
            .collect::<Vec<_>>();
        assert!(normalize_levels(&json!(too_many)).is_none());
    }

    /// 空档位表不落进覆盖表（等价于没有覆盖），档位表也不由 `apply_overrides` 逐键写
    #[test]
    fn empty_levels_list_is_not_stored_as_an_override() {
        let overrides = normalize_object(Some(&json!({
            "supportsReasoning": true,
            "reasoningLevels": [],
        })));
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides["supportsReasoning"], json!(true));
        assert!(levels_from(&overrides).is_none());

        let stored = normalize_object(Some(&json!({ "reasoningLevels": ["low", "high"] })));
        assert_eq!(
            levels_from(&stored).unwrap(),
            vec!["low".to_string(), "high".to_string()]
        );
        let mut item = json!({ "id": "model" });
        apply_overrides(&mut item, &stored);
        assert!(item.get("reasoningLevels").is_none());
    }
}
