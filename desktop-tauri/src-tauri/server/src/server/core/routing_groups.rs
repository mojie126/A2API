//! 跨模型成员投影。运行时字段只存在于候选副本，不写入账号库。

use std::collections::{HashMap, HashSet};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use crate::server::config::{self, RoutingGroup};
use super::{account_store::AccountStore, capability, custom_providers, key_scope::{self, KeyScope}, models::model_id, providers, routing};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestNeeds {
    #[serde(default)] pub tools: bool,
    #[serde(default)] pub images: bool,
    #[serde(default)] pub video: bool,
}

impl RequestNeeds {
    pub fn from_body(body: &Value) -> Self {
        let mut needs = Self {
            tools: ["tools", "functions"].iter().any(|key| body.get(key).and_then(Value::as_array).is_some_and(|tools| !tools.is_empty())),
            ..Default::default()
        };
        for message in body.get("messages").and_then(Value::as_array).into_iter().flatten() {
            needs.tools |= matches!(message.get("role").and_then(Value::as_str), Some("tool" | "function"))
                || message.get("function_call").is_some()
                || message.get("tool_calls").and_then(Value::as_array).is_some_and(|calls| !calls.is_empty());
            for part in message.get("content").and_then(Value::as_array).into_iter().flatten() {
                match part.get("type").and_then(Value::as_str) {
                    Some("image_url" | "input_image" | "image") => needs.images = true,
                    Some("video_url" | "input_video" | "video") => needs.video = true,
                    _ => {},
                }
            }
        }
        needs
    }

    pub fn reason(&self, item: &Value) -> Option<String> {
        for (required, key, label) in [(self.tools, "supportsToolCall", "工具调用"),
            (self.images, "supportsImages", "图片输入"), (self.video, "supportsVideo", "视频输入")] {
            if required && item.get(key).and_then(Value::as_bool) != Some(true) {
                return Some(if item.get(key).and_then(Value::as_bool) == Some(false) {
                    format!("该模型不支持{label}，已跳过")
                } else { format!("该模型的{label}能力未声明，请先在模型管理中确认") });
            }
        }
        None
    }
}

fn manifest(provider: &str) -> Vec<Value> {
    if let Some(owner) = custom_providers::get(provider) {
        // 自定义家的查询必须与对外清单使用同一份绑定出口，别名与补全能力
        // 才能同时参与候选和运行时判定
        custom_providers::public_models(&owner)
    } else { providers::catalog::manifest_for_provider(provider) }
}

pub fn catalog_model(provider: &str, model: &str) -> Option<Value> {
    manifest(provider).into_iter().find(|item| model_id(item).eq_ignore_ascii_case(model))
}

/// 特殊请求路由使用的路由组候选
///
/// 只列出当前至少有一个成员可用的路由组，能力位按成员共同具备的能力求交集，
/// 避免给下游声明成员都不支持的能力。
pub fn public_choices(store: &AccountStore) -> Vec<Value> {
    group_choices(store)
}

fn model_choices(store: &AccountStore) -> Vec<Value> {
    let accounts = store.routing_snapshot();
    let mut ids: Vec<_> = accounts.accounts.iter().map(|account| routing::provider_of(account).to_string()).collect();
    ids.extend(providers::executable_provider_ids());
    ids.extend(custom_providers::list().iter().filter_map(|provider| provider.get("id").and_then(Value::as_str).map(str::to_string)));
    ids.sort(); ids.dedup();
    let mut result = Vec::new();
    for provider in ids {
        if !super::account_store::store_view::account_visible(&provider) { continue; }
        let models = manifest(&provider);
        let mut seen = HashSet::new();
        for item in models {
            let model = model_id(&item);
            if model.is_empty() || !seen.insert(model.to_ascii_lowercase()) { continue; }
            if providers::images::model_is_image(&item) { continue; }
            result.push(json!({ "provider": provider, "providerLabel": providers::label_of(&provider), "model": model,
                "name": item.get("name").and_then(Value::as_str).unwrap_or(&model), "capabilities": capability::effective(&item),
                "accountCount": accounts.accounts.iter().filter(|account| routing::provider_of(account) == provider).count() }));
        }
    }
    result
}

pub fn choices(store: &AccountStore) -> Vec<Value> {
    model_choices(store)
}

pub fn candidate_key(member: &str, account: &str) -> String { format!("{member}\0{account}") }
pub fn account_key(key: &str) -> &str { key.split_once('\0').map(|(_, account)| account).unwrap_or(key) }
pub fn member_key(key: &str) -> Option<&str> { key.split_once('\0').map(|(member, _)| member) }
pub fn attempt_key(account: &Value) -> String {
    account.get("routingCandidateId").and_then(Value::as_str).unwrap_or_else(|| routing::account_id(account).unwrap_or("")).to_string()
}
pub fn candidate_model(account: &Value) -> Option<&str> { account.get("routingModel").and_then(Value::as_str) }
pub fn family_key(account: &Value) -> &str {
    account.get("routingMemberId").and_then(Value::as_str).unwrap_or_else(|| routing::provider_of(account))
}

#[derive(Clone, Default)]
pub struct GroupProjection {
    pub accounts: Vec<Value>,
    pub blocked: Vec<Value>,
}

fn provider_active(provider: &str) -> bool {
    super::account_store::store_view::account_visible(provider)
        && custom_providers::get(provider).map_or_else(|| providers::adapter::adapter_by_id(provider).is_some(), |owner| owner.get("enabled") != Some(&Value::Bool(false)))
}

fn group_choices(store: &AccountStore) -> Vec<Value> {
    let accounts = store.routing_snapshot();
    let mut result = Vec::new();
    for group in config::routing_profiles().groups.values() {
        let members: Vec<_> = group.members.iter().filter(|member| member.included)
            .filter(|member| provider_active(&member.provider)
                && accounts.available_providers.contains(&member.provider)
                && member.accounts.iter().any(|id| accounts.accounts.iter().any(|account|
                    routing::account_id(account) == Some(id)
                        && routing::provider_of(account) == member.provider
                        && account.get("enabled") != Some(&Value::Bool(false)))))
            .filter_map(|member| catalog_model(&member.provider, &member.model))
            .filter(|item| !providers::images::model_is_image(item))
            .collect();
        if members.is_empty() { continue; }
        let capabilities = capability::effective(&json!({
            "supportsToolCall": members.iter().all(|item| item.get("supportsToolCall").and_then(Value::as_bool) == Some(true)),
            "supportsImages": members.iter().all(|item| item.get("supportsImages").and_then(Value::as_bool) == Some(true)),
            "supportsVideo": members.iter().all(|item| item.get("supportsVideo").and_then(Value::as_bool) == Some(true)),
            "supportsReasoning": members.iter().all(|item| item.get("supportsReasoning").and_then(Value::as_bool) == Some(true)),
            "maxInputTokens": common_token_limit(&members, "maxInputTokens"),
            "maxOutputTokens": common_token_limit(&members, "maxOutputTokens"),
        }));
        let member_names: Vec<_> = group.members.iter().filter(|member| member.included)
            .map(|member| member.model.as_str()).collect();
        result.push(json!({
            "id": group.model,
            "provider": "routing-group",
            "providerLabel": "Routing group",
            "model": group.model,
            "name": group.name,
            "kind": "chat",
            "capabilities": capabilities,
            "members": member_names,
            "accountCount": 0,
        }));
    }
    result
}

pub fn project(group: &RoutingGroup, accounts: &[Value], needs: &RequestNeeds, scope: Option<&KeyScope>) -> GroupProjection {
    project_with_vision(group, accounts, needs, scope, None)
}

pub fn project_with_vision(group: &RoutingGroup, accounts: &[Value], needs: &RequestNeeds, scope: Option<&KeyScope>,
    vision: Option<&config::vision_compat::VisionSettings>) -> GroupProjection {
    let mut result = GroupProjection::default();
    let by_id: HashMap<_, _> = accounts.iter().filter_map(|account| routing::account_id(account).map(|id| (id, account))).collect();
    let manifests: HashMap<_, _> = group.members.iter().map(|member| member.provider.clone()).collect::<HashSet<_>>()
        .into_iter().map(|id| { let items = manifest(&id); (id, items) }).collect();
    for member in group.members.iter().filter(|member| member.included) {
        let item = manifests.get(&member.provider).and_then(|items| items.iter().find(|item| model_id(item).eq_ignore_ascii_case(&member.model))).cloned();
        let reason = if !key_scope::allows_provider(scope, &member.provider) { Some("网关 Key 不允许此提供商".to_string()) }
            else if !provider_active(&member.provider) { Some("提供商当前不可用".to_string()) }
            else if let Some(item) = &item {
                if providers::images::model_is_image(item) { Some("生图模型不能用于对话路由".to_string()) }
                else {
                    let mut requirements = needs.clone();
                    if vision.is_some_and(|settings| settings.mode_for(&member.provider, &member.model).is_some()) {
                        requirements.images = false;
                    }
                    requirements.reason(item)
                }
            } else { Some("模型已不在提供商目录中，配置保留但不参与转发".to_string()) };
        let actual = item.as_ref().map(model_id).unwrap_or_else(|| member.model.clone());
        if let Some(reason) = reason {
            result.blocked.push(json!({ "id": member.id, "memberId": member.id, "provider": member.provider,
                "providerLabel": providers::label_of(&member.provider), "model": actual, "name": actual, "state": "unavailable", "reason": reason }));
            continue;
        }
        if member.accounts.is_empty() {
            result.blocked.push(json!({ "id": member.id, "memberId": member.id, "provider": member.provider, "model": actual,
                "name": actual, "state": "excluded", "reason": "该成员没有参与账号" }));
        }
        for id in &member.accounts {
            let Some(account) = by_id.get(id.as_str()).filter(|account| routing::provider_of(account) == member.provider) else {
                result.blocked.push(json!({ "id": id, "memberId": member.id, "provider": member.provider, "model": actual,
                    "name": id, "state": "unavailable", "reason": "参与账号已移除或不属于此提供商" }));
                continue;
            };
            let mut account = (*account).clone();
            account["routingCandidateId"] = candidate_key(&member.id, id).into();
            account["routingMemberId"] = member.id.clone().into();
            account["routingModel"] = actual.clone().into();
            account["routingPriority"] = result.accounts.len().into();
            result.accounts.push(account);
        }
    }
    result
}

pub fn validate_members(group: &RoutingGroup, old: Option<&RoutingGroup>, accounts: &[Value]) -> Result<(), String> {
    for member in &group.members {
        let previous = old.and_then(|group| group.members.iter().find(|item| item.id == member.id && item.provider == member.provider && item.model == member.model));
        match catalog_model(&member.provider, &member.model) {
            Some(item) if providers::images::model_is_image(&item) => return Err("对话路由组不能加入生图模型".into()),
            None if previous.is_none() => return Err(format!("提供商中找不到模型 {}，请刷新后重试", member.model)),
            _ => {},
        }
        for id in &member.accounts {
            if let Some(account) = accounts.iter().find(|account| routing::account_id(account) == Some(id)) {
                if routing::provider_of(account) != member.provider { return Err("参与账号不属于所选提供商".into()); }
            } else if !previous.is_some_and(|item| item.accounts.contains(id)) { return Err("参与账号不存在，请刷新后重试".into()); }
        }
    }
    Ok(())
}

pub fn append_public(store: &AccountStore, scope: Option<&KeyScope>, data: &mut Vec<Value>) {
    let accounts = store.routing_snapshot();
    for group in config::routing_profiles().groups.values() {
        if !key_scope::allows_model(scope, &group.model) || data.iter().any(|item| model_id(item).eq_ignore_ascii_case(&group.model)) { continue; }
        let members: Vec<_> = group.members.iter().filter(|member| member.included && key_scope::allows_provider(scope, &member.provider)
            && provider_active(&member.provider)
            && accounts.available_providers.contains(&member.provider) && member.accounts.iter().any(|id|
                accounts.accounts.iter().any(|account| routing::account_id(account) == Some(id) && routing::provider_of(account) == member.provider && account.get("enabled") != Some(&Value::Bool(false)))))
            .filter_map(|member| catalog_model(&member.provider, &member.model)).filter(|item| !providers::images::model_is_image(item)).collect();
        if members.is_empty() { continue; }
        let mut item = json!({ "id": group.model, "name": group.name, "object": "model", "owned_by": "routing-group", "kind": "chat", "routing_group": true });
        for (key, public) in [("supportsToolCall", "supports_tool_call"), ("supportsImages", "supports_images"),
            ("supportsVideo", "supports_video"), ("supportsReasoning", "supports_reasoning")] {
            item[public] = members.iter().all(|item| item.get(key).and_then(Value::as_bool) == Some(true)).into();
        }
        for (key, public) in [("maxInputTokens", "max_input_tokens"), ("maxOutputTokens", "max_output_tokens")] {
            if members.iter().all(|item| item.get(key).and_then(Value::as_u64).is_some()) {
                item[public] = members.iter().filter_map(|item| item.get(key).and_then(Value::as_u64)).min().map(Value::from).unwrap_or(Value::Null);
            }
        }
        let mut modalities = vec!["text"];
        if item["supports_images"] == true { modalities.push("image"); }
        if item["supports_video"] == true { modalities.push("video"); }
        item["input_modalities"] = json!(modalities);
        item["created"] = 0.into();
        item["is_default"] = false.into();
        data.push(item);
    }
}

fn common_token_limit(members: &[Value], key: &str) -> Value {
    members.iter().map(|item| item.get(key).and_then(Value::as_u64))
        .collect::<Option<Vec<_>>>()
        .and_then(|values| values.into_iter().min())
        .map(Value::from)
        .unwrap_or(Value::Null)
}
