//! 通过公开宿主 HTTP 回调读取页面所需的原生管理数据

use gateway_plugin_sdk::{
    call::middleware::{MiddlewareHeader, http::Version},
    client::{HostClient, HttpBody, HttpFrame, HttpRequest},
};
use serde_json::Value;

/// 读取有界 JSON 成功信封，缺失或失败由页面保留未知状态
/// @param host 当前管理调用的受管客户端
/// @param uri 已确认的宿主只读管理接口
/// @return 原生业务数据，失败时返回 None
pub(crate) async fn get(host: &HostClient, uri: String) -> Option<Value> {
    send(host, "GET", uri, None).await
}

/// 调用原生管理接口，写操作只用于本插件的专用测试 Key
/// @param host 当前管理员调用的受管客户端
/// @param method 已确认的 HTTP 方法
/// @param uri 已确认的管理接口
/// @param body 可选 JSON 正文
/// @return 已解包的成功业务数据
pub(crate) async fn send(
    host: &HostClient,
    method: &str,
    uri: String,
    body: Option<Value>,
) -> Option<Value> {
    let bytes = body.as_ref().map(serde_json::to_vec).transpose().ok()?;
    let mut response = host
        .dispatch_http(HttpRequest {
            settings: Value::Null,
            method: method.into(),
            uri,
            version: Version::Http11,
            headers: if bytes.is_some() {
                vec![MiddlewareHeader {
                    name: "content-type".into(),
                    value: b"application/json".to_vec(),
                }]
            } else {
                Vec::new()
            },
            timeout_ms: Some(if method == "GET" { 1500 } else { 5000 }),
            body: bytes.map_or_else(HttpBody::empty, HttpBody::from_bytes),
        })
        .await
        .ok()?;
    if !matches!(response.status, 200 | 201) {
        let _ = response.body.close().await;
        return None;
    }
    let mut bytes = Vec::new();
    loop {
        let frame = match response.body.read().await {
            Ok(frame) => frame,
            Err(_) => {
                let _ = response.body.close().await;
                return None;
            }
        };
        let Some(frame) = frame else { break };
        if let HttpFrame::Data(chunk) = frame {
            if bytes.len().saturating_add(chunk.len()) > 2 * 1024 * 1024 {
                let _ = response.body.close().await;
                return None;
            }
            bytes.extend(chunk);
        }
    }
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    if value.get("code").and_then(Value::as_u64) != Some(200) {
        return None;
    }
    value.get("data").cloned()
}
