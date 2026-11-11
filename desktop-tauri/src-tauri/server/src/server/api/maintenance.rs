use axum::extract::{Query, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::server::core::{maintenance, scheduled_tasks};
use crate::server::errors::not_found_response;
use crate::server::http::{ok_json, parse_body, MAX_BODY_SIZE};
use crate::server::ServerState;

pub async fn overview(
    State(state): State<ServerState>,
    Query(query): Query<maintenance::types::Query>,
) -> Response {
    let result = (|| -> Result<Value, String> {
        let records = maintenance::storage::list(&query)?;
        let latest = maintenance::storage::latest()?;
        let retention = crate::server::config::retention_settings();
        // 明细弹窗的提供商筛选要和模型目录刷新的匹配口径一致：只列有启用账号的
        // 提供商，避免出现启用但旗下没有账号的家
        let store = state.store();
        let in_use: Vec<String> = crate::server::core::providers::executable_provider_ids()
            .into_iter()
            .filter(|provider| !store.accounts_for_provider(provider).is_empty())
            .collect();
        let providers: Vec<Value> = crate::server::core::providers::summary_json(
            |provider| if in_use.iter().any(|id| id == provider) { store.accounts_for_provider(provider).len() } else { 0 },
            &in_use,
        )
        .into_iter()
        .filter(|item| in_use.iter().any(|id| item["id"].as_str() == Some(id)))
        .collect();
        Ok(json!({
            "tasks": [scheduled_tasks::task_by_id(maintenance::CREDENTIAL), scheduled_tasks::task_by_id(maintenance::MODEL)],
            "latest": latest,
            "records": records,
            "providers": providers,
            "retentionDays": retention.maintenance_days,
            "maxRecords": retention.maintenance_count,
        }))
    })();
    match result {
        Ok(value) => ok_json(value),
        Err(error) => error_response(500, error),
    }
}

pub async fn entry(State(state): State<ServerState>, request: axum::extract::Request) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let rest = path.strip_prefix("/api/maintenance/").unwrap_or_default();
    let segments = rest.split('/').collect::<Vec<_>>();
    match (method.clone(), segments.as_slice()) {
        (Method::POST, ["records", "clear"]) => match maintenance::storage::clear() {
            Ok(cleared) => ok_json(json!({ "cleared": cleared })),
            Err(error) => error_response(500, error),
        },
        (Method::GET, ["records", id]) => match maintenance::storage::detail(id) {
            Ok(detail) => ok_json(json!(detail)),
            Err(error) => error_response(
                if error == "维护记录不存在或已清理" {
                    404
                } else {
                    500
                },
                error,
            ),
        },
        (Method::POST, ["run", task_id]) => {
            if !matches!(*task_id, maintenance::CREDENTIAL | maintenance::MODEL) {
                return error_response(400, "未知的维护任务");
            }
            execution_response(maintenance::launch(
                state.store().clone(),
                maintenance::Request::full(task_id, true),
            ))
        }
        (Method::POST, ["records", id, "retry"]) => {
            let body = match axum::body::to_bytes(request.into_body(), MAX_BODY_SIZE).await {
                Ok(body) => body,
                Err(error) => return error_response(400, format!("请求体读取失败：{error}")),
            };
            let payload = match parse_body(&body) {
                Ok(payload) => payload,
                Err(error) => return error_response(400, error.message),
            };
            let item_id = payload
                .get("itemId")
                .and_then(Value::as_str)
                .map(str::to_string);
            execution_response(maintenance::retry(
                state.store().clone(),
                id,
                item_id.as_deref(),
            ))
        }
        _ => not_found_response(method.as_str(), &path),
    }
}

fn execution_response(result: Result<maintenance::types::Detail, String>) -> Response {
    match result {
        Ok(detail) => ok_json(json!(detail)),
        Err(error) => error_response(400, error),
    }
}

pub fn error_response(status: u16, message: impl Into<String>) -> Response {
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(
            json!({ "success": false, "error": maintenance::types::safe_message(&message.into()) }),
        ),
    )
        .into_response()
}
