//! 商汤日日新（SenseNova）**手机号验证码登录**：OAuth2 授权码 + PKCE 换 JWT。
//!
//! ── 为什么余额查询要自己登录（这一家的特别之处）──────────────
//! 余额接口（`platform.sensenova.cn` 的 tokenplan/pool-usage）只认控制台的
//! Bearer JWT，而**推理用的 API Key 换不到它** —— 两者是两套平面（开放平台
//! API Key 打推理，控制台 JWT 打账户/积分）。上游已经不支持控制台账号密码
//! 登录，用户手上能提供的是绑定手机号与短信验证码，所以查询链路先走手机号
//! 验证码授权，再缓存 refresh token 自动续期（移植来源：OmniProxy 的
//! `server/src/utils/sensenovaAuth.ts` 与官网登录页构建产物，逐步对读）。
//!
//! ── 流程（四步，与源实现逐条对齐）────────────────────────────
//!   1. `GET {platform}/oauth2/auth?...`（PKCE + state）—— 跟随跳转直到不带
//!      location，从**最终地址**的 query 里取 `login_challenge`；
//!   2. `POST {iam}/iam/authn/v1/auth/nova/sendSmsCode` `{phone, region_code}`
//!      → `token_code`；若为空，则按 `getCaptcha` / `checkCaptcha` 先完成
//!      官网滑块，再带 `code_key` 重新发码；
//!   3. `POST {iam}/iam/authn/v1/auth/nova/smsLogin` `{token_code, verify_code, challenge}`；
//!   4. 从登录响应或租户选择的 redirect 继续跟随（带 Cookie），直到地址里出现 `code=`；
//!   5. `POST {platform}/oauth2/token`（authorization_code + code_verifier）
//!      → `access_token`、`refresh_token`。
//!   6. access token 临期时用 `refresh_token` 换新，轮换后的 refresh token
//!      原子回写账号记录；没有 refresh token 的老记录要求重新手机号验证码登录。
//! 站点常量照抄源实现的「网页端配置」：signinUrl / callbackUrl 都是
//! `platform.sensenova.cn`，IAM 域是 `iam.sensecoreapi.cn`，clientId = `nova`。
//!
//! ── 为什么自己跟跳转、自己管 Cookie ──────────────────────────
//! `auth_http::send_raw` 走 reqwest 的默认跳转策略（自动跟，但**读不到
//! Set-Cookie**），而这条链路每一步都要带着会话 Cookie 走下一跳。源实现用
//! `redirect: 'manual'` + 一个极简 Cookie Jar（只认 `name=value`，不处理
//! Domain / Path / 过期）——那套在这里逐条复刻：这条流程里的 Cookie 全是
//! 同一站点的会话票，按「同名覆盖」存即可，引入完整 Cookie 引擎得不偿失。
//!
//! ── JWT 的两个读数（过期判定与 tenant_id）────────────────────
//! 余额接口要 `?account_id=<JWT payload 的 ext.tenant_id>`，缓存判定要
//! `exp`（提前 60 秒视为过期，与源实现同口径）。两者都只读 payload 段，
//! **不校验签名**：这是网关自己拿到的票，不是需要防伪造的入口数据。
//!
//! ── 短信登录为什么需要网关侧暂存 ───────────────────────────────
//! 上游把短信验证码绑在发码时的 `token_code` 与 PKCE 授权会话上，第二阶段
//! 必须原样带回。凭证不能只放在前端（会经调试报文与请求日志落盘），所以把
//! challenge、verifier、state 与 Cookie 存在进程内，前端只拿一次性 ticket。
//! ticket 十分钟过期，验证码校验成功即删除；网关重启会让未完成的登录失效，
//! 用户重新发码即可。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap / expect / panic。
//! 失败一律返回可读的 `String`（调用方拼进余额查询的错误文案）。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use url::Url;

use crate::server::core::egress;
use crate::server::core::providers::custom::sensenova_captcha;
use crate::server::core::proxies::ResolvedProxy;
use crate::server::core::user_agent::AccountRequest;
use crate::server::logging;

/// 控制台（平台）域：授权、换码、余额接口的**同一站点**
pub(super) const PLATFORM_BASE: &str = "https://platform.sensenova.cn";
/// 余额接口的基址（`/lite` 一级，与 token 平面不同路径）
pub(super) const PLATFORM_LITE_BASE: &str = "https://platform.sensenova.cn/lite";
/// IAM 域：短信登录与官网人机验证接口
const IAM_BASE: &str = "https://iam.sensecoreapi.cn";
/// OAuth 客户端标识（网页端配置里的 clientId，逐字照抄）
const CLIENT_ID: &str = "nova";
/// 授权 scope（离线刷新那两项是网页端的原值）
const SCOPE: &str = "openid offline offline_access";
/// 手机号验证码登录的固定区号（当前产品只开放大陆号段）
const REGION_CODE: &str = "86";
/// 跟跳转的步数上限（源实现 12）
const MAX_REDIRECTS: usize = 12;
/// 单步请求超时：登录链路是案例式动作（不像余额每轮都跑），给 20 秒
const STEP_TIMEOUT_MS: u64 = 20_000;
/// 短信登录中间态的有效期（与常见的验证码有效期同量级）
const SMS_PENDING_TTL_MS: i64 = 10 * 60 * 1000;
/// UA：登录页面对非常规 UA 有风控，照源实现固定一个普通浏览器 UA
const USER_AGENT: &str = "Mozilla/5.0";
/// 过期判定的提前量（秒）：与源实现同口径，避开边界竞态
const EXPIRY_SKEW_SECONDS: f64 = 60.0;

/// 一次登录的结果（只留余额链路要用的两样）
pub(crate) struct LoginResult {
    /// 控制台 JWT
    pub access_token: String,
    /// OAuth refresh token（旧密码登录链路可能为空）
    pub refresh_token: String,
    /// 到期时刻（毫秒；解析不出 exp 时给 0 = 不确定，调用方当它不能缓存）
    pub expires_at_ms: i64,
}

/// 发码成功后的公开信息：前端只拿一次 ticket 与脱敏手机号。
pub(crate) struct SmsChallenge {
    pub ticket: String,
    pub phone_masked: String,
}

/// 官网发码前的人机验证。
pub(crate) struct SmsCaptcha {
    pub ticket: String,
    pub phone_masked: String,
    pub code_key: String,
    pub image: String,
    pub block: String,
}

/// 发码阶段可能直接成功，也可能先要求人机验证。
pub(crate) enum SmsStartOutcome {
    Sent(SmsChallenge),
    Captcha(SmsCaptcha),
}

/// 手机号验证码登录完成后可能还要选择租户。
pub(crate) enum SmsLoginOutcome {
    Success(LoginResult),
    TenantRequired(Vec<Value>),
}

/// 网关侧暂存的短信登录中间态（见模块头）。
#[derive(Clone)]
struct PendingSmsLogin {
    phone: String,
    challenge: String,
    verifier: String,
    state: String,
    token_code: String,
    cookies: CookieJar,
    tenants: Vec<Value>,
    expires_at_ms: i64,
}

/// 进程内待完成短信登录表。
static PENDING_SMS_LOGINS: OnceLock<Mutex<HashMap<String, PendingSmsLogin>>> = OnceLock::new();

/// JWT 是否还能用（`exp` 有效且距到期还有 [`EXPIRY_SKEW_SECONDS`] 以上）。
/// 解析不出 payload / 没有 `exp` 一律视为不能用 —— 宁多重登一次。
pub(super) fn jwt_reusable(jwt: &str) -> bool {
    let Some(payload) = jwt_payload(jwt) else { return false };
    let Some(exp) = payload.get("exp").and_then(Value::as_f64) else { return false };
    if !(exp > 0.0) {
        return false;
    }
    let now = logging::now_ms() as f64 / 1000.0;
    now < exp - EXPIRY_SKEW_SECONDS
}

/// JWT → `exp` 的毫秒时刻（解析不出给 0）。
pub(super) fn jwt_expires_at_ms(jwt: &str) -> i64 {
    jwt_payload(jwt)
        .and_then(|payload| payload.get("exp").and_then(Value::as_f64))
        .filter(|exp| *exp > 0.0)
        .map(|exp| (exp * 1000.0) as i64)
        .unwrap_or(0)
}

/// JWT → 账户 id（`ext.tenant_id`，回落顶层 `tenant_id`）。
///
/// 余额接口的 `account_id` 参数就是它 —— 缺了这一项查询没法发出去，
/// 所以调用方拿到 `None` 时按「登录成功但票不可用」处理。
pub(super) fn jwt_tenant_id(jwt: &str) -> Option<String> {
    let payload = jwt_payload(jwt)?;
    let candidate = payload
        .get("ext")
        .and_then(|ext| ext.get("tenant_id"))
        .or_else(|| payload.get("tenant_id"))?;
    let text = match candidate {
        Value::String(text) => text.trim().to_string(),
        Value::Number(number) => number.to_string(),
        _ => String::new(),
    };
    (!text.is_empty()).then_some(text)
}

/// JWT payload 段 → JSON（base64url，容错末尾 `=`；坏值给 None）
fn jwt_payload(jwt: &str) -> Option<Value> {
    let segment = jwt.split('.').nth(1)?;
    let text = segment.trim();
    if text.is_empty() {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(text.trim_end_matches('=').as_bytes())
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// 发起手机号验证码登录：走授权页取 challenge，再发短信。
///
/// 只把一次性 ticket 与脱敏手机号交给前端；`token_code`、PKCE verifier、state
/// 与 Cookie 留在网关进程内，避免它们进入调试报文或前端存储。
pub(crate) async fn begin_sms_login(
    phone: &str,
    proxy: Option<&ResolvedProxy>,
) -> Result<SmsStartOutcome, String> {
    let phone = phone.trim();
    if !valid_phone(phone) {
        return Err("请输入 11 位大陆手机号".to_string());
    }
    let verifier = random_secret()?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_hex()?;
    let client = egress::client_without_redirects(proxy);
    let mut jar = CookieJar::default();

    let auth_url = authorize_url(&challenge, &state)?;
    let followed = follow(&client, &auth_url, &mut jar).await?;
    if followed.status >= 400 {
        return Err(format!("商汤授权页返回 HTTP {}", followed.status));
    }
    let login_challenge = query_param(&followed.url, "login_challenge")
        .ok_or_else(|| "未能从商汤授权页取到登录 challenge".to_string())?;

    let body = json!({
        "phone": phone,
        "region_code": REGION_CODE,
    });
    let (status, payload) = post_json_with_jar(
        &client,
        &format!("{IAM_BASE}/iam/authn/v1/auth/nova/sendSmsCode"),
        &body,
        &mut jar,
    )
    .await?;
    let token_code = payload
        .get("token_code")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if status >= 400 {
        let message = payload
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| payload.get("error").and_then(Value::as_str))
            .map(str::to_string)
            .unwrap_or_else(|| format!("HTTP {status}"));
        return Err(format!("商汤短信验证码发送失败：{}", shorten(&message)));
    }

    let ticket = random_secret()?;
    let pending = PendingSmsLogin {
        phone: phone.to_string(),
        challenge: login_challenge,
        verifier,
        state,
        token_code,
        cookies: jar,
        tenants: Vec::new(),
        expires_at_ms: logging::now_ms() + SMS_PENDING_TTL_MS,
    };
    let sent = {
        let mut table = pending_table()
            .lock()
            .map_err(|_| "商汤短信登录暂存区不可用".to_string())?;
        prune_pending(&mut table);
        let sent = !pending.token_code.is_empty();
        table.insert(ticket.clone(), pending);
        sent
    };
    if sent {
        return Ok(SmsStartOutcome::Sent(SmsChallenge {
            ticket,
            phone_masked: mask_phone(phone),
        }));
    }
    request_captcha(&ticket, phone, proxy).await
}

/// 用户完成滑块后校验人机验证，并重新发码。
pub(crate) async fn complete_sms_captcha(
    ticket: &str,
    code_key: &str,
    code_value: u32,
    proxy: Option<&ResolvedProxy>,
) -> Result<SmsStartOutcome, String> {
    let ticket = ticket.trim();
    let code_key = code_key.trim();
    if ticket.is_empty() || code_key.is_empty() {
        return Err("人机验证会话已失效，请重新发送验证码".to_string());
    }
    let pending = {
        let mut table = pending_table()
            .lock()
            .map_err(|_| "商汤短信登录暂存区不可用".to_string())?;
        prune_pending(&mut table);
        table
            .get(ticket)
            .cloned()
            .ok_or_else(|| "人机验证会话已过期，请重新发送验证码".to_string())?
    };
    let client = egress::client_without_redirects(proxy);
    let mut jar = pending.cookies.clone();
    let check_url = format!(
        "{IAM_BASE}/iam/authn/v1/auth/checkCaptcha?code_key={}&code_value={}",
        url::form_urlencoded::byte_serialize(code_key.as_bytes()).collect::<String>(),
        code_value
    );
    let (status, payload) = get_json_with_jar(&client, &check_url, &mut jar).await?;
    let passed = payload.get("result").and_then(Value::as_bool).unwrap_or(false);
    if status >= 400 || !passed {
        return Err("人机验证未通过，请重新拖动滑块".to_string());
    }
    let (status, payload) = post_json_with_jar(
        &client,
        &format!("{IAM_BASE}/iam/authn/v1/auth/nova/sendSmsCode"),
        &json!({
            "phone": pending.phone,
            "region_code": REGION_CODE,
            "code_key": code_key,
        }),
        &mut jar,
    )
    .await?;
    let token_code = payload
        .get("token_code")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if status >= 400 || token_code.is_empty() {
        return Err("人机验证已通过，但短信验证码仍未发出，请重试".to_string());
    }
    let mut table = pending_table()
        .lock()
        .map_err(|_| "商汤短信登录暂存区不可用".to_string())?;
    if let Some(item) = table.get_mut(ticket) {
        item.token_code = token_code;
        item.cookies = jar;
    }
    Ok(SmsStartOutcome::Sent(SmsChallenge {
        ticket: ticket.to_string(),
        phone_masked: mask_phone(&pending.phone),
    }))
}

/// 向官网取人机验证图片，并回传前端拖动的 challenge。
async fn request_captcha(
    ticket: &str,
    phone: &str,
    proxy: Option<&ResolvedProxy>,
) -> Result<SmsStartOutcome, String> {
    let mut jar = {
        let table = pending_table()
            .lock()
            .map_err(|_| "商汤短信登录暂存区不可用".to_string())?;
        table
            .get(ticket)
            .map(|item| item.cookies.clone())
            .ok_or_else(|| "人机验证会话已过期，请重新发送验证码".to_string())?
    };
    let client = egress::client_without_redirects(proxy);
    let (status, payload) =
        get_json_with_jar(&client, &format!("{IAM_BASE}/iam/authn/v1/auth/getCaptcha"), &mut jar)
            .await?;
    let code_key = payload
        .get("code_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let image = payload
        .get("image")
        .and_then(Value::as_str)
        .map(|text| clean_base64(text))
        .unwrap_or_default()
        .to_string();
    let block = payload
        .get("block")
        .and_then(Value::as_str)
        .map(|text| clean_base64(text))
        .unwrap_or_default()
        .to_string();
    if status >= 400 || code_key.is_empty() || image.is_empty() || block.is_empty() {
        return Err("商汤人机验证初始化失败，请稍后重试".to_string());
    }
    if let Ok(mut table) = pending_table().lock() {
        if let Some(item) = table.get_mut(ticket) {
            item.cookies = jar;
        }
    }
    // 先自己解一遍：滑块是纯图像题，缺口位置就在背景图上（见
    // `sensenova_captcha`）。解出来就直接提交，省掉用户手工拖动；
    // 解不出来（图像形态变了）或上游判错，再回落到人工拖动那一路。
    if let Some(offset) = sensenova_captcha::solve_offset(&image, &block) {
        match complete_sms_captcha(ticket, &code_key, offset, proxy).await {
            Ok(outcome) => {
                logging::verbose("sensenova", &format!("captcha solved automatically, offset {offset}"));
                return Ok(outcome);
            }
            Err(message) => {
                logging::verbose("sensenova", &format!("captcha auto-solve rejected (offset {offset}): {message}"));
                // 求解没通过：上游多半已作废这一张图，重新取一张交回用户
                return request_captcha_for_user(ticket, phone, proxy).await;
            }
        }
    }
    logging::verbose("sensenova", "captcha auto-solve found no match, falling back to manual");
    Ok(SmsStartOutcome::Captcha(SmsCaptcha {
        ticket: ticket.to_string(),
        phone_masked: mask_phone(phone),
        code_key,
        image,
        block,
    }))
}

/// 重新取一张人机验证图，交给用户手工拖动。
///
/// 自动求解失败时走这里：上游判错意味着当前这张图已经作废，继续让用户拖
/// 它只会白拖一次，因此换成新图。
async fn request_captcha_for_user(
    ticket: &str,
    phone: &str,
    proxy: Option<&ResolvedProxy>,
) -> Result<SmsStartOutcome, String> {
    let mut jar = {
        let table = pending_table()
            .lock()
            .map_err(|_| "商汤短信登录暂存区不可用".to_string())?;
        table
            .get(ticket)
            .map(|item| item.cookies.clone())
            .ok_or_else(|| "人机验证会话已过期，请重新发送验证码".to_string())?
    };
    let client = egress::client_without_redirects(proxy);
    let (status, payload) =
        get_json_with_jar(&client, &format!("{IAM_BASE}/iam/authn/v1/auth/getCaptcha"), &mut jar)
            .await?;
    let code_key = payload
        .get("code_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let image = payload
        .get("image")
        .and_then(Value::as_str)
        .map(clean_base64)
        .unwrap_or_default();
    let block = payload
        .get("block")
        .and_then(Value::as_str)
        .map(clean_base64)
        .unwrap_or_default();
    if status >= 400 || code_key.is_empty() || image.is_empty() || block.is_empty() {
        return Err("商汤人机验证初始化失败，请稍后重试".to_string());
    }
    if let Ok(mut table) = pending_table().lock() {
        if let Some(item) = table.get_mut(ticket) {
            item.cookies = jar;
        }
    }
    Ok(SmsStartOutcome::Captcha(SmsCaptcha {
        ticket: ticket.to_string(),
        phone_masked: mask_phone(phone),
        code_key,
        image,
        block,
    }))
}

/// 校验短信验证码并完成授权码换票。
///
/// `tenant` 为官网多租户选择列表中的一项；单租户时由网关自动取唯一项。
pub(crate) async fn complete_sms_login(
    ticket: &str,
    code: &str,
    tenant: Option<&Value>,
    proxy: Option<&ResolvedProxy>,
) -> Result<SmsLoginOutcome, String> {
    let ticket = ticket.trim();
    let code = code.trim();
    if ticket.is_empty() {
        return Err("短信登录会话已失效，请重新发送验证码".to_string());
    }
    if !code.is_empty() && !(code.len() == 6 && code.bytes().all(|byte| byte.is_ascii_digit())) {
        return Err("请填写 6 位数字验证码".to_string());
    }
    let pending = {
        let mut table = pending_table()
            .lock()
            .map_err(|_| "商汤短信登录暂存区不可用".to_string())?;
        prune_pending(&mut table);
        table
            .get(ticket)
            .cloned()
            .ok_or_else(|| "短信登录会话已过期，请重新发送验证码".to_string())?
    };
    if pending.expires_at_ms <= logging::now_ms() {
        return Err("短信登录会话已过期，请重新发送验证码".to_string());
    }
    if !pending.tenants.is_empty() {
        let selected = tenant.ok_or_else(|| "请选择商汤账号所属租户".to_string())?;
        let client = egress::client_without_redirects(proxy);
        let mut jar = pending.cookies.clone();
        return complete_tenant_login(&client, &pending, selected, &mut jar).await;
    }
    if code.is_empty() {
        return Err("请填写 6 位数字验证码".to_string());
    }

    let client = egress::client_without_redirects(proxy);
    let body = json!({
        "token_code": pending.token_code,
        "verify_code": code,
        "challenge": pending.challenge,
    });
    let mut jar = pending.cookies.clone();
    let (status, payload) = post_json_with_jar(
        &client,
        &format!("{IAM_BASE}/iam/authn/v1/auth/nova/smsLogin"),
        &body,
        &mut jar,
    )
    .await?;
    if status >= 400 || payload.is_null() {
        let message = payload
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| payload.get("error").and_then(Value::as_str))
            .map(str::to_string)
            .unwrap_or_else(|| format!("HTTP {status}"));
        return Err(format!("商汤短信登录失败：{}", shorten(&message)));
    }

    let redirect = redirect_of(&payload);
    if redirect.is_empty() {
        if let Some(tenants) = payload.get("tenant_list").and_then(Value::as_array) {
            if tenants.len() > 1 {
                if let Ok(mut table) = pending_table().lock() {
                    if let Some(item) = table.get_mut(ticket) {
                        item.cookies = jar;
                        item.tenants = tenants.clone();
                    }
                }
                return Ok(SmsLoginOutcome::TenantRequired(tenants.clone()));
            }
            // 只有一条租户时上游会**同时**给出 `redirect` 与 `tenant_list`：
            // 直接用这个 redirect 走完换票即可。这一步不能多调一次
            // `loginNext` —— 官网前端在「只有一条租户」时也是直接取响应里的跳转
            // 地址，`loginNext` 只在用户**手动选了租户**之后才调。
            //
            // 万一上游只给了租户、没给 redirect（形态变了），仍按租户走一次
            // `loginNext`，而不是直接报「没有返回可用租户」。
            if let Some(selected) = tenants.first() {
                return complete_tenant_login(&client, &pending, selected, &mut jar).await;
            }
            return Err("商汤短信登录成功，但账号没有返回可用租户".to_string());
        }
        return Err("商汤短信登录没有返回跳转地址".to_string());
    }

    finish_authorization(&client, &pending, &redirect, &mut jar).await
}

/// 选完租户后调上游 `loginNext`，并继续授权码换票。
async fn complete_tenant_login(
    client: &reqwest::Client,
    pending: &PendingSmsLogin,
    tenant: &Value,
    jar: &mut CookieJar,
) -> Result<SmsLoginOutcome, String> {
    let body = json!({
        "challenge": pending.challenge,
        "username": tenant.get("username").and_then(Value::as_str).unwrap_or(""),
        "user_id": tenant.get("user_id").and_then(Value::as_str).unwrap_or(""),
        "sign": tenant.get("sign").and_then(Value::as_str).unwrap_or(""),
    });
    let (status, payload) = post_json_with_jar(
        client,
        &format!("{IAM_BASE}/iam/authn/v1/auth/nova/loginNext"),
        &body,
        jar,
    )
    .await?;
    if status >= 400 {
        return Err(format!("商汤租户登录失败：HTTP {status}"));
    }
    let redirect = redirect_of(&payload);
    if redirect.is_empty() {
        return Err("商汤租户登录没有返回跳转地址".to_string());
    }
    finish_authorization(client, pending, &redirect, jar).await
}

/// 跟随登录成功后的 redirect，取 code 并换 access / refresh token。
async fn finish_authorization(
    client: &reqwest::Client,
    pending: &PendingSmsLogin,
    redirect: &str,
    jar: &mut CookieJar,
) -> Result<SmsLoginOutcome, String> {
    let start = Url::parse(PLATFORM_BASE)
        .ok()
        .and_then(|base| base.join(redirect).ok())
        .map(|url| url.to_string())
        .unwrap_or_else(|| redirect.to_string());
    let followed = follow(client, &start, jar).await?;
    let code = query_param(&followed.url, "code")
        .ok_or_else(|| "商汤登录没有拿到一次性授权码".to_string())?;
    let result = exchange_code(client, &code, &pending.verifier, &pending.state).await?;
    let mut table = pending_table()
        .lock()
        .map_err(|_| "商汤短信登录暂存区不可用".to_string())?;
    table.retain(|_, item| item.expires_at_ms > logging::now_ms());
    table.retain(|_, item| item.token_code != pending.token_code);
    Ok(SmsLoginOutcome::Success(result))
}

/// 用 refresh token 续期（轮换后的 refresh token 由调用方回写）。
pub(crate) async fn refresh_login(
    refresh_token: &str,
    proxy: Option<&ResolvedProxy>,
) -> Result<LoginResult, String> {
    let refresh_token = refresh_token.trim();
    if refresh_token.is_empty() {
        return Err("商汤 refresh token 为空".to_string());
    }
    let client = egress::client_without_redirects(proxy);
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "refresh_token")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("refresh_token", refresh_token)
        .finish();
    let response = client
        .post(format!("{PLATFORM_BASE}/oauth2/token"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("User-Agent", USER_AGENT)
        .timeout(Duration::from_millis(STEP_TIMEOUT_MS))
        .body(form)
        .with_account_user_agent()
        .send()
        .await
        .map_err(|error| format!("商汤续期请求失败：{}", egress::describe_error_detail(&error)))?;
    let status = response.status().as_u16();
    let payload = response_json(response).await;
    let access_token = payload
        .as_ref()
        .and_then(|value| value.get("access_token").and_then(Value::as_str))
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if status >= 400 || access_token.is_empty() {
        let message = payload
            .as_ref()
            .and_then(|value| {
                value
                    .get("error_description")
                    .and_then(Value::as_str)
                    .or_else(|| value.get("error").and_then(Value::as_str))
            })
            .map(str::to_string)
            .unwrap_or_else(|| format!("HTTP {status}"));
        return Err(format!("商汤登录票续期失败：{}", shorten(&message)));
    }
    let next_refresh = payload
        .as_ref()
        .and_then(|value| value.get("refresh_token").and_then(Value::as_str))
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    Ok(LoginResult {
        expires_at_ms: jwt_expires_at_ms(&access_token),
        access_token,
        refresh_token: if next_refresh.is_empty() {
            refresh_token.to_string()
        } else {
            next_refresh
        },
    })
}

/// 用授权码换 access / refresh token。
async fn exchange_code(
    client: &reqwest::Client,
    code: &str,
    verifier: &str,
    state: &str,
) -> Result<LoginResult, String> {
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("code", code)
        .append_pair("redirect_uri", PLATFORM_BASE)
        .append_pair("code_verifier", verifier)
        .append_pair("state", state)
        .finish();
    let response = client
        .post(format!("{PLATFORM_BASE}/oauth2/token"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("User-Agent", USER_AGENT)
        .timeout(Duration::from_millis(STEP_TIMEOUT_MS))
        .body(form)
        .with_account_user_agent()
        .send()
        .await
        .map_err(|error| format!("商汤换码请求失败：{}", egress::describe_error_detail(&error)))?;
    let status = response.status().as_u16();
    let payload = response_json(response).await;
    let access_token = payload
        .as_ref()
        .and_then(|value| value.get("access_token").and_then(Value::as_str))
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if status >= 400 || access_token.is_empty() {
        let message = payload
            .as_ref()
            .and_then(|value| {
                value
                    .get("error_description")
                    .and_then(Value::as_str)
                    .or_else(|| value.get("error").and_then(Value::as_str))
            })
            .map(str::to_string)
            .unwrap_or_else(|| format!("HTTP {status}"));
        return Err(format!("商汤换取登录票失败：{}", shorten(&message)));
    }
    let refresh_token = payload
        .as_ref()
        .and_then(|value| value.get("refresh_token").and_then(Value::as_str))
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    Ok(LoginResult {
        expires_at_ms: jwt_expires_at_ms(&access_token),
        access_token,
        refresh_token,
    })
}

/// 授权地址（参数照源实现：response_type / client_id / PKCE / redirect_uri /
/// scope / state / lang）。
fn authorize_url(challenge: &str, state: &str) -> Result<String, String> {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("code_challenge_method", "S256")
        .append_pair("code_challenge", challenge)
        .append_pair("redirect_uri", PLATFORM_BASE)
        .append_pair("scope", SCOPE)
        .append_pair("state", state)
        .append_pair("lang", "zh-CN")
        .finish();
    Ok(format!("{PLATFORM_BASE}/oauth2/auth?{query}"))
}

/// 跟着 3xx 走到底（每步都带 Cookie、每步都收 Set-Cookie）。
///
/// 返回最终那一步的状态码与地址；**不做**「非 2xx 即错」的判定 ——
/// 调用方按自己的语义读状态（授权页停在 4xx 与正常停页要分开报）。
async fn follow(
    client: &reqwest::Client,
    start: &str,
    jar: &mut CookieJar,
) -> Result<Followed, String> {
    let mut current = start.to_string();
    for _ in 0..=MAX_REDIRECTS {
        let mut request = client
            .get(&current)
            .header("User-Agent", USER_AGENT)
            .timeout(std::time::Duration::from_millis(STEP_TIMEOUT_MS));
        let cookie = jar.header();
        if !cookie.is_empty() {
            request = request.header("Cookie", cookie);
        }
        let response = request
            .with_account_user_agent()
            .send()
            .await
            .map_err(|error| format!("商汤登录跳转请求失败：{}", egress::describe_error_detail(&error)))?;
        jar.capture(&response);
        let status = response.status().as_u16();
        let location = response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        match location {
            Some(location) if (300..400).contains(&status) => {
                current = resolve_url(&current, &location)?;
            }
            _ => {
                let url = Url::parse(&current).ok();
                return Ok(Followed { status, url });
            }
        }
    }
    Err("商汤登录跳转次数过多（已超过上限）".to_string())
}

/// 相对 / 绝对 location → 绝对地址（畸形值报错，不静默截断）
fn resolve_url(current: &str, location: &str) -> Result<String, String> {
    let base = Url::parse(current).map_err(|_| "商汤登录跳转地址无法解析".to_string())?;
    if let Ok(absolute) = Url::parse(location) {
        return Ok(absolute.to_string());
    }
    base.join(location)
        .map(|url| url.to_string())
        .map_err(|_| "商汤登录跳转地址不是合法 URL".to_string())
}

/// 调 IAM 的 JSON 接口（发码 / 短信登录 / 租户选择共用一套头），
/// 并读取 `Set-Cookie` 到给定 Cookie Jar。
async fn get_json_with_jar(
    client: &reqwest::Client,
    url: &str,
    jar: &mut CookieJar,
) -> Result<(u16, Value), String> {
    let mut request = client
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Origin", PLATFORM_BASE)
        .header("Referer", format!("{PLATFORM_BASE}/login"))
        .timeout(Duration::from_millis(STEP_TIMEOUT_MS));
    let cookie = jar.header();
    if !cookie.is_empty() {
        request = request.header("Cookie", cookie);
    }
    let response = request
        .with_account_user_agent()
        .send()
        .await
        .map_err(|error| format!("商汤登录接口请求失败：{}", egress::describe_error_detail(&error)))?;
    jar.capture(&response);
    let status = response.status().as_u16();
    let payload = response_json(response).await.unwrap_or(Value::Null);
    Ok((status, payload))
}

/// 同上，并读取 `Set-Cookie` 到给定 Cookie Jar。
async fn post_json_with_jar(
    client: &reqwest::Client,
    url: &str,
    body: &Value,
    jar: &mut CookieJar,
) -> Result<(u16, Value), String> {
    let response = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("User-Agent", USER_AGENT)
        .header("Origin", PLATFORM_BASE)
        .header("Referer", format!("{PLATFORM_BASE}/login"))
        .timeout(Duration::from_millis(STEP_TIMEOUT_MS))
        .json(body)
        .with_account_user_agent()
        .send()
        .await
        .map_err(|error| format!("商汤登录接口请求失败：{}", egress::describe_error_detail(&error)))?;
    jar.capture(&response);
    let status = response.status().as_u16();
    let payload = response_json(response).await.unwrap_or(Value::Null);
    Ok((status, payload))
}

/// 读一个响应体里的 JSON（读不出 / 不是 JSON 给 None；状态码由调用方先读走）
async fn response_json(response: reqwest::Response) -> Option<Value> {
    let text = response.text().await.ok()?;
    if text.trim().is_empty() {
        return None;
    }
    serde_json::from_str::<Value>(&text).ok()
}

/// URL 里的一个 query 参数（取不出给 None）。值做一次 `+` → 空格与
/// percent-decode —— `code` / `login_challenge` 里出现转义字符时按原文读。
fn query_param(url: &Option<Url>, name: &str) -> Option<String> {
    let url = url.as_ref()?;
    let value = url
        .query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.to_string())?;
    let text = value.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// 从登录响应里取跳转地址（兼容三种上游字段名）。
fn redirect_of(payload: &Value) -> String {
    ["redirect_uri", "redirect_to", "redirect"]
        .into_iter()
        // 空串要**跳过**而不是当选中：上游在 `redirect_uri` / `redirect_to` 上都
        // 放空串，真正有值的是 `redirect`。用 `find_map` 会命中第一个空串就
        // 返回，轮不到 `redirect` —— 于是单租户登录走到「没有返回跳转地址」，
        // 而真正需要 `loginNext` 的多租户分支又拿着错误的 challenge 去调，
        // 上游回 `tryToReLogin`（HTTP 400）。官网前端用的是
        // `t && t || s && s || r && r`，空串同样被跳过。
        .filter_map(|key| payload.get(key).and_then(Value::as_str))
        .map(str::trim)
        .find(|value| !value.is_empty())
        .unwrap_or("")
        .to_string()
}

/// 短信登录中间态表（首次访问时初始化）。
fn pending_table() -> &'static Mutex<HashMap<String, PendingSmsLogin>> {
    PENDING_SMS_LOGINS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 删掉已过期的短信登录中间态。
fn prune_pending(table: &mut HashMap<String, PendingSmsLogin>) {
    let now = logging::now_ms();
    table.retain(|_, item| item.expires_at_ms > now);
}

/// 大陆手机号本地校验（与官网登录页 `^1\d{10}$` 同口径）。
fn valid_phone(phone: &str) -> bool {
    let bytes = phone.as_bytes();
    bytes.len() == 11
        && bytes.first() == Some(&b'1')
        && bytes.iter().all(|byte| byte.is_ascii_digit())
}

/// 洗掉 base64 字段里的控制字符。
///
/// 上游的人机验证图片字段会在 base64 末尾堆一长串填充（2026-11-12 实测
/// `image` 尾部有 128453 个 NUL，以 `\u0000` 转义出现在 JSON 里）。那不是
/// base64 的一部分，原样透传给前端会让 `data:image/png;base64,` 渲染失败。
fn clean_base64(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

/// 手机号脱敏：保留前三后四（与官网 `el` 函数同口径）。
fn mask_phone(phone: &str) -> String {
    let digits: String = phone.chars().filter(|ch| ch.is_ascii_digit()).collect();
    if digits.len() >= 11 {
        let start = &digits[..3];
        let end = &digits[digits.len() - 4..];
        return format!("{start}****{end}");
    }
    if digits.len() >= 7 {
        let start = &digits[..2];
        let end = &digits[digits.len() - 2..];
        return format!("{start}****{end}");
    }
    "****".to_string()
}

/// 极简 Cookie Jar：只认 `name=value`，同名覆盖（见模块头）。
#[derive(Clone, Default)]
struct CookieJar {
    cookies: Vec<(String, String)>,
}

impl CookieJar {
    fn capture(&mut self, response: &reqwest::Response) {
        for header in response.headers().get_all("set-cookie") {
            let Ok(text) = header.to_str() else { continue };
            let pair = text.split(';').next().unwrap_or("");
            let Some((name, value)) = pair.split_once('=') else { continue };
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            let value = value.trim().to_string();
            match self.cookies.iter_mut().find(|(known, _)| known == name) {
                Some(slot) => slot.1 = value,
                None => self.cookies.push((name.to_string(), value)),
            }
        }
    }

    fn header(&self) -> String {
        self.cookies
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// 最终停在哪（状态码 + 地址；地址解析失败时是 None，供 query 参数读取）
struct Followed {
    status: u16,
    url: Option<Url>,
}

/// PKCE code_verifier：32 字节随机 → base64url（43 字符，落在 43~128 之间）
fn random_secret() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes)
        .map_err(|_| "无法生成安全的随机串（系统随机源不可用）".to_string())?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// state：16 字节随机 → hex（源实现 `randomBytes(16).toString('hex')`）
fn random_hex() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes)
        .map_err(|_| "无法生成安全的随机串（系统随机源不可用）".to_string())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// 文案截断（上游的错误说明可能是一整页 HTML）
fn shorten(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= 160 {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(157).collect();
    format!("{head}...")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 上游在 `redirect_uri` / `redirect_to` 上放空串、真正有值的是 `redirect`
    /// （2026-11-12 实测的单租户短信登录响应）。取跳转地址时必须**跳过空串**，
    /// 否则单租户登录拿不到 redirect，登录链路走不通。
    #[test]
    fn redirect_of_skips_empty_fields() {
        let payload = json!({
            "redirect_uri": "",
            "redirect_to": "",
            "redirect": "https://platform.sensenova.cn/oauth2/auth?code_challenge=x",
        });
        assert_eq!(
            "https://platform.sensenova.cn/oauth2/auth?code_challenge=x",
            redirect_of(&payload)
        );
    }

    /// 三个字段都为真空时给出空串（调用方据此报「没有返回跳转地址」）。
    #[test]
    fn redirect_of_returns_empty_when_all_blank() {
        let payload = json!({ "redirect_uri": "  ", "redirect_to": "", "redirect": null });
        assert_eq!("", redirect_of(&payload));
    }

    /// `redirect_uri` 有值时优先用它（上游多租户分支走这个形态）。
    #[test]
    fn redirect_of_prefers_redirect_uri() {
        let payload = json!({ "redirect_uri": "https://a", "redirect": "https://b" });
        assert_eq!("https://a", redirect_of(&payload));
    }
}
