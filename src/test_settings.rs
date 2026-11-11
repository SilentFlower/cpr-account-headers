//! 保存统一测试分组，并通过原生管理接口复用专用测试 Key

use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        data::{AccessQuery, AccountFacts, ClientKeyFactsQuery},
        host::{
            STATE_GET, STATE_PUT, StateGetRequest, StateGetResult, StatePutRequest, StatePutResult,
        },
        resources::{GroupEnsureRequest, GroupMembersChange},
    },
    client::{HostClient, SessionError},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::host_json;

/// 空分组代表自动维护共享测试组，版本用于保护并发编辑
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Settings {
    pub(crate) group_id: Option<String>,
    pub(crate) version: Option<String>,
}

/// 原生分组选择项
#[derive(Serialize, Deserialize)]
pub(crate) struct Group {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) enabled: bool,
}

/// 读取统一设置及其精确版本
/// @param host 当前管理调用
/// @return 已保存设置，首次读取使用自动创建模式
pub(crate) async fn load(host: &HostClient) -> Result<Settings, PluginFault> {
    let reply = host
        .call(
            STATE_GET,
            serde_json::to_value(StateGetRequest {
                namespace: "testing".into(),
                key: "settings".into(),
            })
            .map_err(|_| unavailable())?,
            Vec::new(),
        )
        .await
        .map_err(SessionError::into_plugin_fault)?;
    let record: StateGetResult = serde_json::from_value(reply.result).map_err(|_| unavailable())?;
    let Some(record) = record.record else {
        return Ok(Settings::default());
    };
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Stored {
        group_id: Option<String>,
    }
    let stored: Stored = serde_json::from_value(record.value).map_err(|_| unavailable())?;
    Ok(Settings {
        group_id: stored.group_id,
        version: Some(record.version.to_string()),
    })
}

/// 保存全账号共用的测试分组，已有组的成员由管理员维护
/// @param host 当前管理调用
/// @param input 页面读取后提交的设置
/// @return 持久化设置与新版本
pub(crate) async fn save(host: &HostClient, input: Settings) -> Result<Settings, PluginFault> {
    if let Some(id) = &input.group_id
        && (id.is_empty()
            || id.len() > 128
            || !groups(host).await?.iter().any(|g| g.id == *id && g.enabled))
    {
        return Err(PluginFault::new(
            ErrorCode::InvalidInput,
            "请选择已启用的测试分组",
        ));
    }
    let version = input
        .version
        .map(|v| {
            v.parse::<u64>()
                .map_err(|_| PluginFault::new(ErrorCode::InvalidInput, "测试设置版本无效"))
        })
        .transpose()?;
    let reply = host
        .call(
            STATE_PUT,
            serde_json::to_value(StatePutRequest {
                namespace: "testing".into(),
                key: "settings".into(),
                value: json!({"groupId":input.group_id}),
                expected_version: version,
            })
            .map_err(|_| unavailable())?,
            Vec::new(),
        )
        .await
        .map_err(SessionError::into_plugin_fault)?;
    let saved: StatePutResult = serde_json::from_value(reply.result).map_err(|_| unavailable())?;
    Ok(Settings {
        group_id: input.group_id,
        version: Some(saved.version.to_string()),
    })
}

/// 按原生分页合同读取全部分组选择项
/// @param host 当前管理调用
/// @return 包含停用状态的分组，失败时中止本次设置或测试
pub(crate) async fn groups(host: &HostClient) -> Result<Vec<Group>, PluginFault> {
    #[derive(Deserialize)]
    struct Page {
        items: Vec<Group>,
        page: Pagination,
    }
    #[derive(Deserialize)]
    struct Pagination {
        total: u64,
    }
    let mut result = Vec::new();
    for page in 1..=100 {
        let value = host_json::get(
            host,
            format!("/api/admin/account-groups/options?page={page}&pageSize=100"),
        )
        .await
        .ok_or_else(unavailable)?;
        let value: Page = serde_json::from_value(value).map_err(|_| unavailable())?;
        let done = value.items.is_empty() || page * 100 >= value.page.total;
        result.extend(value.items);
        if done {
            return Ok(result);
        }
    }
    Err(unavailable())
}

/// 留空时为本实例幂等维护一个覆盖全部账号的共享分组
/// @param host 当前管理调用
/// @param instance 插件实例身份
/// @param accounts 宿主当前完整账号事实
/// @param selected 正在测试的账号
/// @return 统一测试 Key ID
pub(crate) async fn resources(
    host: &HostClient,
    instance: &str,
    accounts: &[AccountFacts],
    selected: &AccountFacts,
) -> Result<String, PluginFault> {
    let settings = load(host).await?;
    let group = if let Some(id) = settings.group_id {
        let group = groups(host)
            .await?
            .into_iter()
            .find(|g| g.id == id && g.enabled)
            .ok_or_else(|| {
                PluginFault::new(ErrorCode::InvalidInput, "测试分组已删除或停用，请重新设置")
            })?;

        group.id
    } else {
        let group = host
            .ensure_group(GroupEnsureRequest {
                resource_key: "test.shared".into(),
                name: format!("请求头测试 {instance}"),
                color: "#4477AAFF".into(),
                description: Some("账号请求头插件的共享测试分组，自动加入全部账号".into()),
            })
            .await?;
        if !group.enabled {
            return Err(PluginFault::new(
                ErrorCode::InvalidInput,
                "共享测试分组已停用",
            ));
        }
        host.change_group_members(GroupMembersChange {
            resource_key: "test.shared".into(),
            add: accounts.iter().map(|a| a.account_id.clone()).collect(),
            remove: Vec::new(),
        })
        .await?;
        group.id
    };
    // 专用 Key 按实例和分组稳定命名，创建响应丢失后仍可通过原生列表找回复用
    // 已有分组不属于插件受管资源，不能传给 ensure_key 的 group_resource_keys
    let name = format!("请求头测试 {instance} {group}");
    let label = format!("cpr-account-headers:{instance}");
    let mut cursor: Option<String> = None;
    let mut found = Vec::new();
    loop {
        let uri = {
            let mut query = form_urlencoded::Serializer::new(String::new());
            query
                .append_pair("search", &name)
                .append_pair("limit", "100");
            if let Some(cursor) = &cursor {
                query.append_pair("cursor", cursor);
            }
            format!("/api/admin/client-keys?{}", query.finish())
        };
        let data = host_json::get(host, uri).await.ok_or_else(unavailable)?;
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Keys {
            items: Vec<Key>,
            next_cursor: Option<String>,
        }
        #[derive(Deserialize)]
        struct Key {
            id: String,
            name: String,
            label: Option<String>,
        }
        let data: Keys = serde_json::from_value(data).map_err(|_| unavailable())?;
        found.extend(
            data.items
                .into_iter()
                .filter(|k| k.name == name && k.label.as_deref() == Some(&label))
                .map(|k| k.id),
        );
        cursor = data.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    let key = match found.as_slice() {
        [id] => id.clone(),
        [] => {
            let created = host_json::send(host, "POST", "/api/admin/client-keys/create".into(), Some(json!({"name":name,"label":label,"groupIds":[group],"maxConcurrency":4,"requestsPerMinute":0,"dailyLimitUsd":"0","weeklyLimitUsd":"0"}))).await.ok_or_else(unavailable)?;
            created
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(unavailable)?
                .to_owned()
        }
        _ => {
            return Err(PluginFault::new(
                ErrorCode::InvalidInput,
                "存在重复的专用测试 Key，请在 Key 管理中处理",
            ));
        }
    };
    let facts = host
        .key_facts(ClientKeyFactsQuery {
            client_key_id: key.clone(),
        })
        .await?;
    if !facts.enabled || facts.group_ids != [group] {
        return Err(PluginFault::new(
            ErrorCode::InvalidInput,
            "专用测试 Key 已停用或分组范围已改变",
        ));
    }
    // 权限使用宿主专用接口判定，账号列表的分组展示投影不承担执行授权
    let access = host
        .check_access(AccessQuery {
            client_key_id: key.clone(),
            account_ids: vec![selected.account_id.clone()],
            upstream_model: None,
        })
        .await?;
    if !access
        .accounts
        .iter()
        .any(|a| a.account_id == selected.account_id && a.allowed)
    {
        return Err(PluginFault::new(
            ErrorCode::InvalidInput,
            "所选账号不属于统一测试分组，请调整分组权限",
        ));
    }
    Ok(key)
}

fn unavailable() -> PluginFault {
    PluginFault::new(ErrorCode::Fault, "测试设置读取或资源准备失败")
}
