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
    let plugin = PluginBuilder::from_json(include_bytes!("../plugin.json"))?
        .middleware(outbound)?
        .management(management::registration(), move |call| {
            management::handle(call, tests.clone())
        })?
        .build()?;
    session.run(plugin).await?;
    Ok(())
}

/// 按实际选中账号读取规则，在准备链返回后覆写头部
/// @param call 宿主冻结归属的上游准备调用
/// @return 正文与目标保持原样的准备请求
async fn outbound(call: OutboundCall) -> Result<OutboundRequest, PluginFault> {
    let snapshot = store::load(&call.host, call.metadata.account_id)
        .await
        .map_err(|error| PluginFault::new(error.code, "Account header rules are unavailable."))?;
    let mut request = call.next.run(call.request).await?;
    snapshot.rule.apply(&mut request.head.headers);
    Ok(request)
}
