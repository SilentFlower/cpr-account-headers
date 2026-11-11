//! 为普通模型链路准备单账号测试身份，并按真实模型请求 ID 收集本插件的覆写记录

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use cpr_account_headers::{HeaderOverride, Rule};
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        data::{AccountFactsQuery, ClientKeyFactsQuery},
        management::ManagementRequest,
        middleware::{
            MiddlewareHeader,
            outbound::{Metadata, Target},
        },
        resources::{GroupEnsureRequest, GroupMembersChange, KeyEnsureRequest},
    },
    client::TypedCall,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const LIFETIME: Duration = Duration::from_secs(90);

/// 仅保存短期测试关联，分组和 Key 由宿主按插件实例持有
#[derive(Default, Clone)]
pub(crate) struct Tests(Arc<Mutex<HashMap<String, Entry>>>);

struct Entry {
    created: Instant,
    account: String,
    key: Option<String>,
    cancelled: bool,
    records: Vec<Evidence>,
}

/// 头值只来自当前测试边界，原始认证材料不进入诊断
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Evidence {
    request_id: String,
    account_id: String,
    attempt_index: u32,
    transport: String,
    override_enabled: bool,
    before_headers: Vec<HeaderOverride>,
    headers: Vec<HeaderOverride>,
}

/// 准备失败或调用被取消时释放测试名额
struct Reservation {
    tests: Tests,
    id: String,
    ready: bool,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.ready {
            self.tests
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&self.id);
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    id: String,
    account_id: String,
    model_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct End {
    id: String,
}

impl Tests {
    /// 为账号幂等准备专用分组与 Key，页面随后使用普通模型桥发起请求
    /// @param call 当前管理员调用
    /// @return 不含密钥明文的执行身份与测试关联
    pub(crate) async fn prepare(
        &self,
        call: &TypedCall<ManagementRequest>,
    ) -> Result<Value, PluginFault> {
        let input: Input = serde_json::from_slice(&call.payload).map_err(|_| invalid())?;
        if !valid_id(&input.id)
            || input.account_id.is_empty()
            || input.account_id.len() > 128
            || input.model_id.trim().is_empty()
            || input.model_id.len() > 256
            || input.model_id.chars().any(char::is_control)
        {
            return Err(invalid());
        }
        {
            let mut entries = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            entries.retain(|_, entry| entry.created.elapsed() < LIFETIME);
            if entries.get(&input.id).is_some_and(|entry| entry.cancelled) {
                return Err(PluginFault::new(ErrorCode::Cancelled, "测试已停止"));
            }
            if entries.contains_key(&input.id)
                || entries
                    .values()
                    .any(|entry| !entry.cancelled && entry.account == input.account_id)
            {
                return Err(PluginFault::new(
                    ErrorCode::Conflict,
                    "此账号已有测试正在执行",
                ));
            }
            if entries.len() >= 64 || entries.values().filter(|entry| !entry.cancelled).count() >= 4
            {
                return Err(PluginFault::new(
                    ErrorCode::Conflict,
                    "已有 4 个测试正在执行",
                ));
            }
            entries.insert(
                input.id.clone(),
                Entry {
                    created: Instant::now(),
                    account: input.account_id.clone(),
                    key: None,
                    cancelled: false,
                    records: Vec::new(),
                },
            );
        }
        let mut reservation = Reservation {
            tests: self.clone(),
            id: input.id.clone(),
            ready: false,
        };
        let prepare = resources(call, &input.account_id);
        let key = tokio::select! {
            biased;
            _ = call.cancellation.cancelled() => return Err(PluginFault::new(ErrorCode::Cancelled, "测试已停止")),
            _ = tokio::time::sleep(Duration::from_secs(45)) => return Err(PluginFault::new(ErrorCode::Timeout, "测试资源准备超时")),
            result = prepare => result?,
        };
        {
            let mut entries = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            let entry = entries
                .get_mut(&input.id)
                .filter(|entry| !entry.cancelled)
                .ok_or_else(|| PluginFault::new(ErrorCode::Cancelled, "测试已停止"))?;
            entry.key = Some(key.clone());
        }
        reservation.ready = true;
        Ok(json!({"id":input.id,"accountId":input.account_id,"clientKeyId":key}))
    }

    /// 收集本插件实际处理过且归属于当前模型请求的诊断
    /// @param id 页面随机测试编号
    /// @param request_id 模型响应中的 x-gateway-request-id
    /// @return 匹配该模型请求的覆写记录
    pub(crate) fn result(&self, id: &str, request_id: &str) -> Result<Value, PluginFault> {
        if !valid_id(id) || request_id.is_empty() || request_id.len() > 128 {
            return Err(invalid());
        }
        let entries = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = entries
            .get(id)
            .filter(|entry| !entry.cancelled && entry.created.elapsed() < LIFETIME)
            .ok_or_else(|| {
                PluginFault::new(ErrorCode::InvalidInput, "测试记录已结束，请重新开始")
            })?;
        Ok(
            json!({"outbound":entry.records.iter().filter(|record| record.request_id == request_id).collect::<Vec<_>>()}),
        )
    }

    /// 结束测试关联，提前取消记录覆盖准备请求乱序的窗口
    /// @param call 含随机测试编号的页面调用
    /// @param cancel 是否登记提前取消
    /// @return 关联已结束
    pub(crate) fn end(
        &self,
        call: &TypedCall<ManagementRequest>,
        cancel: bool,
    ) -> Result<Value, PluginFault> {
        let input: End = serde_json::from_slice(&call.payload).map_err(|_| invalid())?;
        if !valid_id(&input.id) {
            return Err(invalid());
        }
        let mut entries = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|_, entry| entry.created.elapsed() < LIFETIME);
        if cancel {
            if let Some(entry) = entries.get_mut(&input.id) {
                entry.cancelled = true;
                entry.records.clear();
            } else if entries.len() < 64 {
                entries.insert(
                    input.id,
                    Entry {
                        created: Instant::now(),
                        account: String::new(),
                        key: None,
                        cancelled: true,
                        records: Vec::new(),
                    },
                );
            }
        } else {
            entries.remove(&input.id);
        }
        Ok(json!({"ended":true}))
    }

    /// 专用测试 Key 的实际账号必须与页面选择一致，校验先于 next 和网络发送
    /// @param metadata 宿主冻结的账号与 Key 归属
    /// @return 本次准备关联的测试编号
    pub(crate) fn target(&self, metadata: &Metadata) -> Result<Option<String>, PluginFault> {
        let entries = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = entries.iter().find(|(_, entry)| {
            !entry.cancelled
                && entry.created.elapsed() < LIFETIME
                && entry.key.as_deref() == Some(&metadata.client_key_id)
        });
        let Some((id, entry)) = entry else {
            return Ok(None);
        };
        if entry.account != metadata.account_id {
            return Err(PluginFault::new(
                ErrorCode::Rejected,
                "Header test account does not match the selected account.",
            ));
        }
        Ok(Some(id.clone()))
    }

    /// 保存当前中间件边界的前后对照，其他外层插件仍可能继续处理请求
    /// @param id 当前测试编号
    /// @param metadata 宿主冻结的请求身份
    /// @param target 本次 HTTP 或 WebSocket 候选目标
    /// @param rule 当前账号的真实保存规则
    /// @param before 本插件改写前的头部
    /// @param after 本插件改写后的头部
    /// @return 无，诊断只保存在当前进程内
    pub(crate) fn record(
        &self,
        id: &str,
        metadata: &Metadata,
        target: &Target,
        rule: &Rule,
        before: &[MiddlewareHeader],
        after: &[MiddlewareHeader],
    ) {
        let identity = |name: &str| {
            name.eq_ignore_ascii_case("user-agent") || name.eq_ignore_ascii_case("originator")
        };
        let project = |headers: &[MiddlewareHeader], configured: bool| {
            headers
                .iter()
                .filter(|header| {
                    identity(&header.name)
                        || (configured
                            && rule.enabled
                            && rule
                                .headers
                                .iter()
                                .any(|item| item.name.eq_ignore_ascii_case(&header.name)))
                })
                .map(|header| HeaderOverride {
                    name: header.name.clone(),
                    value: String::from_utf8_lossy(&header.value).into_owned(),
                })
                .collect()
        };
        let record = Evidence {
            request_id: metadata.request_id.clone(),
            account_id: metadata.account_id.clone(),
            attempt_index: metadata.attempt_index,
            transport: match target {
                Target::Http { .. } => "http",
                Target::WebSocket { .. } => "websocket",
            }
            .into(),
            override_enabled: rule.enabled,
            before_headers: project(before, false),
            headers: project(after, true),
        };
        let mut entries = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = entries.get_mut(id).filter(|entry| !entry.cancelled) {
            // 单次测试只保留最近四次准备，避免重试和同 Key 的其他调用无限占用内存
            if entry.records.len() == 4 {
                entry.records.remove(0);
            }
            entry.records.push(record);
        }
    }
}

async fn resources(
    call: &TypedCall<ManagementRequest>,
    account: &str,
) -> Result<String, PluginFault> {
    let mut accounts = Vec::new();
    let mut cursor = None;
    loop {
        let page = call
            .host
            .account_facts(AccountFactsQuery {
                provider_id: None,
                cursor,
                limit: 200,
            })
            .await?;
        accounts.extend(page.accounts);
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    let selected = accounts
        .iter()
        .find(|item| item.account_id == account)
        .ok_or_else(|| PluginFault::new(ErrorCode::InvalidInput, "测试账号已不存在"))?;
    if !selected.enabled {
        return Err(PluginFault::new(ErrorCode::InvalidInput, "所选账号已停用"));
    }
    let resource_key = format!("test.{account}");
    let name = format!("请求头测试 {} {account}", call.context.instance_id);
    let group = call
        .host
        .ensure_group(GroupEnsureRequest {
            resource_key: resource_key.clone(),
            name: name.clone(),
            color: "#4477AAFF".into(),
            description: Some("账号请求头插件的单账号测试范围".into()),
        })
        .await?;
    if !group.enabled {
        return Err(PluginFault::new(ErrorCode::InvalidInput, "测试分组已停用"));
    }
    if accounts
        .iter()
        .any(|item| item.account_id != account && item.group_ids.contains(&group.id))
    {
        return Err(PluginFault::new(
            ErrorCode::InvalidInput,
            "测试分组包含其他账号，请只保留所选账号",
        ));
    }
    call.host
        .change_group_members(GroupMembersChange {
            resource_key: resource_key.clone(),
            add: vec![account.into()],
            remove: Vec::new(),
        })
        .await?;
    let key = call
        .host
        .ensure_key(KeyEnsureRequest {
            resource_key: resource_key.clone(),
            name,
            group_resource_keys: vec![resource_key],
            max_concurrency: 1,
            requests_per_minute: 0,
            daily_limit_usd: "0".into(),
            weekly_limit_usd: "0".into(),
        })
        .await?;
    let facts = call
        .host
        .key_facts(ClientKeyFactsQuery {
            client_key_id: key.id.clone(),
        })
        .await?;
    if !key.enabled || !facts.enabled {
        return Err(PluginFault::new(ErrorCode::InvalidInput, "测试 Key 已停用"));
    }
    if facts.group_ids != [group.id] {
        return Err(PluginFault::new(
            ErrorCode::InvalidInput,
            "测试 Key 的分组范围已改变，请仅保留本账号的测试分组",
        ));
    }
    Ok(key.id)
}

fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn invalid() -> PluginFault {
    PluginFault::new(ErrorCode::InvalidInput, "测试参数无效")
}
