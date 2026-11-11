//! 为隔离扩展页提供账号分页和按账号保存的请求头规则

use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use form_urlencoded::parse;
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        data::{AccountFacts, AccountFactsQuery},
        management::{
            ManagementPage, ManagementRegistration, ManagementRequest, ManagementResource,
            ManagementResponse, ManagementRoute,
        },
        middleware::MiddlewareHeader,
    },
    client::{HostClient, TypedCall, TypedReply},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, from_slice, json, to_value, to_vec};

use crate::{
    host_json,
    store::{self, Snapshot},
    testing::Tests,
};

/// 列表只公开规则状态与数量，头值由编辑接口按账号读取
#[derive(Serialize)]
struct AccountEntry {
    #[serde(flatten)]
    account: AccountFacts,
    group_names: Vec<String>,
    override_enabled: bool,
    header_count: usize,
}

/// 注册页面及其最小管理接口，资源由宿主按包摘要交付
/// @return 与作者清单一致的管理注册
pub(crate) fn registration() -> ManagementRegistration {
    ManagementRegistration {
        routes: [
            ("GET", "accounts"),
            ("GET", "rule"),
            ("POST", "rule"),
            ("POST", "test"),
            ("POST", "test/cancel"),
            ("POST", "test/finish"),
            ("GET", "test"),
        ]
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
            description: Some("查看各账号的请求头覆写状态，编辑上游请求头".into()),
            entry: "web/index.html".into(),
            icon: None,
        }],
        callbacks: Vec::new(),
    }
}

/// 处理宿主已认证的管理请求，错误正文保持中文且不回显头值
/// @param call 当前管理员调用及原始正文
/// @param tests 当前进程共享的测试取消登记
/// @return JSON 业务正文或明确的错误状态
pub(crate) async fn handle(
    call: TypedCall<ManagementRequest>,
    tests: Tests,
) -> Result<TypedReply<ManagementResponse>, PluginFault> {
    let (status, value) = match route(&call, &tests).await {
        Ok(value) => (200, value),
        Err(error) => {
            let status = match error.code {
                ErrorCode::InvalidInput => 400,
                ErrorCode::Conflict => 409,
                ErrorCode::Rejected => 404,
                ErrorCode::Cancelled => 409,
                ErrorCode::Timeout => 408,
                _ => 503,
            };
            let message = match error.code {
                ErrorCode::Conflict | ErrorCode::Cancelled | ErrorCode::Timeout
                    if call.request.path.starts_with("test") =>
                {
                    &error.message
                }
                ErrorCode::Conflict => "规则已被其他页面更新，请重新读取后编辑",
                ErrorCode::InvalidInput => &error.message,
                _ if call.request.path.starts_with("test") => "测试资源准备失败",
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

async fn route(call: &TypedCall<ManagementRequest>, tests: &Tests) -> Result<Value, PluginFault> {
    let mut query = BTreeMap::new();
    for (key, value) in parse(call.request.query.as_bytes()).into_owned() {
        if query.insert(key, value).is_some() {
            return Err(invalid());
        }
    }
    match (call.request.method.as_str(), call.request.path.as_str()) {
        ("GET", "accounts") if query.keys().all(|key| key == "cursor" || key == "search") => {
            accounts(
                call,
                query.remove("cursor"),
                query.remove("search").unwrap_or_default(),
            )
            .await
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
        ("POST", "test") if query.is_empty() => tests.prepare(call).await,
        ("POST", "test/cancel") if query.is_empty() => tests.end(call, true),
        ("POST", "test/finish") if query.is_empty() => tests.end(call, false),
        ("GET", "test") if query.len() == 2 => tests.result(
            &query.remove("id").ok_or_else(invalid)?,
            &query.remove("requestId").ok_or_else(invalid)?,
        ),
        _ => Err(invalid()),
    }
}

async fn accounts(
    call: &TypedCall<ManagementRequest>,
    mut cursor: Option<String>,
    search: String,
) -> Result<Value, PluginFault> {
    if search.len() > 256 {
        return Err(invalid());
    }
    let search = search.trim().to_lowercase();
    let mut entries: Vec<AccountEntry> = Vec::new();
    loop {
        let page = call
            .host
            .account_facts(AccountFactsQuery {
                provider_id: None,
                cursor,
                limit: 200,
            })
            .await?;
        for account in page.accounts {
            if !search.is_empty()
                && ![
                    account.name.as_str(),
                    account.email.as_deref().unwrap_or(""),
                    &account.account_id,
                ]
                .iter()
                .any(|text| text.to_lowercase().contains(&search))
            {
                continue;
            }
            // 多读一个匹配账号确认下一页，只为当前展示的账号读取规则
            if entries.len() == 20 {
                add_group_names(&call.host, &mut entries).await;
                let next_cursor = &entries[19].account.account_id;
                return Ok(json!({"accounts":entries,"next_cursor":next_cursor}));
            }
            let snapshot = store::load(&call.host, account.account_id.clone()).await?;
            entries.push(AccountEntry {
                group_names: Vec::new(),
                account,
                override_enabled: snapshot.rule.enabled,
                header_count: snapshot.rule.headers.len(),
            });
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            add_group_names(&call.host, &mut entries).await;
            return Ok(json!({"accounts":entries,"next_cursor":null}));
        }
    }
}

fn invalid() -> PluginFault {
    PluginFault::new(ErrorCode::InvalidInput, "请求参数无效")
}

/// 账号列表展示当前所属分组，名称缺失时保留稳定分组 ID
/// @param host 当前管理调用的受管客户端
/// @param entries 当前页账号及其宿主分组 ID
/// @return 原地补齐各账号的当前分组名称
async fn add_group_names(host: &HostClient, entries: &mut [AccountEntry]) {
    #[derive(Deserialize)]
    struct Group {
        id: String,
        name: String,
    }
    /// 分组选择项使用管理 API 的分页信封
    #[derive(Deserialize)]
    struct Page {
        items: Vec<Group>,
        page: Pagination,
    }
    #[derive(Deserialize)]
    struct Pagination {
        total: u64,
    }
    let mut needed: BTreeSet<_> = entries
        .iter()
        .flat_map(|e| e.account.group_ids.iter().cloned())
        .collect();
    let mut names = BTreeMap::new();
    let lookup = async {
        let mut page = 1_u64;
        while !needed.is_empty() {
            let Some(value) = host_json::get(
                host,
                format!("/api/admin/account-groups/options?page={page}&pageSize=100"),
            )
            .await
            else {
                break;
            };
            let Ok(groups) = serde_json::from_value::<Page>(value) else {
                break;
            };
            let empty = groups.items.is_empty();
            for group in groups.items {
                if needed.remove(&group.id) && !group.name.trim().is_empty() {
                    names.insert(group.id, group.name);
                }
            }
            if empty || page.saturating_mul(100) >= groups.page.total {
                break;
            }
            page += 1;
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(2), lookup).await;
    for entry in entries {
        entry.group_names = entry
            .account
            .group_ids
            .iter()
            .map(|id| names.get(id).unwrap_or(id).clone())
            .collect();
    }
}
