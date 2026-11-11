//! 通过宿主私有状态保存每个账号的规则，精确版本用于防止覆盖并发编辑

use cpr_account_headers::Rule;
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::host::{
        STATE_GET, STATE_PUT, StateGetRequest, StateGetResult, StatePutRequest, StatePutResult,
    },
    client::{HostClient, SessionError},
};
use serde::{Deserialize, Serialize};
use serde_json::{from_value, to_value};

/// 页面读取或保存的完整快照，版本为宿主原始十进制字符串
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Snapshot {
    pub(crate) account_id: String,
    pub(crate) version: Option<String>,
    #[serde(flatten)]
    pub(crate) rule: Rule,
}

/// 读取真实持久状态，尚无配置时返回停用的空规则
/// @param host 当前父调用的宿主客户端
/// @param account 宿主提供的账号 ID，作为不透明字符串使用
/// @return 规则与精确版本
pub(crate) async fn load(host: &HostClient, account: String) -> Result<Snapshot, PluginFault> {
    let reply = host
        .call(
            STATE_GET,
            to_value(StateGetRequest {
                namespace: "headers".into(),
                key: account.clone(),
            })
            .map_err(encoding)?,
            Vec::new(),
        )
        .await
        .map_err(SessionError::into_plugin_fault)?;
    let result: StateGetResult = from_value(reply.result).map_err(encoding)?;
    let (rule, version) = match result.record {
        Some(record) => {
            let rule: Rule = from_value(record.value).map_err(encoding)?;
            (rule, Some(record.version.to_string()))
        }
        None => (Rule::default(), None),
    };
    Ok(Snapshot {
        account_id: account,
        version,
        rule,
    })
}

/// 按页面读取的版本保存，写入结果未知时由页面重新读取确认
/// @param host 当前管理员调用的宿主客户端
/// @param input 页面提交的完整快照
/// @return 已确认持久化的规范规则及新版本
pub(crate) async fn save(host: &HostClient, input: Snapshot) -> Result<Snapshot, PluginFault> {
    let account = input.account_id;
    let rule = input
        .rule
        .normalized()
        .map_err(|message| PluginFault::new(ErrorCode::InvalidInput, message))?;
    let version = input
        .version
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| PluginFault::new(ErrorCode::InvalidInput, "规则版本无效"))
        })
        .transpose()?;
    let reply = host
        .call(
            STATE_PUT,
            to_value(StatePutRequest {
                namespace: "headers".into(),
                key: account.clone(),
                value: to_value(&rule).map_err(encoding)?,
                expected_version: version,
            })
            .map_err(encoding)?,
            Vec::new(),
        )
        .await
        .map_err(SessionError::into_plugin_fault)?;
    let result: StatePutResult = from_value(reply.result).map_err(encoding)?;
    Ok(Snapshot {
        account_id: account,
        rule,
        version: Some(result.version.to_string()),
    })
}

fn encoding(_: serde_json::Error) -> PluginFault {
    invalid_state()
}

fn invalid_state() -> PluginFault {
    PluginFault::new(ErrorCode::Fault, "账号请求头规则格式无效")
}
