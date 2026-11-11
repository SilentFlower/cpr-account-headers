//! 运行真实插件子进程，验证管理桥合同、账号隔离、持久状态与错误及取消

use std::{
    collections::{HashMap, VecDeque},
    process::Stdio,
    time::Duration,
};

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
    resource_calls: usize,
    key_created: usize,
    key_group: String,
    changed_key_scope: bool,
    http_bodies: HashMap<String, VecDeque<Vec<u8>>>,
    fail_groups: bool,
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
            resource_calls: 0,
            key_created: 0,
            key_group: "grp_11111111111111111111111111111111".into(),
            changed_key_scope: false,
            http_bodies: HashMap::new(),
            fail_groups: false,
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
                    timeout_ms: if matches!(stage, Stage::Routing | Stage::Scheduling) {
                        2000
                    } else {
                        5000
                    },
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
                            assert!(matches!(
                                params["namespace"].as_str(),
                                Some("headers" | "testing")
                            ));
                            let state_key = if params["namespace"] == "testing" {
                                format!("testing:{}", params["key"].as_str().unwrap())
                            } else {
                                params["key"].as_str().unwrap().into()
                            };
                            let record = self.state.get(&state_key).map(|(value, version)| json!({"value":value,"version":version,"schema_version":1}));
                            Ok(json!({"record":record}))
                        }
                        "host.state.put" => {
                            let key = if params["namespace"] == "testing" {
                                format!("testing:{}", params["key"].as_str().unwrap())
                            } else {
                                params["key"].as_str().unwrap().into()
                            };
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
                                "enabled":true,"group_ids":if !second {vec!["group-main","group-missing"]} else {vec![]},"updated_at_ms":0
                            }],"next_cursor":if second {None} else {Some("next-page")}})).unwrap();
                            Ok(json!({}))
                        }
                        "host.http.dispatch" => {
                            let uri = params["uri"].as_str().unwrap();
                            let second = uri.contains("page=2&");
                            let status = if self.fail_groups { 503 } else { 200 };
                            let data = if uri.starts_with("/api/admin/account-groups/options?") {
                                json!({"items":if second {json!([{"id":"group-main","name":"当前 VIP","enabled":true}])} else {json!([{"id":"unrelated","name":"其他组","enabled":true}])},"page":{"page":if second {2} else {1},"pageSize":100,"total":101,"totalPages":2}})
                            } else if uri.starts_with("/api/admin/client-keys?") {
                                json!({"items":if self.key_created > 0 {json!([{"id":"key","name":format!("请求头测试 headers-test {}",self.key_group),"label":"cpr-account-headers:headers-test"}])} else {json!([])},"nextCursor":null})
                            } else if uri == "/api/admin/client-keys/create" {
                                assert_eq!(params["method"], "POST");
                                let body: Value = serde_json::from_slice(&frame.payload).unwrap();
                                self.key_group = body["groupIds"][0].as_str().unwrap().into();
                                assert_eq!(body["maxConcurrency"], 4);
                                self.key_created += 1;
                                json!({"id":"key","plaintextKey":"never-expose-this"})
                            } else {
                                panic!("未预期的管理接口 {uri}")
                            };
                            let bytes =
                                serde_json::to_vec(&json!({"code":status,"data":data})).unwrap();
                            let handle = format!("groups-{id}");
                            self.http_bodies
                                .insert(handle.clone(), VecDeque::from([bytes]));
                            Ok(
                                json!({"status":status,"version":"HTTP/1.1","headers":[],"body":{"kind":"handle","handle":handle},"response":null,"session":false}),
                            )
                        }
                        "host.http.body_read" => {
                            let bytes = self
                                .http_bodies
                                .get_mut(params["handle"].as_str().unwrap())
                                .unwrap()
                                .pop_front();
                            let eof = bytes.is_none();
                            payload = bytes.unwrap_or_default();
                            Ok(json!({"eof":eof,"trailers":null}))
                        }
                        "host.http.body_close" => Ok(json!({})),
                        "host.data.access.check" => {
                            let query: Value = serde_json::from_slice(&frame.payload).unwrap();
                            let account = query["account_ids"][0].as_str().unwrap();
                            payload = serde_json::to_vec(&json!({"schema_version":1,"client_key_id":"key","accounts":[{"account_id":account,"allowed":self.key_group != "group-main" || account == ACCOUNT}]})).unwrap();
                            Ok(json!({}))
                        }
                        "host.groups.ensure"
                        | "host.groups.change_members"
                        | "host.keys.ensure"
                        | "host.data.keys.get" => {
                            assert_eq!(params, json!({}));
                            let query: Value = serde_json::from_slice(&frame.payload).unwrap();
                            self.resource_calls += 1;
                            let value = match method.as_str() {
                                "host.groups.ensure" => {
                                    assert_eq!(query["resource_key"], "test.shared");
                                    json!({"id":"grp_11111111111111111111111111111111","name":"测试分组","enabled":true})
                                }
                                "host.groups.change_members" => {
                                    assert_eq!(query["add"], json!([ACCOUNT, OTHER]));
                                    assert_eq!(query["remove"], json!([]));
                                    json!({"added":1,"removed":0})
                                }
                                "host.data.keys.get" => {
                                    assert_eq!(query["client_key_id"], "key");
                                    json!({"schema_version":1,"client_key_id":"key","enabled":true,"group_ids":[if self.changed_key_scope {"grp_22222222222222222222222222222222"} else {self.key_group.as_str()}]})
                                }
                                _ => unreachable!(),
                            };
                            payload = serde_json::to_vec(&value).unwrap();
                            Ok(json!({}))
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

    async fn route_test(&mut self, id: &str, request_id: &str) -> Value {
        self.call(Stage::Routing, "policy.route_model", json!({"request_id":request_id,"operation":"responses","protocol":"openai","model":"test-model","available_providers":["openai","xai"],"headers":[]}), serde_json::to_vec(&json!({"model":"test-model","metadata":{"cpr_account_headers_test":id}})).unwrap()).await.unwrap().0
    }

    async fn schedule(&mut self, request_id: &str, target_available: bool) -> Value {
        self.call(Stage::Scheduling, "policy.schedule_account", json!({"request_id":request_id,"attempt_index":1,"provider":"openai","model":"test-model","candidates":[
            {"account_id":ACCOUNT,"available":target_available,"weight":1,"in_flight":0,"maximum_concurrency":1},
            {"account_id":OTHER,"available":true,"weight":1,"in_flight":0,"maximum_concurrency":1}
        ]}), Vec::new()).await.unwrap().0
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
    assert_eq!(
        page["accounts"][0]["group_names"],
        json!(["当前 VIP", "group-missing"])
    );
    host.fail_groups = true;
    let (_, fallback) = host.management("GET", "accounts", "", Value::Null).await;
    assert_eq!(
        fallback["accounts"][0]["group_names"],
        json!(["group-main", "group-missing"])
    );
    host.fail_groups = false;
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
async fn account_tests_share_resources_and_pin_real_outbound_records() {
    let mut host = Host::start(State::from([(ACCOUNT.into(), (json!({"enabled":true,"headers":[{"name":"user-agent","value":"fixture-agent"},{"name":"originator","value":"pi"}]}),1))])).await;
    let id = "a".repeat(32);
    let input = json!({"id":id,"accountId":ACCOUNT,"modelId":"test-model"});
    let (status, plan) = host.management("POST", "test", "", input.clone()).await;
    assert_eq!(status, 200);
    assert_eq!(plan["clientKeyId"], "key");
    assert_eq!(host.resource_calls, 3);
    assert_eq!(host.route_test(&id, "request").await["decision"], "route");
    assert_eq!(
        host.management(
            "POST",
            "test",
            "",
            json!({"id":"b".repeat(32),"accountId":ACCOUNT,"modelId":"test-model"})
        )
        .await
        .0,
        409
    );
    let error = host
        .outbound(OTHER, false, b"wrong-account")
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Rejected);
    assert_eq!(host.next_count, 0);
    for websocket in [false, true] {
        let (head, body) = host
            .outbound(ACCOUNT, websocket, b"real-request")
            .await
            .unwrap();
        assert_eq!(body, b"real-request");
        assert_eq!(
            head["headers"][1]["value"],
            json!(b"fixture-agent".to_vec())
        );
    }
    let other_id = "b".repeat(32);
    assert_eq!(
        host.management(
            "POST",
            "test",
            "",
            json!({"id":other_id,"accountId":OTHER,"modelId":"test-model"})
        )
        .await
        .0,
        200
    );
    assert_eq!(
        host.route_test(&other_id, "other-request").await["decision"],
        "route"
    );
    assert_eq!(
        host.schedule("other-request", true).await,
        json!({"decision":"pick","account_id":OTHER})
    );
    assert_eq!(host.key_created, 1, "不同账号的同时测试复用同一 Key");
    host.management("POST", "test/finish", "", json!({"id":other_id}))
        .await;
    let query = format!("id={id}&requestId=request");
    let (status, captured) = host.management("GET", "test", &query, Value::Null).await;
    assert_eq!(status, 200);
    let records = captured["outbound"].as_array().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["accountId"], ACCOUNT);
    assert_eq!(
        records[0]["beforeHeaders"],
        json!([{"name":"User-Agent","value":"original"}])
    );
    assert_eq!(
        records[0]["headers"],
        json!([{"name":"user-agent","value":"fixture-agent"},{"name":"originator","value":"pi"}])
    );
    assert_eq!(records[1]["transport"], "websocket");
    let (_, other) = host
        .management(
            "GET",
            "test",
            &format!("id={id}&requestId=other-request"),
            Value::Null,
        )
        .await;
    assert_eq!(other["outbound"], json!([]));
    assert_eq!(
        host.management("POST", "test/finish", "", json!({"id":id}))
            .await
            .0,
        200
    );
    assert_eq!(
        host.management("GET", "test", &query, Value::Null).await.0,
        400
    );
    assert_eq!(host.management("POST", "test", "", input).await.0, 200);
    assert_eq!(host.key_created, 1);
}

#[tokio::test]
async fn account_tests_reject_changed_resource_scope_and_handle_early_cancel() {
    let mut host = Host::start(State::new()).await;
    let id = "a".repeat(32);
    let input = json!({"id":id,"accountId":ACCOUNT,"modelId":"test-model"});
    host.changed_key_scope = true;
    let (status, error) = host.management("POST", "test", "", input.clone()).await;
    assert_eq!(status, 400);
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("分组范围")
    );
    host.changed_key_scope = false;
    assert_eq!(
        host.management("POST", "test/cancel", "", json!({"id":id}))
            .await
            .0,
        200
    );
    let before = host.resource_calls;
    assert_eq!(host.management("POST", "test", "", input).await.0, 409);
    assert_eq!(host.resource_calls, before);
    assert_eq!(
        host.management(
            "POST",
            "test",
            "",
            json!({"id":"b".repeat(32),"accountId":ACCOUNT,"modelId":"test-model"})
        )
        .await
        .0,
        200
    );
}

#[tokio::test]
async fn shared_group_settings_and_scheduling_keep_tests_isolated() {
    let mut host = Host::start(State::new()).await;
    let (_, initial) = host
        .management("GET", "test/settings", "", Value::Null)
        .await;
    assert!(initial["settings"]["groupId"].is_null());
    let (status, saved) = host
        .management(
            "POST",
            "test/settings",
            "",
            json!({"groupId":"group-main","version":null}),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(saved["version"], "1");
    assert_eq!(
        host.management(
            "POST",
            "test/settings",
            "",
            json!({"groupId":null,"version":null})
        )
        .await
        .0,
        409
    );
    let state = host.state.clone();
    let mut host = Host::start(state).await;
    let id = "c".repeat(32);
    let input = json!({"id":id,"accountId":ACCOUNT,"modelId":"test-model"});
    assert_eq!(host.management("POST", "test", "", input).await.0, 200);
    assert_eq!(host.resource_calls, 1, "已有分组只读取 Key 事实");
    assert_eq!(host.key_group, "group-main");
    assert_eq!(
        host.management(
            "POST",
            "test",
            "",
            json!({"id":"d".repeat(32),"accountId":OTHER,"modelId":"test-model"})
        )
        .await
        .0,
        400
    );
    assert_eq!(
        host.route_test(&id, "request").await,
        json!({"decision":"route","provider":"openai"})
    );
    assert_eq!(
        host.schedule("ordinary", true).await,
        json!({"decision":"delegate"})
    );
    assert_eq!(
        host.schedule("request", true).await,
        json!({"decision":"pick","account_id":ACCOUNT})
    );
    assert_eq!(
        host.schedule("request", false).await,
        json!({"decision":"delegate_filtered","exclude_account_ids":[OTHER]})
    );
    assert_eq!(
        host.route_test(&id, "another-request").await["decision"],
        "reject"
    );
    assert_eq!(
        host.management(
            "POST",
            "test/settings",
            "",
            json!({"groupId":null,"version":"1"})
        )
        .await
        .0,
        409
    );
    host.management("POST", "test/cancel", "", json!({"id":id}))
        .await;
    assert_eq!(host.schedule("request", true).await["decision"], "reject");
    assert_eq!(
        host.outbound(ACCOUNT, false, b"cancelled")
            .await
            .unwrap_err()
            .code,
        ErrorCode::Rejected
    );
    assert_eq!(
        host.management(
            "POST",
            "test/settings",
            "",
            json!({"groupId":null,"version":"1"})
        )
        .await
        .0,
        200
    );
    let mut restarted = Host::start(host.state.clone()).await;
    assert_eq!(
        restarted.route_test(&id, "request").await["decision"],
        "reject"
    );
}
