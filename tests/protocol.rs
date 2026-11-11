//! 运行真实插件子进程，验证管理桥合同、账号隔离、持久状态与错误及取消

use std::{collections::HashMap, process::Stdio, time::Duration};

use gateway_plugin_sdk::{
    CallContext, ErrorCode, Frame, Handshake, Manifest, Message, PluginFault, Stage,
    client::{read_frame, write_frame},
};
use serde_json::{Value, json};
use tokio::{
    process::{Child, ChildStdin, ChildStdout, Command},
    time::timeout,
};

const ACCOUNT: &str = "acct_10000000000000000000000000000001";
const OTHER: &str = "acct_10000000000000000000000000000002";
type State = HashMap<String, (Value, u64)>;

/// 模拟受管宿主回调，插件本身使用发行入口和真实 SDK 会话
struct Host {
    _child: Child,
    input: ChildStdin,
    output: ChildStdout,
    state: State,
    next_count: usize,
    fail_read: bool,
    fail_next: bool,
    cancel_next: bool,
    id: u64,
    http_status: u16,
    http_body: Vec<u8>,
    dispatch_count: usize,
}

impl Host {
    async fn start(state: State) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_cpr-account-headers"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut host = Self {
            input: child.stdin.take().unwrap(),
            output: child.stdout.take().unwrap(),
            _child: child,
            state,
            next_count: 0,
            fail_read: false,
            fail_next: false,
            cancel_next: false,
            id: 0,
            http_status: 200,
            http_body: Vec::new(),
            dispatch_count: 0,
        };
        let manifest = Manifest::from_author_slice(include_bytes!("../plugin.json")).unwrap();
        host.send(
            Message::Hello {
                handshake: Handshake {
                    protocol_version: 7,
                    artifact_sha256: "a".repeat(64),
                    plugin_id: manifest.plugin_id().unwrap(),
                    instance_id: "headers-test".into(),
                    generation: 1,
                    incarnation: "test".into(),
                    configuration: json!({}),
                    contributes: manifest.contributes,
                },
            },
            Vec::new(),
        )
        .await;
        assert!(matches!(
            host.read().await.message,
            Message::Ready {
                protocol_version: 7,
                ..
            }
        ));
        host
    }

    async fn send(&mut self, message: Message, payload: Vec<u8>) {
        write_frame(&mut self.input, &Frame { message, payload })
            .await
            .unwrap();
    }

    async fn read(&mut self) -> Frame {
        timeout(Duration::from_secs(10), read_frame(&mut self.output))
            .await
            .expect("插件响应应有界")
            .unwrap()
    }

    async fn call(
        &mut self,
        stage: Stage,
        method: &str,
        params: Value,
        payload: Vec<u8>,
    ) -> Result<(Value, Vec<u8>), PluginFault> {
        self.id += 1;
        let call_id = self.id * 2 - 1;
        self.send(
            Message::Call {
                id: call_id,
                method: method.into(),
                params,
                context: CallContext {
                    request: None,
                    call_id,
                    instance_id: "headers-test".into(),
                    generation: 1,
                    incarnation: "test".into(),
                    stage,
                    timeout_ms: 5000,
                    resource_stream: false,
                    resource_scope_id: format!("test-{call_id}"),
                    request_id: None,
                    attempt_id: None,
                    account_id: None,
                    credential_revision: None,
                },
            },
            payload,
        )
        .await;
        loop {
            let frame = self.read().await;
            match frame.message {
                Message::Result { id, result } if id == call_id => {
                    return Ok((result, frame.payload));
                }
                Message::Error { id, error } if id == call_id => return Err(error),
                Message::Cancelled { id } if id == call_id => {
                    return Err(PluginFault::new(ErrorCode::Cancelled, "测试调用已取消"));
                }
                Message::Resume { id, epoch } => {
                    self.send(Message::Resumed { id, epoch }, Vec::new()).await
                }
                Message::Suspend { .. } | Message::CallbackConsumed { .. } => {}
                Message::Callback {
                    id,
                    parent_id,
                    method,
                    params,
                } => {
                    let mut payload = Vec::new();
                    let reply = match method.as_str() {
                        "host.state.get" if self.fail_read => {
                            Err(PluginFault::new(ErrorCode::Fault, "测试存储故障"))
                        }
                        "host.state.get" => {
                            assert_eq!(params["namespace"], "headers");
                            let record = self.state.get(params["key"].as_str().unwrap()).map(|(value, version)| json!({"value":value,"version":version,"schema_version":1}));
                            Ok(json!({"record":record}))
                        }
                        "host.state.put" => {
                            let key = params["key"].as_str().unwrap().to_owned();
                            let current = self.state.get(&key).map(|(_, version)| *version);
                            if params["expected_version"] != json!(current) {
                                Err(PluginFault::new(ErrorCode::Conflict, "测试并发冲突"))
                            } else {
                                let version = current.unwrap_or(0) + 1;
                                self.state.insert(key, (params["value"].clone(), version));
                                Ok(json!({"version":version}))
                            }
                        }
                        "host.data.accounts.list" => {
                            assert_eq!(params, json!({}));
                            let query: Value = serde_json::from_slice(&frame.payload).unwrap();
                            let second = query["cursor"] == "next-page";
                            payload = serde_json::to_vec(&json!({"schema_version":1,"accounts":[{
                                "account_id":if second {OTHER} else {ACCOUNT},"provider_id":"openai","name":"测试账号","email":null,
                                "enabled":true,"group_ids":[],"updated_at_ms":0
                            }],"next_cursor":if second {None} else {Some("next-page")}})).unwrap();
                            Ok(json!({}))
                        }
                        "host.http.dispatch" => {
                            self.dispatch_count += 1;
                            assert_eq!(params["method"], "GET");
                            let uri = params["uri"].as_str().unwrap();
                            assert!(uri.starts_with("/api/admin/accounts/connection-test?"));
                            assert!(uri.contains("outboundPlugins=true"));
                            assert!(uri.contains(ACCOUNT));
                            assert!(uri.contains("modelId=test-model"));
                            assert_eq!(params["headers"], json!([]));
                            Ok(
                                json!({"status":self.http_status,"version":"HTTP/1.1","headers":[],"body":{"kind":"handle","handle":"probe-body"},"response":null,"session":false}),
                            )
                        }
                        "host.http.body_read" => {
                            assert_eq!(params["handle"], "probe-body");
                            payload = std::mem::take(&mut self.http_body);
                            Ok(json!({"eof":payload.is_empty(),"trailers":null}))
                        }
                        "host.middleware.next" => {
                            self.next_count += 1;
                            if self.cancel_next {
                                self.send(Message::Cancel { id: parent_id }, Vec::new())
                                    .await;
                                continue;
                            }
                            if self.fail_next {
                                Err(PluginFault::new(
                                    ErrorCode::Rejected,
                                    "Downstream rejected.",
                                ))
                            } else {
                                payload = frame.payload;
                                Ok(params)
                            }
                        }
                        _ => panic!("未预期回调：{method}"),
                    };
                    match reply {
                        Ok(result) => self.send(Message::Result { id, result }, payload).await,
                        Err(error) => self.send(Message::Error { id, error }, Vec::new()).await,
                    }
                }
                other => panic!("未预期消息：{other:?}"),
            }
        }
    }

    async fn management(
        &mut self,
        method: &str,
        path: &str,
        query: &str,
        body: Value,
    ) -> (u64, Value) {
        let (head, payload) = self.call(Stage::Management, "management.handle", json!({
            "method":method,"path":path,"query":query,"content_type":if method == "POST" {Some("application/json")} else {None},"headers":[]
        }), if method == "POST" { serde_json::to_vec(&body).unwrap() } else { Vec::new() }).await.unwrap();
        (
            head["status"].as_u64().unwrap(),
            serde_json::from_slice(&payload).unwrap(),
        )
    }

    async fn outbound(
        &mut self,
        account: &str,
        websocket: bool,
        body: &[u8],
    ) -> Result<(Value, Vec<u8>), PluginFault> {
        self.call(Stage::Outbound, "middleware.handle", json!({
            "metadata":{"request_id":"request","client_key_id":"key","account_group_ids":[],"provider":"openai","model":"test-model","account_id":account,"attempt_index":1},
            "request":{"target":if websocket {json!({"kind":"websocket","uri":"wss://example.test/responses"})} else {json!({"kind":"http","method":"POST","uri":"https://example.test/responses"})},
            "headers":[{"name":"User-Agent","value":b"original".to_vec()},{"name":"x-untouched","value":vec![0xff_u8,0x80]}]}
        }), body.to_vec()).await
    }
}

#[tokio::test]
async fn management_and_account_rules_survive_process_restart() {
    let mut host = Host::start(State::new()).await;
    let (_, payload) = host
        .call(
            Stage::Registration,
            "management.register",
            json!({}),
            Vec::new(),
        )
        .await
        .unwrap();
    let registration: Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(registration["pages"][0]["id"], "headers");
    let (status, page) = host.management("GET", "accounts", "", Value::Null).await;
    assert_eq!(status, 200);
    assert!(page["next_cursor"].is_null());
    assert_eq!(page["accounts"].as_array().unwrap().len(), 2);
    assert_eq!(page["accounts"][0]["override_enabled"], false);
    assert_eq!(page["accounts"][0]["header_count"], 0);
    let (_, page) = host
        .management("GET", "accounts", "cursor=next-page", Value::Null)
        .await;
    assert_eq!(page["accounts"][0]["account_id"], OTHER);
    assert!(page["next_cursor"].is_null());
    let (status, mut snapshot) = host
        .management("GET", "rule", &format!("accountId={ACCOUNT}"), Value::Null)
        .await;
    assert_eq!(status, 200);
    assert_eq!(snapshot["enabled"], false);
    snapshot["enabled"] = json!(true);
    snapshot["headers"] =
        json!([{"name":" User-Agent ","value":"custom-agent"},{"name":"x-new","value":""}]);
    let (status, saved) = host.management("POST", "rule", "", snapshot.clone()).await;
    assert_eq!(status, 200);
    assert_eq!(saved["version"], "1");
    assert_eq!(saved["headers"][0]["name"], "user-agent");
    let (status, page) = host
        .management("GET", "accounts", &format!("search={ACCOUNT}"), Value::Null)
        .await;
    assert_eq!(status, 200);
    assert_eq!(page["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(page["accounts"][0]["override_enabled"], true);
    assert_eq!(page["accounts"][0]["header_count"], 2);
    assert!(page["accounts"][0].get("headers").is_none());
    let (status, _) = host.management("POST", "rule", "", snapshot).await;
    assert_eq!(status, 409);
    let mut invalid = saved.clone();
    invalid["headers"] = json!([{"name":"x-header","value":"secret\r\ninjected: value"}]);
    let (status, error) = host.management("POST", "rule", "", invalid).await;
    assert_eq!(status, 400);
    assert!(!error.to_string().contains("secret"));
    for websocket in [false, true] {
        let body = if websocket {
            b"{\"type\":\"response.create\"}".as_slice()
        } else {
            &[0x1f, 0x8b, 0xff, 0x00]
        };
        let (head, returned) = host.outbound(ACCOUNT, websocket, body).await.unwrap();
        assert_eq!(returned, body);
        assert_eq!(head["headers"][0]["value"], json!([255, 128]));
        assert_eq!(head["headers"][1]["value"], json!(b"custom-agent".to_vec()));
        assert_eq!(
            head["target"]["kind"],
            if websocket { "websocket" } else { "http" }
        );
    }
    let (head, _) = host.outbound(OTHER, false, b"unchanged").await.unwrap();
    assert_eq!(head["headers"][0]["value"], json!(b"original".to_vec()));
    assert_eq!(host.next_count, 3);
    let state = host.state.clone();
    drop(host);
    let mut host = Host::start(state).await;
    let (head, _) = host.outbound(ACCOUNT, false, b"restart").await.unwrap();
    assert_eq!(head["headers"][1]["value"], json!(b"custom-agent".to_vec()));
    let mut disabled = saved;
    disabled["enabled"] = json!(false);
    assert_eq!(host.management("POST", "rule", "", disabled).await.0, 200);
    let (head, _) = host.outbound(ACCOUNT, false, b"disabled").await.unwrap();
    assert_eq!(head["headers"][0]["value"], json!(b"original".to_vec()));
}

#[tokio::test]
async fn failures_and_cancel_keep_session_usable_without_replaying_next() {
    let mut host = Host::start(State::new()).await;
    host.fail_read = true;
    let error = host.outbound(ACCOUNT, false, b"payload").await.unwrap_err();
    assert_eq!(error.message, "Account header rules are unavailable.");
    assert_eq!(host.next_count, 0);
    host.fail_read = false;
    host.fail_next = true;
    let error = host.outbound(ACCOUNT, false, b"payload").await.unwrap_err();
    assert_eq!(error.message, "Downstream rejected.");
    assert_eq!(host.next_count, 1);
    host.fail_next = false;
    host.cancel_next = true;
    assert!(host.outbound(ACCOUNT, false, b"payload").await.is_err());
    assert_eq!(host.next_count, 2);
    host.cancel_next = false;
    assert!(host.outbound(ACCOUNT, false, b"payload").await.is_ok());
    assert_eq!(host.next_count, 3);
}

#[tokio::test]
async fn account_tests_require_matching_evidence_and_keep_upstream_errors() {
    let mut host = Host::start(State::new()).await;
    let evidence = json!({"type":"outbound_request","accountId":ACCOUNT,"requestId":"req_probe","transport":"http","headers":[{"name":"user-agent","value":"fixture-agent"}]});
    for (account, complete, error, expected) in [
        (ACCOUNT, true, false, "success"),
        (OTHER, true, false, "unsupported"),
        (ACCOUNT, false, false, "unsupported"),
        (ACCOUNT, false, true, "failed"),
    ] {
        let mut event = evidence.clone();
        event["accountId"] = json!(account);
        let mut stream = format!("data: {event}\r\n\r\n");
        if complete {
            stream.push_str("data: {\"type\":\"test_complete\",\"success\":true}\n\n");
        }
        if error {
            stream.push_str("data: {\"type\":\"error\",\"error\":\"fixture failure\",\"sendState\":\"not_sent\"}\n\n");
        }
        host.http_body = stream.into_bytes();
        let (status, result) = host
            .management(
                "POST",
                "test",
                "",
                json!({"id":"a".repeat(32),"accountId":ACCOUNT,"modelId":"test-model"}),
            )
            .await;
        assert_eq!(status, 200);
        assert_eq!(result["status"], expected);
        if error {
            assert_eq!(result["message"], "fixture failure");
            assert_eq!(result["error"]["sendState"], "not_sent");
            assert_eq!(
                result["outbound"][0]["headers"][0]["value"],
                "fixture-agent"
            );
        }
    }
    host.http_status = 400;
    host.http_body = b"unknown field outboundPlugins".to_vec();
    let (_, result) = host
        .management(
            "POST",
            "test",
            "",
            json!({"id":"a".repeat(32),"accountId":ACCOUNT,"modelId":"test-model"}),
        )
        .await;
    assert_eq!(result["status"], "unsupported");
    let before = host.dispatch_count;
    assert_eq!(
        host.management("POST", "test/cancel", "", json!({"id":"b".repeat(32)}))
            .await
            .0,
        200
    );
    let (_, result) = host
        .management(
            "POST",
            "test",
            "",
            json!({"id":"b".repeat(32),"accountId":ACCOUNT,"modelId":"test-model"}),
        )
        .await;
    assert_eq!(result["status"], "cancelled");
    assert_eq!(host.dispatch_count, before);
    assert_eq!(
        host.management(
            "POST",
            "test",
            "",
            json!({"id":"invalid","accountId":ACCOUNT,"modelId":"test-model"})
        )
        .await
        .0,
        400
    );
}
