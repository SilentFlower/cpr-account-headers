//! 为隔离扩展页提供账号分页和按账号保存的请求头规则

use std::collections::BTreeMap;

use form_urlencoded::parse;
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        data::AccountFactsQuery,
        management::{
            ManagementPage, ManagementRegistration, ManagementRequest, ManagementResource,
            ManagementResponse, ManagementRoute,
        },
        middleware::MiddlewareHeader,
    },
    client::{TypedCall, TypedReply},
};
use serde_json::{Value, from_slice, json, to_value, to_vec};

use crate::store::{self, Snapshot};

/// 注册页面及其最小管理接口，资源由宿主按包摘要交付
/// @return 与作者清单一致的管理注册
pub(crate) fn registration() -> ManagementRegistration {
    ManagementRegistration {
        routes: [("GET", "accounts"), ("GET", "rule"), ("POST", "rule")]
            .into_iter()
            .map(|(method, path)| ManagementRoute {
                method: method.into(),
                path: path.into(),
                request_content_types: if method == "POST" {
                    vec!["application/json".into()]
                } else {
                    Vec::new()
                },
                response_content_types: vec!["application/json".into()],
            })
            .collect(),
        resources: ["web/index.html", "web/app.js", "web/app.css"]
            .into_iter()
            .map(|path| ManagementResource {
                path: path.into(),
                public: false,
            })
            .collect(),
        pages: vec![ManagementPage {
            id: "headers".into(),
            title: "账号请求头".into(),
            description: Some("选择账号，覆写发往模型上游的请求头".into()),
            entry: "web/index.html".into(),
            icon: None,
        }],
        callbacks: Vec::new(),
    }
}

/// 处理宿主已认证的管理请求，错误正文保持中文且不回显头值
/// @param call 当前管理员调用及原始正文
/// @return JSON 业务正文或明确的错误状态
pub(crate) async fn handle(
    call: TypedCall<ManagementRequest>,
) -> Result<TypedReply<ManagementResponse>, PluginFault> {
    let (status, value) = match route(&call).await {
        Ok(value) => (200, value),
        Err(error) => {
            let status = match error.code {
                ErrorCode::InvalidInput => 400,
                ErrorCode::Conflict => 409,
                ErrorCode::Rejected => 404,
                _ => 503,
            };
            let message = match error.code {
                ErrorCode::Conflict => "规则已被其他页面更新，请重新读取后编辑",
                ErrorCode::InvalidInput => &error.message,
                _ => "账号请求头操作失败，请重新读取确认当前规则",
            };
            (
                status,
                json!({"error":{"code":error.code,"message":message}}),
            )
        }
    };
    Ok(TypedReply::new(ManagementResponse {
        status,
        content_type: "application/json".into(),
        headers: vec![MiddlewareHeader {
            name: "cache-control".into(),
            value: b"no-store".to_vec(),
        }],
    })
    .with_payload(
        to_vec(&value).map_err(|_| PluginFault::new(ErrorCode::Fault, "管理响应编码失败"))?,
    ))
}

async fn route(call: &TypedCall<ManagementRequest>) -> Result<Value, PluginFault> {
    let mut query = BTreeMap::new();
    for (key, value) in parse(call.request.query.as_bytes()).into_owned() {
        if query.insert(key, value).is_some() {
            return Err(invalid());
        }
    }
    match (call.request.method.as_str(), call.request.path.as_str()) {
        ("GET", "accounts") if query.keys().all(|key| key == "cursor") => {
            let page = call
                .host
                .account_facts(AccountFactsQuery {
                    provider_id: None,
                    cursor: query.remove("cursor"),
                    limit: 200,
                })
                .await?;
            to_value(page).map_err(|_| invalid())
        }
        ("GET", "rule")
            if query.len() == 1 && query.get("accountId").is_some_and(|id| !id.is_empty()) =>
        {
            let account = query.remove("accountId").ok_or_else(invalid)?;
            to_value(store::load(&call.host, account).await?).map_err(|_| invalid())
        }
        ("POST", "rule") if query.is_empty() => {
            let input: Snapshot = from_slice(&call.payload).map_err(|_| invalid())?;
            if input.account_id.is_empty() {
                return Err(invalid());
            }
            to_value(store::save(&call.host, input).await?).map_err(|_| invalid())
        }
        _ => Err(invalid()),
    }
}

fn invalid() -> PluginFault {
    PluginFault::new(ErrorCode::InvalidInput, "请求参数无效")
}
