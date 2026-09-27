# 构建 CONST API Local

[English](SOURCE.md) · [README](README.zh-CN.md)

这份源码用于构建管理自己渠道的桌面网关，不是 Releases 中带平台功能的官方客户端源码。

## 环境要求

- 与 `client/.node-version` 一致的 Node.js 和 npm。
- `client/rust-toolchain.toml` 指定的稳定版 Rust。
- 对应系统的 [Tauri 2 原生构建依赖](https://v2.tauri.app/start/prerequisites/)：
  Windows 需要 C++ Build Tools 和 WebView2；macOS 需要 Xcode Command Line Tools；
  Linux 需要 C/C++ 工具链以及 WebKitGTK 4.1、GTK 3、OpenSSL、librsvg、AppIndicator 开发包。

依赖解析由 lockfile 固定。不需要私有仓库、服务器、生产环境文件、签名密钥或上游参考仓库。

## 运行和构建

从仓库根目录执行：

```sh
cd client
npm ci
npm run dev
```

构建包含前端资源的可执行程序：

```sh
npm run build
```

默认输出为 `client/src-tauri/target/release/const-api-local`，Windows 文件名带 `.exe`。
`npm run build -- --debug` 可更快生成开发构建，输出在 `target/debug`。
设置了 `CARGO_TARGET_DIR` 时以该目录为准。

构建使用 `--no-bundle`，不生成带签名的安装包、自动更新产物或 GitHub Release。
运行需要图形会话，正常打开程序即可；彻底退出请使用托盘或菜单栏中的**退出**。
已经移除的旧命令行模式不再是程序接口。

## 可选的 Antigravity OAuth 配置

公开源码不内置 Google OAuth 应用凭据。Antigravity 的浏览器授权和令牌刷新需要构建者在
构建进程环境中提供 `CONST_LOCAL_ANTIGRAVITY_CLIENT_ID` 和
`CONST_LOCAL_ANTIGRAVITY_CLIENT_SECRET`，修改后重新启动构建。
这些值会编入生成的程序，不是机密服务端应用 Secret 的安全存储方案；不要提交到仓库、
放进公开 CI，或分发你无权共享的凭据。

未配置时，这两条授权路径会直接返回明确的本地错误。仅导入 Access Token 不保证后续能刷新。
其他 API 渠道不需要这些值。仍须有兼容的 OAuth 应用注册与上游权益，不能保证任意自行创建的
Google OAuth 项目都能访问 Antigravity。

## 完成首次本机请求

1. 添加 API、受支持的订阅或本地模型渠道。
2. 刷新模型资料，选择模型，按需执行模型测试。测试可能产生上游费用。
3. 启用渠道，打开**使用**页，等待本地 API 启动后配置工具，或复制界面显示的地址和本地 Key。
4. 使用本地模型列表中的真实 ID。工具连接本地网关时，不应填写上游服务商的 Key。

默认数据目录是 `~/.const-api-local`，API 地址是 `127.0.0.1:38789`，
应用 ID 是 `xin.const.api.local`。官方版使用不同的目录、应用 ID、可执行文件名和默认端口。

不要同时用两个版本管理同一款外部工具的配置。切换版本前，先在当前版本取消托管配置，
再用另一版本重新配置；重要工具设置和会话数据应自行备份。

## 验证命令

在 `client/` 下执行：

```sh
npm run typecheck
npm run i18n:check
npm run test:renderer
cargo check --manifest-path src-tauri/Cargo.toml --all-targets --locked
cargo test --manifest-path src-tauri/Cargo.toml local_policy::tests -- --test-threads=1
```

隔离测试覆盖配置加载、手动注入端点和 Token 后仍拒绝平台传输、四种协议的本机转发，
以及缺少 OAuth 应用配置时的明确报错。
测试使用本地 mock，不调用付费模型。公开 CI 对 Windows、Linux、macOS 执行工作流中列出的检查。

保留的部分 Rust 测试描述的是平台版契约，因此公开 CI 选择本机版隔离测试，
不把它表述为私有版本全量测试通过。

## 平台隔离

- 不包含平台账号、凭据存储、安装证明和奖励的实现；对应公开接口返回不可用。
- 官方端点发现、更新源和平台凭据被禁用；复制配置不能开启。
- 此快照移除了平台 HTTP/HTTP3 及供应 WS/QUIC 建连实现，不影响上游 API、SSE、
  WebSocket 和局域网使用。
- 这是产品功能边界，不是防篡改保证。修改后的程序可以发起其他网络请求，上游 URL
  也仍由用户配置。持有有效服务凭据的人，可能独立使用普通 API；平台授权由服务器执行。

## 源码和发布策略

源码由私有主仓库单向同步。`public-source-manifest.json` 记录源提交和导出文件哈希，
不携带私有 Git 历史。该文件是来源记录，不是运行时授权凭据，也不是带签名的发布证明。

官方安装包仍通过原来的私有构建和发布流程产生。公开 CI 只有仓库读取权限，
不发布应用或更新源。通过审查的社区贡献先合入主仓库，再进入下一次公开快照。

许可范围见 [LICENSE](LICENSE) 和[第三方声明](THIRD_PARTY_NOTICES.md)。
