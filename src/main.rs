//! 启动独立进程，组合账号出站覆写和扩展页管理能力

mod management;
mod store;
mod testing;

use std::error::Error;

use gateway_plugin_sdk::{
    Manifest, PluginFault,
    client::{OutboundCall, OutboundRequest, PluginBuilder, PluginSession, SessionConfig},
};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let session = PluginSession::accept(
        tokio::io::stdin(),
        tokio::io::stdout(),
        SessionConfig::default(),
    )
    .await?;
    let manifest = Manifest::from_author_slice(include_bytes!("../plugin.json"))?;
    if session.handshake().plugin_id != manifest.plugin_id()?
        || session.handshake().contributes != manifest.contributes
    {
        return Err("插件握手身份不匹配".into());
    }
    let tests = testing::Tests::default();
    let outbound_tests = tests.clone();
    let plugin = PluginBuilder::from_json(include_bytes!("../plugin.json"))?
        .middleware(move |call| outbound(call, outbound_tests.clone()))?
        .management(management::registration(), move |call| {
            management::handle(call, tests.clone())
        })?
        .build()?;
    session.run(plugin).await?;
    Ok(())
}

/// 按实际选中账号读取规则，在准备链返回后覆写头部
/// @param call 宿主冻结归属的上游准备调用
/// @param tests 当前进程的测试关联与诊断记录
/// @return 正文与目标保持原样的准备请求
async fn outbound(
    call: OutboundCall,
    tests: testing::Tests,
) -> Result<OutboundRequest, PluginFault> {
    let test = tests.target(&call.metadata)?;
    let snapshot = store::load(&call.host, call.metadata.account_id.clone())
        .await
        .map_err(|error| PluginFault::new(error.code, "Account header rules are unavailable."))?;
    let mut request = call.next.run(call.request).await?;
    let before = test.as_ref().map(|_| request.head.headers.clone());
    snapshot.rule.apply(&mut request.head.headers);
    if let (Some(id), Some(before)) = (test, before) {
        tests.record(
            &id,
            &call.metadata,
            &request.head.target,
            &snapshot.rule,
            &before,
            &request.head.headers,
        );
    }
    Ok(request)
}
