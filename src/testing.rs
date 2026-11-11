//! 固定账号的真实出站测试与有界取消登记，结果仅保留在当前调用

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use form_urlencoded::Serializer;
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{management::ManagementRequest, middleware::http::Version},
    client::{HttpBody, HttpFrame, HttpRequest, TypedCall},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::oneshot;

const TIMEOUT: Duration = Duration::from_secs(45);
const MAX_BODY: usize = 512 * 1024;

/// 进程内测试登记，限制同时执行数量与提前取消记录寿命
#[derive(Default, Clone)]
pub(crate) struct Tests(Arc<Mutex<HashMap<String, Entry>>>);

struct Entry {
    created: Instant,
    active: bool,
    cancel: Option<oneshot::Sender<()>>,
}

struct Run {
    tests: Tests,
    id: String,
}

impl Drop for Run {
    fn drop(&mut self) {
        self.tests
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
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
struct Cancel {
    id: String,
}

impl Tests {
    /// 请求真实账号测试，期限与父调用共同约束宿主子请求
    /// @param call 当前页面管理调用
    /// @return 有限测试结果与最终准备的身份头
    pub(crate) async fn run(
        &self,
        call: &TypedCall<ManagementRequest>,
    ) -> Result<Value, PluginFault> {
        let input: Input = serde_json::from_slice(&call.payload).map_err(|_| invalid())?;
        if !valid_id(&input.id)
            || input.account_id.len() > 128
            || input.account_id.is_empty()
            || input.model_id.trim().is_empty()
            || input.model_id.len() > 256
            || input.model_id.chars().any(char::is_control)
        {
            return Err(invalid());
        }
        let started = Instant::now();
        let (send, receive) = oneshot::channel();
        {
            let mut entries = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            entries.retain(|_, entry| {
                entry.active || entry.created.elapsed() < Duration::from_secs(90)
            });
            if let Some(entry) = entries.get(&input.id) {
                if !entry.active {
                    entries.remove(&input.id);
                    return Ok(json!({"status":"cancelled","elapsedMs":0}));
                }
                return Err(PluginFault::new(ErrorCode::Conflict, "测试编号正在使用"));
            }
            if entries.values().filter(|entry| entry.active).count() >= 4 {
                return Err(PluginFault::new(
                    ErrorCode::Conflict,
                    "已有 4 个测试正在执行",
                ));
            }
            entries.insert(
                input.id.clone(),
                Entry {
                    created: started,
                    active: true,
                    cancel: Some(send),
                },
            );
        }
        let _run = Run {
            tests: self.clone(),
            id: input.id,
        };
        let query = Serializer::new(String::new())
            .append_pair("accountId", &input.account_id)
            .append_pair("modelId", input.model_id.trim())
            .append_pair("outboundPlugins", "true")
            .finish();
        // 子调用返回或被丢弃后，宿主按父调用资源作用域回收正文与执行
        let operation = async {
            let mut response = call
                .host
                .dispatch_http(HttpRequest {
                    settings: Value::Null,
                    method: "GET".into(),
                    uri: format!("/api/admin/accounts/connection-test?{query}"),
                    version: Version::Http11,
                    headers: Vec::new(),
                    timeout_ms: Some(TIMEOUT.as_millis() as u64),
                    body: HttpBody::empty(),
                })
                .await?;
            let mut bytes = Vec::new();
            while let Some(frame) = response.body.read().await? {
                if let HttpFrame::Data(data) = frame {
                    if bytes.len().saturating_add(data.len()) > MAX_BODY {
                        response.body.close().await?;
                        return Err(PluginFault::new(
                            ErrorCode::Rejected,
                            "测试响应超过大小限制",
                        ));
                    }
                    bytes.extend(data);
                }
            }
            Ok(parse(response.status, &bytes, &input.account_id))
        };
        let mut result = tokio::select! {
            biased;
            _ = call.cancellation.cancelled() => json!({"status":"cancelled"}),
            _ = receive => json!({"status":"cancelled"}),
            _ = tokio::time::sleep(TIMEOUT) => json!({"status":"timeout","message":"测试已达到 45 秒期限"}),
            result = operation => match result {
                Ok(value) => value,
                Err(error) => json!({"status":"failed","message":error.message}),
            },
        };
        result["elapsedMs"] = json!(started.elapsed().as_millis() as u64);
        Ok(result)
    }

    /// 取消当前测试，允许取消先于测试请求到达
    /// @param call 带有随机测试编号的页面调用
    /// @return 取消请求已登记
    pub(crate) fn cancel(&self, call: &TypedCall<ManagementRequest>) -> Result<Value, PluginFault> {
        let input: Cancel = serde_json::from_slice(&call.payload).map_err(|_| invalid())?;
        if !valid_id(&input.id) {
            return Err(invalid());
        }
        let mut entries = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        entries
            .retain(|_, entry| entry.active || entry.created.elapsed() < Duration::from_secs(90));
        if let Some(entry) = entries.get_mut(&input.id) {
            if let Some(cancel) = entry.cancel.take() {
                let _ = cancel.send(());
            }
        } else if entries.len() < 64 {
            // 短期取消记录覆盖两个页面请求乱序的窗口，数量与寿命均有限
            entries.insert(
                input.id,
                Entry {
                    created: Instant::now(),
                    active: false,
                    cancel: None,
                },
            );
        } else {
            return Err(PluginFault::new(ErrorCode::Conflict, "测试取消登记已满"));
        }
        Ok(json!({"cancelled":true}))
    }
}

fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn invalid() -> PluginFault {
    PluginFault::new(ErrorCode::InvalidInput, "测试参数无效")
}

fn parse(status: u16, bytes: &[u8], account_id: &str) -> Value {
    if status == 400 {
        return json!({"status":"unsupported","message":"宿主拒绝出站测试参数，请确认已安装配套宿主"});
    }
    if status != 200 {
        return json!({"status":"failed","message":format!("宿主测试入口返回 HTTP {status}")});
    }
    let text = String::from_utf8_lossy(bytes).replace("\r\n", "\n");
    let mut outbound = Vec::new();
    let mut content = String::new();
    let mut completed = false;
    let mut error = None;
    for event in text.split("\n\n") {
        let data = event
            .lines()
            .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
            .collect::<Vec<_>>()
            .join("\n");
        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        match value["type"].as_str() {
            Some("outbound_request") if value["accountId"] == account_id => outbound.push(value),
            Some("content") => {
                if let Some(text) = value["text"].as_str() {
                    content.push_str(text);
                }
            }
            Some("test_complete") if value["success"] == true => completed = true,
            Some("error") => error = Some(value),
            _ => {}
        }
    }
    if let Some(error) = error {
        return json!({"status":"failed","message":error["error"],"error":error,"outbound":outbound,"text":content});
    }
    if !completed || outbound.is_empty() {
        return json!({"status":"unsupported","message":"宿主未返回所选账号的出站诊断，请检查插件绑定与配套宿主","outbound":outbound});
    }
    json!({"status":"success","outbound":outbound,"text":content})
}
