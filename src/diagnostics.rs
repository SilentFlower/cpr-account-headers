//! 为独立检测插件登记短期关联并收集本实例真实出站覆写记录

use cpr_account_headers::{HeaderOverride, Rule};
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        management::ManagementRequest,
        middleware::{
            MiddlewareHeader,
            outbound::{Metadata, Target},
        },
    },
    client::TypedCall,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};
const LIFETIME: Duration = Duration::from_secs(90);

/// 诊断仅存于当前实例，账号与 Key 均取真实出站身份复核
#[derive(Default, Clone)]
pub(crate) struct Diagnostics(Arc<Mutex<HashMap<String, Entry>>>);
struct Entry {
    created: Instant,
    account: String,
    key: String,
    request_id: Option<String>,
    cancelled: bool,
    records: Vec<Evidence>,
}
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
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Registration {
    id: String,
    account_id: String,
    client_key_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct End {
    id: String,
}

impl Diagnostics {
    /// 登记由独立检测插件发起的单次执行
    /// @param call 已认证管理员提交的测试身份
    /// @return 当前诊断编号
    pub(crate) fn register(
        &self,
        call: &TypedCall<ManagementRequest>,
    ) -> Result<Value, PluginFault> {
        let input: Registration = serde_json::from_slice(&call.payload).map_err(|_| invalid())?;
        if !valid_id(&input.id)
            || !valid_identity(&input.account_id)
            || !valid_identity(&input.client_key_id)
        {
            return Err(invalid());
        }
        let mut entries = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|_, e| e.created.elapsed() < LIFETIME);
        if entries.contains_key(&input.id) {
            return Err(PluginFault::new(
                ErrorCode::Conflict,
                "检测编号已登记或取消",
            ));
        }
        if entries.len() >= 64 {
            return Err(PluginFault::new(
                ErrorCode::Capacity,
                "诊断关联数量已达上限",
            ));
        }
        entries.insert(
            input.id.clone(),
            Entry {
                created: Instant::now(),
                account: input.account_id,
                key: input.client_key_id,
                request_id: None,
                cancelled: false,
                records: Vec::new(),
            },
        );
        Ok(json!({"id":input.id}))
    }

    /// 校验原始测试标记并关联宿主实际请求 ID
    /// @param metadata 宿主冻结的账号与 Key
    /// @param body Provider 准备的实际正文
    /// @return 本次诊断编号，普通请求返回空
    pub(crate) fn target(
        &self,
        metadata: &Metadata,
        body: &[u8],
    ) -> Result<Option<String>, PluginFault> {
        const FIELD: &str = "cpr_account_headers_test";
        if !body.windows(FIELD.len()).any(|w| w == FIELD.as_bytes()) {
            return Ok(None);
        }
        let value: Value = serde_json::from_slice(body).map_err(|_| {
            PluginFault::new(ErrorCode::Rejected, "Header test metadata is invalid.")
        })?;
        let Some(marker) = value.get("metadata").and_then(|m| m.get(FIELD)) else {
            return Ok(None);
        };
        let id = marker.as_str().ok_or_else(|| {
            PluginFault::new(ErrorCode::Rejected, "Header test metadata is invalid.")
        })?;
        let mut entries = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = entries.get_mut(id).ok_or_else(|| {
            PluginFault::new(
                ErrorCode::Rejected,
                "Header test association is unavailable.",
            )
        })?;
        if entry.cancelled
            || entry.created.elapsed() >= LIFETIME
            || entry.account != metadata.account_id
            || entry.key != metadata.client_key_id
            || entry
                .request_id
                .as_ref()
                .is_some_and(|request| request != &metadata.request_id)
        {
            return Err(PluginFault::new(
                ErrorCode::Rejected,
                "Header test identity does not match the registered execution.",
            ));
        }
        entry.request_id = Some(metadata.request_id.clone());
        Ok(Some(id.into()))
    }

    /// 保存当前中间件边界的请求头对照，认证材料仅在用户明确配置时进入改写后字段
    /// @param id 检测编号
    /// @param metadata 宿主冻结的实际执行身份
    /// @param target 本次传输目标
    /// @param rule 本次真实保存规则
    /// @param before 覆写前的实际头部
    /// @param after 覆写后的实际头部
    /// @return 无，诊断保存在短期内存
    pub(crate) fn record(
        &self,
        id: &str,
        metadata: &Metadata,
        target: &Target,
        rule: &Rule,
        before: &[MiddlewareHeader],
        after: &[MiddlewareHeader],
    ) {
        let project = |headers: &[MiddlewareHeader], configured: bool| {
            headers
                .iter()
                .filter(|h| {
                    h.name.eq_ignore_ascii_case("user-agent")
                        || h.name.eq_ignore_ascii_case("originator")
                        || (configured
                            && rule.enabled
                            && rule
                                .headers
                                .iter()
                                .any(|item| item.name.eq_ignore_ascii_case(&h.name)))
                })
                .map(|h| HeaderOverride {
                    name: h.name.clone(),
                    value: String::from_utf8_lossy(&h.value).into_owned(),
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
        if let Some(entry) = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(id)
            .filter(|e| !e.cancelled)
        {
            if entry.records.len() == 4 {
                entry.records.remove(0);
            }
            entry.records.push(record);
        }
    }

    /// 按真实模型请求 ID 提取当前检测的出站证据
    /// @param id 检测编号
    /// @param request_id 宿主返回的真实请求 ID
    /// @return 对应请求的真实覆写记录
    pub(crate) fn result(&self, id: &str, request_id: &str) -> Result<Value, PluginFault> {
        let entries = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = entries
            .get(id)
            .filter(|e| !e.cancelled && e.created.elapsed() < LIFETIME)
            .ok_or_else(invalid)?;
        Ok(
            json!({"outbound":entry.records.iter().filter(|e|e.request_id==request_id).collect::<Vec<_>>()}),
        )
    }

    /// 结束或取消诊断，取消墓碑覆盖请求乱序窗口
    /// @param call 含检测编号的管理员调用
    /// @param cancel 是否取消并拒绝后续出站准备
    /// @return 结束确认
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
        entries.retain(|_, e| e.created.elapsed() < LIFETIME);
        if cancel {
            if let Some(e) = entries.get_mut(&input.id) {
                e.cancelled = true;
                e.records.clear();
            } else if entries.len() < 64 {
                entries.insert(
                    input.id,
                    Entry {
                        created: Instant::now(),
                        account: String::new(),
                        key: String::new(),
                        request_id: None,
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
}
fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}
fn valid_identity(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control)
}
fn invalid() -> PluginFault {
    PluginFault::new(ErrorCode::InvalidInput, "诊断参数无效或关联已结束")
}
