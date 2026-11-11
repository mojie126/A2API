//! 商汤日日新手机号验证码登录（自定义提供商账号的余额查询凭证）。
//!
//! 上游已关闭控制台账号密码登录，余额查询需要先完成手机号 + 短信验证码登录，
//! 再持久化 OAuth access / refresh token。此模块只负责把 HTTP 形状翻译成 core
//! 调用，短信登录的授权链路在 `core::providers::custom::sensenova_auth`。
//!
//! 端点：
//! - `POST /api/sensenova/sms/send`：发码，返回一次性 ticket 与脱敏手机号
//! - `POST /api/sensenova/sms/captcha`：提交官网滑块结果，通过后重新发码
//! - `POST /api/sensenova/sms/verify`：校验验证码并写入目标账号
//!
//! `target` 由请求体判别：
//! - `accountId`：更新现有账号的余额查询凭证
//! - `providerId`：给已有自定义提供商添加账号（需要 `apiKey`）
//! - `createProvider`：新建自定义提供商并添加首个账号

use axum::body::Bytes;
use axum::extract::State;
use axum::response::Response;
use serde_json::{json, Value};

use crate::server::core::account_store::AccountStoreError;
use crate::server::core::custom_providers;
use crate::server::core::proxies::ResolvedProxy;
use crate::server::core::providers::custom::sensenova_auth::{
    self, SmsLoginOutcome, SmsStartOutcome,
};
use crate::server::errors::management_error;
use crate::server::http::{ok_json, parse_body};
use crate::server::ServerState;

fn body_object(body: &Bytes) -> Result<serde_json::Map<String, Value>, Response> {
    let payload = parse_body(body).map_err(|error| management_error(400, error.message))?;
    payload
        .as_object()
        .cloned()
        .ok_or_else(|| management_error(400, "请求体必须是 JSON 对象"))
}

fn text(object: &serde_json::Map<String, Value>, key: &str) -> String {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

fn resolved_proxy(
    payload: &serde_json::Map<String, Value>,
) -> Result<Option<ResolvedProxy>, Response> {
    let selection = super::session::checked_login_proxy(payload.get("proxy"))
        .map_err(|error| management_error(error.status_code, error.message))?;
    let Some(selection) = selection else { return Ok(None) };
    ResolvedProxy::from_json(&selection.resolved)
        .map_err(|message| management_error(400, message))
}

fn account_public(state: &ServerState, id: &str) -> Value {
    state
        .store()
        .list_visible_accounts()
        .get("accounts")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("id").and_then(Value::as_str) == Some(id))
        })
        .cloned()
        .unwrap_or(Value::Null)
}

fn store_error(error: AccountStoreError) -> Response {
    management_error(error.status_code, error.message)
}

/// 发短信验证码。
pub async fn send_sms(State(state): State<ServerState>, body: Bytes) -> Response {
    let object = match body_object(&body) {
        Ok(object) => object,
        Err(response) => return response,
    };
    let mut phone = text(&object, "phone");
    let account_id = text(&object, "accountId");
    if phone.is_empty() && !account_id.is_empty() {
        let Some(query) = state.store().custom_balance_query_by_id(&account_id) else {
            return management_error(404, "账号不存在");
        };
        phone = query.phone;
    }
    if phone.is_empty() {
        return management_error(400, "请填写手机号");
    }
    let mut proxy = match resolved_proxy(&object) {
        Ok(proxy) => proxy,
        Err(response) => return response,
    };
    if proxy.is_none() && !account_id.is_empty() {
        if let Some(credential) = state.store().custom_credential_by_id(&account_id) {
            if let Some(message) = credential.proxy_error {
                return management_error(502, message);
            }
            proxy = credential.proxy;
        }
    }
    match sensenova_auth::begin_sms_login(&phone, proxy.as_ref()).await {
        Ok(SmsStartOutcome::Sent(challenge)) => ok_json(json!({
            "status": "sent",
            "ticket": challenge.ticket,
            "phoneMasked": challenge.phone_masked,
        })),
        Ok(SmsStartOutcome::Captcha(captcha)) => ok_json(json!({
            "status": "captcha",
            "ticket": captcha.ticket,
            "phoneMasked": captcha.phone_masked,
            "captcha": {
                "codeKey": captcha.code_key,
                "image": captcha.image,
                "block": captcha.block,
            },
        })),
        Err(message) => management_error(502, message),
    }
}

/// 提交官网滑块验证，通过后重新发码。
pub async fn verify_captcha(State(state): State<ServerState>, body: Bytes) -> Response {
    let object = match body_object(&body) {
        Ok(object) => object,
        Err(response) => return response,
    };
    let ticket = text(&object, "ticket");
    let code_key = text(&object, "codeKey");
    let code_value = object
        .get("codeValue")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32;
    let account_id = text(&object, "accountId");
    let mut proxy = match resolved_proxy(&object) {
        Ok(proxy) => proxy,
        Err(response) => return response,
    };
    if proxy.is_none() && !account_id.is_empty() {
        if let Some(credential) = state.store().custom_credential_by_id(&account_id) {
            if let Some(message) = credential.proxy_error {
                return management_error(502, message);
            }
            proxy = credential.proxy;
        }
    }
    match sensenova_auth::complete_sms_captcha(&ticket, &code_key, code_value, proxy.as_ref())
        .await
    {
        Ok(SmsStartOutcome::Sent(challenge)) => ok_json(json!({
            "status": "sent",
            "ticket": challenge.ticket,
            "phoneMasked": challenge.phone_masked,
        })),
        Ok(SmsStartOutcome::Captcha(captcha)) => ok_json(json!({
            "status": "captcha",
            "ticket": captcha.ticket,
            "phoneMasked": captcha.phone_masked,
            "captcha": {
                "codeKey": captcha.code_key,
                "image": captcha.image,
                "block": captcha.block,
            },
        })),
        Err(message) => management_error(502, message),
    }
}

/// 校验验证码并写入账号。
pub async fn verify_sms(State(state): State<ServerState>, body: Bytes) -> Response {
    let object = match body_object(&body) {
        Ok(object) => object,
        Err(response) => return response,
    };
    let ticket = text(&object, "ticket");
    let code = text(&object, "code");
    let mut phone = text(&object, "phone");
    let account_id = text(&object, "accountId");
    let provider_id = text(&object, "providerId");
    let create_provider = object.get("createProvider").cloned();

    if phone.is_empty() && !account_id.is_empty() {
        if let Some(query) = state.store().custom_balance_query_by_id(&account_id) {
            phone = query.phone;
        }
    }
    if phone.is_empty() {
        return management_error(400, "缺少手机号");
    }

    let mut proxy = match resolved_proxy(&object) {
        Ok(proxy) => proxy,
        Err(response) => return response,
    };
    if proxy.is_none() && !account_id.is_empty() {
        if let Some(credential) = state.store().custom_credential_by_id(&account_id) {
            if let Some(message) = credential.proxy_error {
                return management_error(502, message);
            }
            proxy = credential.proxy;
        }
    }
    let outcome = match sensenova_auth::complete_sms_login(
        &ticket,
        &code,
        object.get("tenant"),
        proxy.as_ref(),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(message) => return management_error(502, message),
    };
    let result = match outcome {
        SmsLoginOutcome::Success(result) => result,
        SmsLoginOutcome::TenantRequired(tenants) => {
            return ok_json(json!({
                "status": "tenant_required",
                "tenants": tenants,
            }));
        }
    };

    if !account_id.is_empty() {
        if let Err(error) = state.store().store_custom_balance_sensenova_login(
            &account_id,
            &phone,
            &result.access_token,
            &result.refresh_token,
            result.expires_at_ms,
        ) {
            return store_error(error);
        }
        let account = account_public(&state, &account_id);
        return ok_json(json!({
            "status": "success",
            "account": account,
            "list": state.store().list_visible_accounts(),
        }));
    }

    let store = state.store();
    let mut created_provider = false;
    let target_provider = if let Some(create) = create_provider {
        let Some(create_object) = create.as_object() else {
            return management_error(400, "createProvider 必须是 JSON 对象");
        };
        let mut provider_payload = create_object.clone();
        provider_payload
            .entry("balanceScheme".to_string())
            .or_insert_with(|| Value::String("sensenova".to_string()));
        let provider = match custom_providers::create(&Value::Object(provider_payload)) {
            Ok(provider) => provider,
            Err(message) => return management_error(400, message),
        };
        created_provider = true;
        provider
    } else if !provider_id.is_empty() {
        let Some(provider) = custom_providers::get(&provider_id) else {
            return management_error(404, "自定义提供商不存在");
        };
        provider
    } else {
        return management_error(400, "缺少账号或提供商上下文");
    };

    let Some(target_id) = target_provider.get("id").and_then(Value::as_str) else {
        return management_error(500, "提供商记录缺少 id");
    };
    // 账号仍保留转发用的 API Key；余额查询凭证由后面的专用写入补上。
    let mut account_payload = object.clone();
    account_payload.remove("ticket");
    account_payload.remove("code");
    account_payload.remove("phone");
    account_payload.remove("accountId");
    account_payload.remove("providerId");
    account_payload.remove("createProvider");
    account_payload.remove("tenant");
    account_payload.insert(
        "provider".to_string(),
        Value::String(target_id.to_string()),
    );
    let name = text(&object, "name");
    let upsert = match store.add_custom_account(
        target_id,
        &Value::Object(account_payload),
        (!name.is_empty()).then_some(name.as_str()),
    ) {
        Ok(upsert) => upsert,
        Err(error) => {
            if created_provider {
                let _ = custom_providers::remove(target_id, store);
            }
            return store_error(error);
        }
    };
    let account_id = upsert
        .account
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if upsert.created && !account_id.is_empty() {
        if let Err(error) = store.store_custom_balance_sensenova_phone(&account_id, &phone) {
            let _ = store.remove_account(&account_id);
            if created_provider {
                let _ = custom_providers::remove(target_id, store);
            }
            return store_error(error);
        }
    }
    if let Err(error) = store.store_custom_balance_sensenova_login(
        &account_id,
        &phone,
        &result.access_token,
        &result.refresh_token,
        result.expires_at_ms,
    ) {
        if created_provider {
            let _ = custom_providers::remove(target_id, store);
        } else if upsert.created && !account_id.is_empty() {
            let _ = store.remove_account(&account_id);
        }
        return store_error(error);
    }
    ok_json(json!({
        "status": "success",
        "account": account_public(&state, &account_id),
        "provider": target_provider,
        "created": upsert.created,
        "list": store.list_visible_accounts(),
    }))
}
