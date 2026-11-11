# 账号请求头插件

为选定账号覆写发往模型上游的请求头，提供宿主管理端扩展页

## 使用

1. 从 [Releases](https://github.com/SilentFlower/cpr-account-headers/releases/latest) 下载服务器平台对应的 `.tar.gz` 安装包，在 CPR 插件管理中上传，接受完整信任并启用实例
2. 核对自动创建的 `outbound` 中间件绑定，按需限制 Provider、模型、Key 或分组范围
3. 从扩展页打开「账号请求头」，查看账号名称、邮箱、分组及覆写状态
4. 点击「编辑」，设置请求头名称和值，打开「启用覆写」并保存
5. 定向测试使用独立的 [账号请求头检测插件](https://github.com/SilentFlower/cpr-account-headers-test)，在检测页选择账号与模型

也可通过 CPR 的 GitHub 安装入口填写仓库 `SilentFlower/cpr-account-headers`，选择对应平台的 Release 附件，标签留空使用最新稳定版

所属分组使用宿主当前名称，名称缺失时保留分组 ID，单个账号可同时属于多个分组

每个账号单独保存规则，支持 OpenAI 与 xAI 账号。头名不区分大小写，配置值替换原请求全部同名字段，缺失字段会追加。值留空表示发送空值，移除配置项后恢复宿主生成的原始字段。关闭「启用覆写」保留配置并恢复原请求头

头值直接显示，便于查看和编辑。每个账号最多 64 项、64 KiB，名称重复或含非法字符时拒绝保存。保存采用精确版本检查，遇到并发修改或结果待确认时重新读取规则后继续编辑

## 请求头检测

独立的「账号请求头检测」插件提供只读账号列表、规则详情、统一分组与模型选择。检测插件通过宿主定向模型接口执行，本插件继续参与真实出站覆写，两者按随机检测编号及宿主实际请求 ID 关联诊断

本插件只注册出站中间件和管理页面，账号调度由宿主与已安装的调度插件负责。检测所用 Key、分组、平台和模型须在出站绑定范围内

诊断仅在内存保留 90 秒，每次最多保存四条真实出站准备记录。取消后拒绝该诊断继续出站，原始认证材料沿用白名单投影，用户明确配置的覆写字段按当前规则展示

旧版单账号测试资源和预发布版统一测试设置保留，定向检测的分组设置由独立检测插件管理

## 扩展页

以下页面使用合成账号和请求头值展示

![账号请求头扩展页](docs/screenshots/light.png)

[深色主题](docs/screenshots/dark.png) · [窄屏布局](docs/screenshots/mobile.png)

![编辑请求头弹窗](docs/screenshots/editor.png)

## 作用范围

- 使用 `middleware v6` 的 `outbound` 阶段，在 Provider 序列化完成后覆写头部，目标 URL、正文与账号归属保持原样
- 支持模型 HTTP 请求与 WebSocket 候选握手头，OAuth、额度刷新等后台请求沿用宿主行为
- 账号页「测试连接」沿用原生诊断；独立检测插件使用宿主定向执行接口，账号资格、租约、并发与用量由宿主维护
- WebSocket 已建立连接沿用原握手头，新建连接使用当前规则，精确续接由宿主管理
- `Authorization` 等认证头也按配置值覆写，实际发送仍受 HTTP 和 WebSocket 传输格式约束，WebSocket 握手头值要求 ASCII
- 多个中间件按宿主绑定顺序组合，本插件在下层准备完成后应用覆写，外层中间件仍可继续处理
- 规则保存在当前插件实例的私有状态中，停用再启用或进程重启后保留，每次上游准备读取当前账号规则，状态读取失败时本次准备返回错误

## 兼容与构建

面向 CPR `>=3.43.0`，使用清单 v2、RPC v7、`middleware v6` 与 `management v2`。SDK 固定到 `1a2a4a56aed0a9660f173cb1d125b9728e2f214c`，UI 固定为 `@codex-proxy/ui 0.8.0`

需要 Rust `1.99.1`、Node.js `>=24`、pnpm `12.6.0` 和与目标 SDK 合同匹配的 `cpr-plugin`

```bash
cargo fmt -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
pnpm --dir frontend install --frozen-lockfile
pnpm --dir frontend lint
pnpm --dir frontend build
CPR_PLUGIN_CLI=/path/to/cpr-plugin bash scripts/package.sh
```

默认构建 Linux x86_64，可用 `CPR_PLUGIN_TARGET` 指定已有工具链支持的平台。构建脚本生成页面、二进制、第三方许可和带 SHA-256 校验文件的安装包，源码目录保持独立于宿主 workspace

测试使用真实插件子进程和模拟宿主回调，覆盖账号隔离、覆写与透传、分页、状态重启恢复、并发冲突、真实出站诊断关联、账号与 Key 校验、取消墓碑、错误与取消

本地集成验收使用原版宿主、独立数据库与合成上游，线上账号的实际结果在相关 Issue 中继续反馈

## 许可证

Apache-2.0，第三方依赖许可见 `THIRD_PARTY_LICENSES.txt`
