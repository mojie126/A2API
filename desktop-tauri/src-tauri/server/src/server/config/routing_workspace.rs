pub(crate) mod global_accounts;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{RoutingProfile, RoutingProfiles, CONFIG, CONFIG_UPDATE_LOCK};
use crate::server::core::{account_order, account_store::store::AccountStore, routing, routing_scope};

pub fn revision(profile: &RoutingProfile, settings: super::RoutingSettings, customized: bool) -> String {
    let value = json!({ "profile": profile, "strategy": settings.strategy.as_str(), "sticky": settings.sticky.as_str(), "customized": customized });
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}

/// 清理删除账号在路由配置中的引用
///
/// 账号 id 从排除名单与账号顺序移除，提供商没有剩余账号时从参与名单与停用名单移除，
/// 路由组的成员账号同步移除，账号 id 按凭据确定生成，不清理时重新添加会复用旧位置与排除状态
pub fn prune_deleted_accounts(store: &AccountStore, removed: &[(String, String)]) {
    if removed.is_empty() { return; }
    let deleted: HashSet<&str> = removed.iter().map(|(id, _)| id.as_str()).collect();
    let affected: HashSet<&str> = removed.iter().map(|(_, provider)| provider.as_str()).collect();
    // 读一次剩余账号：只把已经没有任何账号的家从路由名单移除，插件暂时未加载不在此列
    let remaining: HashSet<String> = routing::accounts_of(&store.list_accounts()).into_iter()
        .map(|account| routing::provider_of(&account).to_string()).collect();
    let gone: HashSet<String> = affected.iter()
        .filter(|provider| !remaining.contains(**provider)).map(|provider| provider.to_string()).collect();
    let mut pruned = false;
    let saved = super::update(|config: &mut super::RuntimeConfig| {
        let mut profiles = (*config.routing_profiles).clone();
        let mut changed = false;
        if let Some(profile) = profiles.global.as_mut() { changed |= prune_profile(profile, &deleted, &gone); }
        for profile in profiles.models.values_mut() { changed |= prune_profile(profile, &deleted, &gone); }
        for group in profiles.groups.values_mut() {
            for member in &mut group.members {
                let before = member.accounts.len();
                member.accounts.retain(|id| !deleted.contains(id.as_str()));
                changed |= member.accounts.len() != before;
            }
        }
        if !changed { return; }
        pruned = true;
        match serde_json::to_value(&profiles) {
            Ok(value) => {
                config.raw.insert(super::KEY_ROUTING_PROFILES.to_string(), value);
                config.routing_profiles = Arc::new(profiles);
            }
            Err(error) => crate::server::logging::log("[Routing]", &format!("清理删除账号的路由引用序列化失败: {error}")),
        }
    });
    if saved && pruned {
        crate::server::logging::log("[Routing]", &format!("已清理 {} 个删除账号在路由配置中的引用", removed.len()));
    }
}

fn prune_profile(profile: &mut RoutingProfile, deleted: &HashSet<&str>, gone: &HashSet<String>) -> bool {
    let mut changed = false;
    let before = profile.excluded_accounts.len();
    profile.excluded_accounts.retain(|id| !deleted.contains(id.as_str()));
    changed |= profile.excluded_accounts.len() != before;
    for ids in profile.account_order.values_mut() {
        let before = ids.len();
        ids.retain(|id| !deleted.contains(id.as_str()));
        changed |= ids.len() != before;
    }
    if !gone.is_empty() {
        let before = (profile.providers.len(), profile.disabled_providers.len(), profile.account_order.len());
        profile.providers.retain(|provider| !gone.contains(provider));
        profile.disabled_providers.retain(|provider| !gone.contains(provider));
        profile.account_order.retain(|provider, _| !gone.contains(provider));
        changed |= before != (profile.providers.len(), profile.disabled_providers.len(), profile.account_order.len());
    }
    changed
}

pub fn save_workspace(
    store: &AccountStore, model: &str, profile: Option<RoutingProfile>, expected: &str,
) -> Result<(), String> {
    let _update = CONFIG_UPDATE_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // 模型目录可能回读账号状态，必须在获取账号锁前解析。
    let available = (!model.is_empty()).then(|| crate::server::core::providers::catalog::forwarding_providers(model));
    let guard = store.guard();
    let state = store.load(&guard);
    let visible: Vec<Value> = state.accounts.iter()
        .filter(|record| crate::server::core::account_store::store_view::account_visible(&record.provider()))
        .map(|record| store.public_account(record)).collect();
    let available = available.unwrap_or_else(|| routing_scope::available_providers(&visible));
    let mut next = super::current();
    if next.routing_profiles.group_for_model(model).is_some() { return Err("此模型 ID 已被自定义路由组占用".into()); }
    let effective = routing_scope::materialize_profile_with(&visible, &available, model, &next.routing_profiles);
    let current_settings = next.routing_profiles.settings_for(model, next.routing);
    if revision(&effective, current_settings, next.routing_profiles.model_profile(model).is_some()) != expected {
        return Err("路由配置已在其他位置修改，请刷新当前配置后重试；草稿未丢失".to_string());
    }
    let mut profiles: RoutingProfiles = (*next.routing_profiles).clone();
    if model.is_empty() && (profiles.global.as_ref().is_some_and(global_accounts::has_restrictions)
        || profile.as_ref().is_some_and(global_accounts::has_restrictions)) {
        return Err("全局默认已改为账号启停，请先转换旧版全局参与设置；模型独立路由仍可设置参与名单".into());
    }
    if !model.is_empty() && profile.is_some() && !profiles.models.contains_key(model) && profiles.models.len() >= super::routing_profiles::MAX_ROUTING_MODELS {
        return Err("模型独立配置数量已达上限".to_string());
    }
    let mut priorities = Vec::new();
    if model.is_empty() {
        if let Some(profile) = &profile {
            let sorting = account_order::sorting(&visible, crate::server::logging::now_ms());
            let mut groups = account_order::manual_groups(&visible);
            for (provider, members) in &mut groups {
                if let Some(order) = profile.account_order.get(provider) {
                    let changed = effective.account_order.get(provider) != Some(order);
                    if changed {
                        crate::server::core::routing_order::global_queue(&visible, profile, Some(provider))?;
                        let positions: HashMap<_, _> = order.iter().enumerate().map(|(index, id)| (id.as_str(), index)).collect();
                        members.sort_by_key(|account| routing::account_id(account).and_then(|id| positions.get(id)).copied().unwrap_or(usize::MAX));
                    }
                }
            }
            let positions: HashMap<_, _> = profile.providers.iter().enumerate().map(|(index, id)| (id.as_str(), index)).collect();
            let mut selected: Vec<_> = groups.iter().filter(|(id, _)| positions.contains_key(id.as_str())).cloned().collect();
            selected.sort_by_key(|(id, _)| positions.get(id.as_str()).copied().unwrap_or(usize::MAX));
            let mut replacement = selected.into_iter();
            for group in &mut groups { if positions.contains_key(group.0.as_str()) { if let Some(next) = replacement.next() { *group = next; } } }
            let mut ids = groups.into_iter().flat_map(|(_, members)| members.into_iter().filter_map(|account| routing::account_id(&account).map(str::to_string)));
            let visible_ids: std::collections::HashSet<_> = sorting.account_order.values().flatten().cloned().collect();
            let mut records: Vec<_> = state.accounts.iter().collect();
            records.sort_by_key(|record| record.order_key());
            priorities = records.into_iter().enumerate().map(|(index, record)| {
                (if visible_ids.contains(record.id()) { ids.next().unwrap_or_else(|| record.id().to_string()) } else { record.id().to_string() }, index as i64)
            }).collect();
        }
        let mut global = profile;
        if let Some(profile) = &mut global {
            if let Some(strategy) = profile.strategy.take() { next.routing.strategy = super::RouteStrategy::from_id(&strategy); next.raw.insert(super::KEY_ROUTING_STRATEGY.into(), strategy.into()); }
            if let Some(sticky) = profile.sticky.take() { next.routing.sticky = super::StickyMode::from_id(&sticky); next.raw.insert(super::KEY_ROUTING_STICKY.into(), sticky.into()); }
            // 组内并发只属于模型独立路由与自定义路由组，全局默认不保存该项
            profile.concurrency_enabled = None;
            profile.concurrency = None;
            profile.concurrency_strategy = None;
            profile.account_order.clear();
        }
        profiles.global = global;
    } else if let Some(profile) = profile { profiles.models.insert(model.to_string(), profile); }
    else { profiles.models.remove(model); }
    let serialized = serde_json::to_value(&profiles).map_err(|error| error.to_string())?;
    next.raw.insert("routingProfiles".into(), serialized.clone());
    store.with_conn_mut(&guard, |conn| {
        let tx = conn.transaction()?;
        tx.execute("INSERT INTO kv(key,value) VALUES('routingProfiles',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", rusqlite::params![serialized.to_string()])?;
        if model.is_empty() {
            for key in [super::KEY_ROUTING_STRATEGY, super::KEY_ROUTING_STICKY] {
                if let Some(value) = next.raw.get(key) { tx.execute("INSERT INTO kv(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", rusqlite::params![key, value.to_string()])?; }
            }
            crate::server::core::account_store::sql::apply_priorities(&tx, &priorities)?;
        }
        tx.commit()
    }).map_err(|error| error.to_string())?;
    next.routing_profiles = Arc::new(profiles);
    crate::server::core::providers::catalog::invalidate();
    *CONFIG.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(next);
    crate::server::core::providers::catalog::invalidate();
    drop(guard);
    crate::server::core::cloud_sync::auto_sync::notify_change();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::prune_profile;
    use crate::server::config::{RoutingProfile, RoutingProviderScope};
    use std::collections::{BTreeMap, HashSet};

    fn profile() -> RoutingProfile {
        RoutingProfile {
            provider_scope: Some(RoutingProviderScope::Selected),
            providers: vec!["alpha".into(), "beta".into()],
            disabled_providers: vec!["beta".into()],
            account_order: BTreeMap::from([
                ("alpha".to_string(), vec!["a1".to_string(), "a2".to_string()]),
                ("beta".to_string(), vec!["b1".to_string()]),
            ]),
            excluded_accounts: vec!["a2".into(), "b1".into()],
            ..Default::default()
        }
    }

    #[test]
    fn prune_removes_deleted_accounts_and_empty_providers() {
        let mut profile = profile();
        let deleted: HashSet<&str> = ["a2", "b1"].into_iter().collect();
        let gone: HashSet<String> = ["beta".to_string()].into_iter().collect();
        assert!(prune_profile(&mut profile, &deleted, &gone));
        assert_eq!(profile.providers, vec!["alpha"]);
        assert!(profile.disabled_providers.is_empty());
        assert_eq!(profile.account_order.get("alpha"), Some(&vec!["a1".to_string()]));
        assert!(!profile.account_order.contains_key("beta"));
        assert!(profile.excluded_accounts.is_empty());
        assert!(!prune_profile(&mut profile, &deleted, &gone));
    }

    #[test]
    fn prune_keeps_providers_that_still_have_accounts() {
        let mut profile = profile();
        let deleted: HashSet<&str> = ["a1"].into_iter().collect();
        assert!(prune_profile(&mut profile, &deleted, &HashSet::new()));
        assert_eq!(profile.providers, vec!["alpha", "beta"]);
        assert_eq!(profile.disabled_providers, vec!["beta"]);
        assert_eq!(profile.account_order.get("alpha"), Some(&vec!["a2".to_string()]));
    }
}
