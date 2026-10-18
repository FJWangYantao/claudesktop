# 设计说明：MCP 远程服务的 OAuth 登录

> 状态：设计（2026-10-18），待实现。
> 代码落点：`crates/builtin/src/mcp_oauth.rs`（新）、`mcp.rs` / `mcp_http.rs`、`crates/core/src/lib.rs`（`McpState`）、
> `apps/desktop/src/app/mcp_view.rs`。

## 一、要解决什么

自研内核现在能连远程 MCP（Streamable HTTP / 旧版 SSE），但**认证只有一条路：在 `mcp.json` 的 `headers` 里手填**。
回 401 时界面给出的提示就是这句（`mcp_http.rs` 的 `status_reason`）：

> HTTP 401：服务器要求授权，在 mcp.json 这个服务的 `headers` 里配上凭据（如 Authorization）。

托管型的 MCP 服务基本都不给长期 token，而是走 **OAuth 2.1 + 动态客户端注册**（官方 registry 上的多数服务、
各家 SaaS 的 MCP 端点都是）。这类服务现在**根本连不上**：没有地方能用"登录一次"换一个 access token，
手填的 token 几小时就过期。

目标：远程服务回 401 时，设置页出现「登录」，点一下在浏览器里授权，回来后自动连上；token 过期自动刷新；
刷新不了再提示重新登录。用户不需要知道 OAuth 的任何细节。

不做：

- **stdio 服务**：MCP 规范明确说 stdio 不该走 OAuth（凭据从环境变量读），现在这条路是对的；
- 多账号 / 账号切换（一个服务一份 token）；
- token 跨机器同步、导入导出；
- 服务器元数据里的 `scopes` 精挑细选（拿到什么用什么，够用就行）。

## 二、用户看到什么

1. **设置 → MCP**，需要登录的服务：

   ```
   ● linear         需要登录   [登录]
     https://mcp.linear.app/mcp
   ```

   点「登录」在默认浏览器里打开授权页。这一行下面补一句灰色说明：「在浏览器里完成授权，回来后自动连上」。

2. **回调页**：浏览器显示一个极简白页：「授权完成，可以关掉这个标签页了。」（我们自己写的 HTML，
   不依赖本应用窗口在前台 —— 用户授权时可能切去别的程序）。失败时同样是一个白页，写着原因。

3. **回来之后**：状态「连接中…」→「N 个工具」，和原本就连上的一模一样。

4. **失败**：状态「没连上」+ 一句原因。要分清这几种，因为对策不同：
   - `你在浏览器里拒绝了授权`
   - `等授权回调超时了，再点一次「登录」`
   - `授权服务器不支持自动注册（DCR），在 mcp.json 里给它填 "oauth": { "client_id": … }`
   - `令牌被服务器拒绝了（scope 不够？）`

5. **token 过期**：静默刷新，用户看不到任何东西。刷新也失败（refresh token 被撤销）就回到「需要登录」。

6. **隐私**：token 只落在数据目录，界面不显示、不进日志、不进会话记录、不进用量网关。

## 三、规范里必须走的几步

参考 MCP 2025-06-18 的 Authorization 章节（OAuth 2.1 + RFC 9728 + RFC 8414 + RFC 7591 + RFC 8707 + PKCE）。

| 步 | 做什么 | 失败时 |
|---|---|---|
| 1 | 请求回 `401` 且 `WWW-Authenticate: Bearer resource_metadata="…"` → 取 PRM（RFC 9728） | 没有这个头：按现在的办法走（提示配 headers） |
| 2 | PRM 里 `authorization_servers[0]` → AS metadata（RFC 8414，`/.well-known/oauth-authorization-server`，issuer 带路径时插在中间，再退 `openid-configuration`） | 报「这个服务器没说授权服务器在哪，只能手填 headers」 |
| 3 | 动态客户端注册（RFC 7591）拿 `client_id`（有些 AS 还给 `client_secret`） | 退到 mcp.json 里手填的 `client_id`；也没有就报错 |
| 4 | 生成 PKCE（S256）+ `state`，拼授权 URL，开浏览器 | — |
| 5 | 回调拿到 `code`，换 access / refresh token（带 `resource` 参数，RFC 8707） | 报原因，可重来 |
| 6 | 之后的每个请求带 `Authorization: Bearer …`；401 时用 refresh token 换一次、重试一次 | 换不到就回「需要登录」 |

**每一步失败都要能退回到「手填 headers」**，这是这个功能的安全阀：OAuth 是加分项，不能让原来能用的配置变坏。

## 四、怎么落到代码

### 4.1 新模块 `crates/builtin/src/mcp_oauth.rs`

纯逻辑尽量单独成函数，好单测；网络和线程只做薄薄一层。

```rust
/// 从 401 的 WWW-Authenticate 里取资源元数据的地址（可能有多条 challenge，只认带 resource_metadata 的 Bearer）
pub fn resource_metadata_url(challenge: &str, server: &str) -> Option<String>;

/// RFC 8414 的元数据地址：/.well-known/oauth-authorization-server + issuer 的路径
pub fn metadata_urls(issuer: &str) -> Vec<String>;

/// PKCE：verifier 与 S256 challenge（RFC 7636 附录 B 的测试向量要对得上）
pub fn pkce(verifier: &str) -> String;
pub fn new_verifier() -> String;

/// 授权 URL（参数按 RFC 6749 排序不重要，但都要 urlencode）
pub struct AuthRequest { pub url: String, pub state: String, pub verifier: String }
pub fn authorize_url(meta: &AsMetadata, client_id: &str, redirect: &str, resource: &str,
                     scope: Option<&str>, req: &AuthRequest) -> String;

/// 在 127.0.0.1 上等一次回调：读请求行 → 校验 state → 回一段 HTML → 交出 code 或 error
pub fn wait_for_code(port: u16, expect_state: &str, timeout: Duration) -> Result<String, String>;
```

`wait_for_code` 用 `std::net::TcpListener` + 手写最小 HTTP（照 `gateway.rs` 的做法，不引 HTTP 框架）：
读第一行请求行取 `?code=&state=`，回一段 `200 text/html` 的固定页面，然后关连接。

新增依赖 `getrandom = "0.3"`（lock 文件里已经有了）做 `state` / verifier 的随机源；
`sha2` 在 core 里有、builtin 要用就加一份（或把 `pkce` 挪进 core 的 `checkpoint` 旁边，看哪边更顺）；
base64url 用已有的 `base64`。

### 4.2 配置：`mcp.json` 里多一个可选的 `oauth`

```jsonc
"linear": {
  "type": "http",
  "url": "https://mcp.linear.app/mcp",
  "oauth": { "client_id": "…", "callback_port": 54545, "scope": "read" }
}
```

- 整个 `oauth` 可以省略（走自动发现 + DCR）。
- `client_id`：AS 不支持 DCR 时手填。
- `callback_port`：AS 要求重定向地址精确匹配、又不认随机端口时固定下来。
- `scope`：不写就用 PRM 里 `scopes_supported` 的第一个 / AS 的默认。
- **写了 `headers.Authorization` 就不走 OAuth**：显式配置永远优先，这条规则让老配置零风险。

### 4.3 `Http` 的鉴权从"建连接时算好"改成"每次发送时取"

现在 `Http.headers` 是 `open()` 时拷进来的一坨。OAuth 要能刷新，所以：

```rust
enum Auth {
    None,
    /// 固定的（用户手填的 headers 里有 Authorization），原样发
    Static,
    /// 动态：发送时取当前 access token，401 时 refresh 一次并重试
    Bearer(Arc<OAuth>),
}
```

`send()` 里先 `auth.authorize(&mut request)`；请求回来是 401 且 `Auth::Bearer` 允许刷新时，
在同一线程里 `refresh()` → 重发一次；再 401 就 `sink.closed("令牌被拒绝")`，状态回到「需要登录」（重新起授权流程）。

刷新后的 token 立刻原子写回文件（先写 `.tmp` 再改名），**然后**才重发，避免"刷了但没存"。

### 4.4 新状态 `McpState::NeedsAuth`

```rust
// crates/core/src/lib.rs
pub enum McpState {
    Connecting,
    Ready,
    Failed(String),
    Reconnecting(String),
    Disabled,
    /// 服务器要求 OAuth 授权：授权页的地址（界面上的「登录」就开它）。
    /// 流程有效期 10 分钟，过期后再点「重新连接」会重新起一轮、推新地址
    NeedsAuth { url: String },
}
```

- 序列化照现有写法：`#[serde(tag = "state", content = "detail")]` → `{"state":"needs_auth","detail":{"url":"…"}}`，
  老日志 / 老界面读到不认识的 variant 时是整体反序列化失败，所以**这个枚举不落盘**（`McpStatus` 不进 `Record`，
  见 `Record::worth_keeping`），没有兼容负担。
- `McpServerView.retry` 对 `NeedsAuth` 也为真（「重新连接」能重来一轮）。
- UI 只需在 `mcp_view.rs` 的 match 里加一支：`(CORAL, "需要登录", None)` + 一个「登录」按钮
  （`cx.open_url(&url)`）。`cx.open_url` 在 `App` 上，`cx.listener` 闭包里拿得到。

### 4.5 不加新的内核接口

「登录」= 打开 state 里那个 URL，不需要内核配合；要重来一轮就复用已有的 `Kernel::reconnect_mcp(name)`
（`reconnect` 现在只处理 `Failed` / `Reconnecting`，加一条 `NeedsAuth` 就够）。

授权流程本身在 hub 的后台线程里跑（`establish` 之前）：拿到 401 → 发现元数据 → DCR → 起回调监听 →
`state = NeedsAuth { url }` 并上报 → 阻塞等 code（最多 10 分钟，期间 `generation` 变了就放弃）→ 换 token → 继续握手。

### 4.6 token 存在哪

`<data>/mcp-auth/<server 地址的 sha256 前 16 位>.json`：

```json
{ "server": "https://mcp.linear.app/mcp", "issuer": "https://auth.linear.app",
  "client_id": "…", "client_secret": null, "scope": "read",
  "access_token": "…", "refresh_token": "…", "expires_at": 1793000000000 }
```

- 按**服务器地址**存，不按服务名：改了 `mcp.json` 里的名字不丢授权，同一个地址配两个名字也共用。
- 不放进 `mcp.json`：那份文件用户会贴给别人、会进 git 模板。
- 删除服务 / 卸载插件时一并删（`McpHub::reload` 里清掉不再出现的地址？不 —— 停用一下不该丢登录，
  只在设置页给一条「清除登录状态」，和服务删除时清）。
- Windows 上没有方便的 600 权限，就是用户目录下的普通文件；文档里写清楚，别指望它是保险箱。

## 五、时序

```
hub 后台线程                    授权服务器 (AS)                 浏览器
    │ POST /mcp ────────────────▶│                              │
    │◀──── 401 + resource_metadata│                             │
    │ GET  PRM ──────────────────▶│                              │
    │ GET  AS metadata ──────────▶│                              │
    │ POST /register (DCR) ──────▶│                              │
    │◀──── client_id ─────────────│                              │
    │ 起 127.0.0.1:<随机端口> 监听   │                              │
    │ state=NeedsAuth{url} 上报 → 界面出现「登录」→ 用户点 → cx.open_url
    │                             │◀──── GET /authorize ─────────│
    │                             │──── 302 → 127.0.0.1/callback ▶│
    │◀──── GET /callback?code&state ─────────────────────────────│
    │ 回 HTML「授权完成」                                            │
    │ POST /token (code + verifier) ─▶│                          │
    │◀──── access + refresh ─────────│                          │
    │ 写 mcp-auth/<hash>.json，state=Connecting，继续握手            │
```

## 六、测试

**单测**（`mcp_oauth.rs` 内，不需要网络）：

- `resource_metadata_url`：单个 / 多个 challenge、没有 metadata、Bearer 之外的类型、大小写；
- `metadata_urls`：issuer 带路径时的插入规则、`openid-configuration` 兜底；
- `pkce`：RFC 7636 附录 B 的固定向量；
- 授权 URL 的查询参数（含 `resource`、`code_challenge_method=S256`、特殊字符的 urlencode）；
- token 文件：读写往返、过期判定（`expires_at` 为空表示不过期）、损坏文件当没有。

**集成**（`crates/builtin/tests/mcp.rs`，扩 `tools/mock-mcp-server.mjs`）：

给 mock 加 `--http --oauth`：没带 Bearer 就 401 + `WWW-Authenticate`；实现 PRM / AS metadata / `/register` /
`/authorize`（**自动批准**，直接 302 回 `redirect_uri`，省掉真浏览器）/ `/token`（发短命 access + refresh）；
带过期 access 回 401，带 refresh 的交换能拿到新的。测试里用普通 HTTP 客户端扮演浏览器：GET 授权 URL，
跟 302 到 `127.0.0.1:<端口>/callback`，回环监听由 hub 收。断言：

1. 一开始状态走到 `NeedsAuth`，URL 里有 `code_challenge` 和 `resource`；
2. 跟一次跳转后变成 `Ready` 且有工具；
3. token 文件写出来了；
4. 让 access 强制过期 → 一次工具调用内部完成刷新，用户无感，token 文件的 `expires_at` 变了；
5. 把 refresh token 也作废 → 状态回到 `NeedsAuth`。

**真实服务**（`#[ignore]`）：`CLAUDESKTOP_MCP_URL` 指向一个要登录的公开端点时，打印授权 URL 让人手动点一次；
不做断言（浏览器交互没法自动化）。

## 七、风险与对策

| 风险 | 对策 |
|---|---|
| AS 不支持 DCR | 退到手填 `client_id`；提示写清楚，不反复弹浏览器 |
| AS 不认随机端口的 redirect | `"oauth": { "callback_port": … }` 固定下来；文档里写这句 |
| 用户永远不点「登录」 | 流程 10 分钟自己结束，状态停在「需要登录」，不占线程、不占端口 |
| 回调监听被占 / 防火墙 | 绑 `127.0.0.1` 随机端口，Windows 一般不弹窗；绑不上就报原因（可改 `callback_port`） |
| token 文件泄露 | 只在用户目录；界面不显示、日志不打；设置页能一键清除 |
| 老配置变坏 | 有 `headers.Authorization` 就完全不碰 OAuth；发不出 401 的服务行为不变 |
| 授权中途改了配置 / 重新加载 | 回调线程拿到 `generation`，变了直接放弃，不写状态、不写 token |
| 刷新并发（两个请求同时 401） | `OAuth` 里一把 `Mutex`，拿到锁后先看 token 是否已经换过，避免重复交换 |
| 401 重试放大 | 每个请求**最多重试一次**，再失败就断开连接走重连，不做无限循环 |

## 八、分期

| 期 | 内容 | 主要改动 | 预计 |
|---|---|---|---|
| 1 | 发现 + DCR + PKCE + 回调 + 换 token + 存 token；设置页「登录」；mock 服务器的 happy path | `mcp_oauth.rs`、`mcp.rs` 的 `establish` 前段、`McpState::NeedsAuth`、`mcp_view.rs` | ~1.5 天 |
| 2 | 刷新与 401 重试、过期后回到「需要登录」、并发刷新去重、「清除登录状态」 | `mcp_http.rs` 的 `Auth`、设置页按钮 | ~0.5 天 |
| 3 | 手填 `client_id` / 固定 `callback_port` / `scope` 这几个配置项与错误文案 | `mcp.rs` 解析、文档 | ~0.3 天 |

第 1 期做完就能连上绝大多数托管服务（它们都支持 DCR）；第 2 期决定它能不能长期用下去。

## 九、需要你拍板

1. **点「登录」之外，要不要自动弹浏览器？**
   推荐**不要**：401 可能发生在启动时一口气连八个服务器的时候，挨个弹窗比不连还烦。
2. **固定回调端口还是随机？**
   推荐**默认随机**（DCR 注册的就是那个地址），`oauth.callback_port` 兜底。固定端口的副作用是和别的程序抢。
3. **token 存单独文件还是塞进 `mcp.json`？**
   推荐**单独文件**（`mcp-auth/`）。塞进 `mcp.json` 会让会贴给别人、进 git 的那份文件带密钥。
4. **401 时要不要先试一次"无凭据也能连"？**
   推荐**不要**：现在就是这样（发出去、401、报错），多一轮没意义。
5. **顺手把 `mcp_http.rs` 里 401 的文案改掉吗？**
   推荐**改**：OAuth 可用时，「配 headers」就不再是首选答案了，应该先说「点登录」。
