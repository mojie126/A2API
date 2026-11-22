//! 全局顺序来自账号库与消耗规则；参与范围独立于展示名单。

use std::collections::HashMap;

use serde_json::Value;

use crate::server::config::{self, RoutingProfile, RoutingProviderScope};
use super::{account_order, routing};

/**
 * 模型独立路由的参与名单补齐，新出现的提供商（新添加账号带来的家）自动参与，
 * 明确停用的提供商保持排除
 */
pub fn include_new_providers(profile: &mut RoutingProfile, candidates: &[String]) {
    for provider in candidates {
        if !profile.providers.contains(provider) && !profile.disabled_providers.iter().any(|id| id == provider) {
            profile.providers.push(provider.clone());
        }
    }
}

/**
 * 账号库中出现的提供商，按首次出现顺序返回
 */
pub fn account_providers(accounts: &[Value]) -> Vec<String> {
    let mut ids = Vec::new();
    for account in accounts {
        let id = routing::provider_of(account);
        if !ids.iter().any(|known| known == id) { ids.push(id.to_string()); }
    }
    ids
}

pub fn available_providers(accounts: &[Value]) -> Vec<String> {
    let mut sorted = accounts.to_vec();
    sorted.sort_by_key(|account| account.get("priority").and_then(Value::as_i64).unwrap_or(i64::MAX));
    let mut ids = Vec::new();
    for account in sorted {
        let id = routing::provider_of(&account).to_string();
        if !ids.contains(&id) { ids.push(id); }
    }
    for id in crate::server::core::plugin_host::installed_ids() {
        if !ids.contains(&id) { ids.push(id); }
    }
    for provider in crate::server::core::custom_providers::list() {
        if let Some(id) = provider.get("id").and_then(Value::as_str) {
            if !ids.iter().any(|known| known == id) { ids.push(id.to_string()); }
        }
    }
    ids
}

pub fn providers_for(model: &str, available: &[String]) -> Vec<String> {
    let profiles = config::routing_profiles();
    match profiles.effective(model) {
        Some(profile) => {
            let independent = profiles.model_profile(model).is_some();
            let mut profile = profile.clone();
            // 模型独立路由不再按保存时的名单排除后来新出现的家
            if independent { include_new_providers(&mut profile, available); }
            let mut providers = if independent {
                profile.providers.clone()
            } else {
                available.to_vec()
            };
            if profile.scope() == RoutingProviderScope::All {
                for provider in available {
                    if !providers.contains(provider) { providers.push(provider.clone()); }
                }
            }
            providers.retain(|id| available.contains(id) && profile.includes_provider(id));
            providers
        }
        None => available.to_vec(),
    }
}

pub fn apply_accounts(accounts: &[Value], model: &str) -> Vec<Value> {
    let profiles = config::routing_profiles();
    let mut ordered = if let Some(profile) = profiles.model_profile(model) {
        let mut profile = profile.clone();
        include_new_providers(&mut profile, &account_providers(accounts));
        project_accounts(accounts, &profile)
    } else {
        let sorting = account_order::sorting(accounts, crate::server::logging::now_ms());
        let mut ordered = account_order::ordered_accounts(accounts, &sorting);
        if let Some(profile) = &profiles.global {
            ordered.retain(|account| profile.includes_provider(routing::provider_of(account))
                && !routing::account_id(account).is_some_and(|id| profile.excluded_accounts.iter().any(|excluded| excluded == id)));
        }
        ordered
    };
    for (index, account) in ordered.iter_mut().enumerate() {
        if let Some(object) = account.as_object_mut() {
            object.insert("routingPriority".to_string(), Value::from(index));
        }
    }
    ordered
}

pub fn project_accounts(accounts: &[Value], profile: &RoutingProfile) -> Vec<Value> {
    let sorting = account_order::sorting(accounts, crate::server::logging::now_ms());
    let baseline = account_order::ordered_accounts(accounts, &sorting);
    let mut providers = profile.providers.clone();
    if profile.scope() == RoutingProviderScope::All {
        for provider in &sorting.provider_order {
            if !providers.contains(provider) { providers.push(provider.clone()); }
        }
    }
    let mut ordered = Vec::new();
    for provider in &providers {
        if !profile.includes_provider(provider) { continue; }
        let mut members: Vec<Value> = baseline.iter()
            .filter(|account| routing::provider_of(account) == provider)
            .filter(|account| routing::account_id(account)
                .is_some_and(|id| !profile.excluded_accounts.iter().any(|excluded| excluded == id)))
            .cloned().collect();
        if let Some(ids) = profile.account_order.get(provider).or_else(|| sorting.account_order.get(provider)) {
            let positions: HashMap<&str, usize> = ids.iter().enumerate().map(|(index, id)| (id.as_str(), index)).collect();
            members.sort_by_key(|account| routing::account_id(account)
                .and_then(|id| positions.get(id)).copied().unwrap_or(usize::MAX));
        }
        ordered.extend(members);
    }
    ordered
}

pub fn materialize_profile(accounts: &[Value], available: &[String], model: &str) -> RoutingProfile {
    materialize_profile_with(accounts, available, model, &config::routing_profiles())
}

pub fn materialize_profile_with(accounts: &[Value], available: &[String], model: &str, profiles: &config::RoutingProfiles) -> RoutingProfile {
    let sorting = account_order::sorting(accounts, crate::server::logging::now_ms());
    let independent = profiles.model_profile(model).is_some();
    let mut profile = profiles.effective(model).cloned().unwrap_or_else(|| RoutingProfile {
        provider_scope: Some(RoutingProviderScope::All),
        providers: available.to_vec(),
        ..Default::default()
    });
    let scope = profile.scope();
    profile.provider_scope = Some(scope);
    // 模型独立路由：名单外的新提供商自动参与，明确停用的保持不变
    if independent { include_new_providers(&mut profile, available); }
    if !independent {
        let mut providers = sorting.provider_order.clone();
        providers.extend(available.iter().filter(|id| !sorting.provider_order.contains(id)).cloned());
        for id in profile.providers.iter().chain(&profile.disabled_providers) {
            if !providers.contains(id) { providers.push(id.clone()); }
        }
        if scope == RoutingProviderScope::Selected {
            providers.retain(|id| profile.providers.contains(id));
        }
        profile.providers = providers;
        profile.account_order.clear();
    }
    if !independent && !model.is_empty() {
        profile.providers.retain(|id| available.contains(id));
        if scope == RoutingProviderScope::Selected {
            profile.disabled_providers.retain(|id| profile.providers.contains(id));
        }
    }
    for provider in &profile.providers {
        let ids = profile.account_order.entry(provider.clone()).or_default();
        for id in sorting.account_order.get(provider).into_iter().flatten() {
            if !ids.contains(id) { ids.push(id.clone()); }
        }
    }
    profile
}

#[cfg(test)]
mod tests {
    use super::include_new_providers;
    use crate::server::config::{RoutingProfile, RoutingProviderScope};

    fn profile(scope: RoutingProviderScope, providers: &[&str], disabled: &[&str]) -> RoutingProfile {
        RoutingProfile {
            provider_scope: Some(scope),
            providers: providers.iter().map(|id| id.to_string()).collect(),
            disabled_providers: disabled.iter().map(|id| id.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn include_new_providers_adds_unknown_and_keeps_explicit_removal() {
        let mut profile = profile(RoutingProviderScope::Selected, &["zcode", "raccoon"], &["raccoon"]);
        include_new_providers(&mut profile, &["zcode".into(), "raccoon".into(), "new-a".into(), "new-b".into()]);
        assert_eq!(profile.providers, vec!["zcode", "raccoon", "new-a", "new-b"]);
        assert!(profile.includes_provider("zcode"));
        assert!(!profile.includes_provider("raccoon"));
        assert!(profile.includes_provider("new-a"));
    }

    #[test]
    fn include_new_providers_skips_disabled_even_when_unlisted() {
        let mut profile = profile(RoutingProviderScope::All, &[], &["blocked"]);
        include_new_providers(&mut profile, &["blocked".into(), "fresh".into()]);
        assert_eq!(profile.providers, vec!["fresh"]);
    }
}
