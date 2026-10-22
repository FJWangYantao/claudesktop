# 设计说明：发现并接入本机 agent

> 状态：**部分实施**（2026-10-21）。第 1 步（`agents.rs`）、第 2 步（多档案 + 迁移 + 内核槽）、
> 第 3 步（设置页「本机 agent」+ 首页引导 + `last_model` 记忆）已实施；
> 第 4 步（模型列表经 ACP `config_options`）、第 5 步（登录）、第 6 步（Key 导入）未做。
> 代码落点：`crates/core/src/agents.rs`（✅ 发现与预设）、`apps/desktop/src/settings.rs`（✅ 多 agent 档案与迁移）、
> `app/mod.rs`（✅ 内核槽按档案 key 开）、`app/settings_view.rs`（✅「本机 agent」一栏）、
> `crates/core/src/acp.rs`（登录、模型列表与切换，未动）、`app/composer.rs`（模型下拉认 ACP，未动）、`app/chat.rs`（登录卡片，未动）。

## 一、要解决什么

用户机器上往往已经装好并登录了别的 coding agent（Claude Code、OpenCode、pi……），订阅额度和模型都在那边。
现在想在 claudesktop 里用它们，得自己知道 ACP 适配器的包名、手填一条启动命令（`AcpSettings::command`，只能填一条），
没登录时只看到一句 `SessionFailed`，模型也不能在输入框旁切换（`model_menu_items` 对 ACP 只显示一句提示）。

目标：

1. 打开设置就能看到「本机装了哪些 agent」，点一下启用，不用填命令；
2. 可以同时启用好几个，新建会话时选用哪个；
3. 没登录时界面给出登录入口，登录本身由 agent 完成；
4. agent 报上来的模型能在输入框旁的模型下拉里看到、切换。

不做：

- **不读、不搬运别的程序的登录凭据**（令牌文件、钥匙串）。理由见第二节。
- 不替用户安装 agent（只在没装时给一条安装命令和文档链接）。
- 不做 ACP 的 `terminal/*` 能力（agent 借客户端开终端跑命令），与本方案无关。

## 二、为什么走 ACP，而不是借凭据直接调模型

两条路技术上都通：

| | A：把本机 agent 当 ACP 内核拉起来 | B：读出它们的 OAuth 令牌，由自研内核直接调模型接口 |
|---|---|---|
| 谁持有凭据 | agent 自己，claudesktop 碰不到 | claudesktop 读出来另用 |
| 条款 | 各家正式提供的接入方式（`opencode acp`、Zed 用的 Claude 适配器） | Anthropic 2026-02 起写明订阅 OAuth 令牌只能用在 Claude Code / Claude.ai，用在别的产品（含 Agent SDK）算违约；Google 2026-02 因第三方工具借用 Antigravity OAuth 批量封号 |
| 后果 | 无 | 用户的账号被封，损失在用户身上 |
| 工具 / 权限 / 记忆 | 走 agent 自己的那套 | 走自研内核那套 |

选 A。B 的收益（用自研内核的工具和记忆配订阅额度）抵不上封号风险，也违背「用户授权」的本意：用户点「启用」
是同意我们**启动**那个程序，不是同意我们拿走它的钥匙。

例外：用户自己的 **API Key**（环境变量 `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`、本机 Ollama）本来就是给程序用的，
可以在用户确认后导入自研内核的档案，见第九节。

## 三、要接的几家

| 预设 ID | 名称 | 怎么找 | 启动命令 | 登录 | 备注 |
|---|---|---|---|---|---|
| `claude` | Claude Code | `claude` 在 PATH | `npx -y @agentclientprotocol/claude-agent-acp`（Windows 外面套 `cmd /c`，同设置页现有的 `ACP_PRESETS`） | 适配器报 `authMethods`；或终端里跑 `claude /login` | 适配器要 Node；它跑的是 Claude Code 本体，用的是用户自己的登录；**带风险提示**（第十一节） |
| `opencode` | OpenCode | `opencode` 在 PATH | `opencode acp` | `opencode auth login` | 原生 ACP；它配好的各家模型都经 `configOptions` 报上来 |
| `pi` | pi | `pi` 在 PATH，版本 ≥ 0.80.4 | `npx -y pi-acp`（社区适配器，内部跑 `pi --mode rpc`） | pi 自己 | 适配器是社区项目，MCP 还没接通；见第十一节 |
| `gemini` | Gemini CLI | `gemini` 在 PATH | Gemini CLI 的 ACP 模式（参数名实施时核实） | 适配器报 `authMethods` | 替代 Antigravity |

**Antigravity 接不了**：它是 IDE，没有命令行 / ACP 模式。想用 Gemini 的走 Gemini CLI。

预设写成数据（`agents.rs` 里一张表），加一家 = 表里加一行。字段：

```rust
// crates/core/src/agents.rs
pub struct AgentPreset {
    pub id: &'static str,
    pub name: &'static str,
    /// 在 PATH 里找的可执行文件名（Windows 自动补 .cmd / .exe / .ps1）
    pub probe: &'static str,
    /// 取版本号的参数，一般是 `--version`
    pub version_args: &'static [&'static str],
    /// 最低版本；低于它显示「版本过旧」
    pub min_version: Option<&'static str>,
    /// ACP 启动命令（shell-words，同 `AcpKernelConfig::command`）；Windows 另有一条
    pub command: &'static str,
    pub command_windows: Option<&'static str>,
    /// 需要 Node（命令以 npx 开头）：没装 Node 时提示先装
    pub needs_node: bool,
    /// 没装时给的安装命令 / 文档链接
    pub install_hint: &'static str,
    /// 登录命令（agent 没报 `authMethods` 时的退路），如 `["claude", "/login"]`
    pub login: Option<&'static [&'static str]>,
    /// 经用量网关时要改的环境变量（不认网关的 agent 留空，界面不给开）
    pub gateway_env: GatewayEnv,
    /// 条款上有风险时的一句提示：检测列表、启用确认框里都显示（第十一节）
    pub risk: Option<&'static str>,
}
```

## 四、发现

`agents::discover() -> Vec<Detected>`，在后台线程跑（要起几个子进程），结果缓存在 `ChatApp`，设置页打开、点「重新检测」时刷新。

```rust
pub struct Detected {
    pub preset: &'static AgentPreset,
    /// 找到的可执行文件
    pub path: PathBuf,
    /// `--version` 的输出里抠出来的版本号；取不到 = None（不当成没装）
    pub version: Option<String>,
    pub state: DetectState, // Ready / TooOld { need } / NeedsNode
}
```

- 找的位置：`PATH` 各项，外加常见的全局安装目录（Windows `%APPDATA%\npm`、`%LOCALAPPDATA%\Programs`；
  其余 `~/.local/bin`、`~/.npm-global/bin`、`~/.bun/bin`）。图形界面启动时 PATH 往往比终端里短，所以这几处要补。
- `--version` 带 3 秒超时，跑的时候带 `tools::QUIET_ENV` 那套不弹窗口的环境变量（Windows 加 `CREATE_NO_WINDOW`）。
- **只看装没装，不打开它们的配置目录和凭据文件**。登没登录要等真连上了由 agent 自己说（第六节）。
- 可测：`discover_in(paths: &[PathBuf], run: impl Fn(&Path, &[&str]) -> Option<String>)`，单测用假目录和假版本输出。

## 五、启用（用户授权）

设置页「内核与模型」新增一栏 **本机 agent**（`section` + 每家一行 `setting_row`）：

```
本机 agent                                       [重新检测]
  Claude Code   2.3.1                             [启用]
  OpenCode      1.4.0   · 已启用                   [停用]
  pi            0.79.0  · 版本过旧，需要 0.80.4 以上   [如何升级]
  Gemini CLI    未安装                             [如何安装]
  自定义命令…                                       [添加]
```

- 点「启用」之前，任何 agent 进程都不会启动；启用 = 写进设置的 `acp.agents`。
- 「自定义命令」就是现在的那个启动命令输入框，挪到这里成为一条普通档案（老用户的设置迁移过来就落在这）。
- 首页配置引导（还什么都没配时）也列出检测到的 agent，一键启用，同 `choice_panel` 的写法。
- 第一次启用某家时弹一个确认框，一句话说明：「claudesktop 会在后台启动 OpenCode，用它自己的登录和模型；我们不会读取它的登录信息。」
  预设带 `risk` 的，确认框里再加那句风险提示，列表里那一行下面也常驻显示（`TEXT_META` + `INK_SOFT`）。

## 六、登录

ACP 1.x 的 `initialize` 响应带 `authMethods`，分两种（`agent-client-protocol-schema` 的 `AuthMethod`）：

- **`Agent`**：客户端调 `authenticate(methodId)`，agent 自己去开浏览器 / 走设备码。
- **`Terminal`**：客户端用同一条启动命令再加上 `args` / `env` 开一个**交互式终端**，用户在里面登录，退出码 0 = 成功。
  只有客户端在 `initialize` 里声明了终端登录能力，agent 才会报这种。

流程：

1. `acp.rs` 握手后把 `authMethods` 记下来，经 `KernelEvent::Connected` 一并报给界面（新字段 `auth_methods: Vec<AuthMethodView>`）。
2. `session/new` 失败且错误码是 `auth_required` 时，发新的 `KernelEvent::AuthRequired { token, methods }`，而不是 `SessionFailed`。
3. 会话里出一张登录卡片（`Session::note` 那一层，不进日志）：标题「OpenCode 需要登录」，每种方法一个按钮。
   - `Agent` 方法 → `Kernel::authenticate(method_id)`（新增 trait 方法，默认不支持），按钮 `.loading()` 等结果；
   - `Terminal` 方法 → 开一个系统终端窗口跑那条命令（Windows `cmd /c start "" <命令>`，macOS `open -a Terminal`，
     Linux 依次试 `x-terminal-emulator` / `gnome-terminal` / `konsole`），等进程退出；
   - agent 什么都没报、但预设里有 `login` → 同 `Terminal` 的方式跑预设的登录命令。
4. 成功后自动重试刚才失败的 `session/new`（`Session::queued` 里的消息照常发出）；失败就留着卡片和错误原文。

登录全程 claudesktop 只负责「开窗口 / 发请求 / 等结果」，令牌落在 agent 自己的存储里。

## 七、模型列表与切换

现状：`acp.rs` 的 `current_model` 只从 `session/new` 的 `config_options` 里取当前值；`Kernel::set_model` 对 ACP 没实现；
模型下拉 `model_menu_items` 对非自研内核直接显示「当前内核的模型由 agent 自己决定」。

改法：

- `acp.rs`：把 `config_options` 里 `category == "model"`（或 `id == "model"`）那一项的**全部可选值**解析出来，
  `KernelEvent::Started` 增加 `models: Vec<ModelChoice>`（`id` + 显示名 + 说明），`current_model` 不变。
- agent 中途报 `SessionUpdate::ConfigOptionUpdate`（模型列表变了、当前值变了）→ 新的 `KernelEvent::ModelsChanged { models, current }`。
- `AcpKernel::set_model` → 发 `SetSessionConfigOptionRequest { config_id: <那一项的 id>, value }`，响应里的新 `config_options` 再走一遍上面的解析。
- 界面：`Session` 加 `agent_models: Vec<ModelChoice>`；`model_menu_items` 在 ACP 会话下列这份（不读 `settings.builtin.models()`），
  选中走现有的 `choose_model`，但**不写回** `builtin.current_mut().model`（那是自研内核的默认值）。
  agent 没报模型项时保留现在那句提示。
- 选过的模型按档案记住（`AgentProfile::last_model`），新会话 `Started` 后若 agent 当前值不同就切过去，同自研内核的 `pending_model`。

## 八、数据模型与迁移

```rust
// apps/desktop/src/settings.rs
pub struct AcpSettings {
    /// 启用了的 agent，按添加顺序；第一项是新会话的默认
    pub agents: Vec<AgentProfile>,
    // 老字段：只读不写，迁移用
    #[serde(default, skip_serializing)]
    command: String,
    #[serde(default, skip_serializing)]
    gateway: bool,
}

pub struct AgentProfile {
    /// 预设 ID（`claude` / `opencode` / …）或 `custom-<n>`
    pub id: String,
    /// 显示名；预设的取预设名
    pub name: String,
    /// 只有自定义档案有；预设档案每次按预设现算（预设改了命令跟着变）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    pub gateway: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_model: Option<String>,
}
```

- 迁移：读到老的非空 `command` 时，生成一条 `custom-1` 档案，名字取「自定义 agent」，`gateway` 带过去。
- 内核槽：现在 `kernels: [KernelSlot; 3]` 按 `KernelChoice::index` 下标。改成「自研 / 演示」两个固定槽 + 每个 ACP 档案一个槽
  （`Vec<(String, KernelSlot)>`，按档案 ID 找），`KernelChoice::Acp` 带上档案 ID：`Acp(String)`。
  每个档案一个进程，**懒启动**：用到它的会话第一次连接时才起。
- 会话归属：日志 / `SessionMeta` 里的 agent 标识现在是 agent 自报名（`of_agent` 只分 builtin / 空 / 其他），
  改成 `acp:<档案 ID>:<自报名>`。老会话没有档案 ID → 归到第一个启用的档案（只有一个时就是迁移来的那条），续接时照旧。
- 设置里的 `kernel: Option<KernelChoice>` 改为默认内核（`builtin` / `acp:<id>`），解析认老值 `acp`（= 第一个档案）。

## 九、导入 API Key（可选，同一栏下面）

`agents::detect_keys()` 只看**环境变量**和**本机端口**，不读文件：

- `ANTHROPIC_API_KEY`、`OPENAI_API_KEY`、`OPENROUTER_API_KEY`、`GEMINI_API_KEY`；
- `127.0.0.1:11434` 能连上 → Ollama。

检测到就在「自研内核」档案区显示一行「检测到 OPENAI_API_KEY，导入为档案？」，用户点了才建档案（Key 进设置，同手填）。

现有的「从 Antigravity Tools 导入端点和 Key」（`settings::antigravity_gateway`）走的是第三方反代 Antigravity 的 OAuth，
正是第二节说的 B 路线。**定了：保留，按钮下面常驻一句风险提示**（已实施，`settings_view.rs` 的 `ANTIGRAVITY_RISK`）。

## 十、界面改动一览

- **设置页「内核与模型」**：上面「本机 agent」一栏（第五节），下面是现有的自研内核档案；原来的「ACP 启动命令」输入框并进「自定义命令」。
- **新建会话 / 目录 chip 旁**：内核 chip 变成可选具体 agent（`menu_popover` + `menu_item`：图标 + 名称 + 「已连接 / 未启动 / 需要登录」）。
- **侧栏 / 顶栏面包屑**：显示档案名（「OpenCode」），不再统称「ACP agent」；会话行指纹（`sync_sidebar`）并进档案 ID。
- **登录卡片**（第六节），**模型下拉**认 ACP（第七节）。
- **命令面板**：加「检测本机 agent」一条（`palette_groups`）。

文案全中文；agent 自报的名称和模型名照原样显示。

## 十一、风险与待定

**原则（2026-10-21 定）：有条款风险的入口一律保留，旁边加一句风险提示，由用户自己决定用不用。**

1. **Claude Code 适配器的条款**：适配器跑的是 Claude Code 本体（Zed 就是这么接的），但底层是 Agent SDK，Anthropic 的条款
   不允许把订阅登录经第三方程序使用。保留，预设带 `risk` 提示；用 API Key 登录的不受影响。现有设置页的「填入 Claude Code」
   下面已经加了这句（`CLAUDE_CODE_RISK`）。对外发版前再核对一次当时的条款，文案跟着改。
2. **Antigravity Tools 导入**（第九节）：保留，加风险提示（已实施）。
3. **pi 适配器成熟度**：社区的 `pi-acp` 还是 MVP（MCP 没接通，包名有过变动）。先用它；不够用再考虑自己写一个
   （参考 Rust 版 `pi-acpinator`，或在 `crates/` 里直接对 `pi --mode rpc` 写一层）。
4. **Windows**：npm 装的是 `.cmd` 垫片，起进程要过 `cmd /c`；图形界面启动拿不到用户 shell 里改过的 PATH（第四节已补常见目录）；
   开终端登录要实机试。
5. **多进程开销**：每个启用的档案一个常驻进程（Node 适配器每个几十 MB）。懒启动 + 档案停用时关掉；空闲很久的进程是否回收，实施时看实际占用再定。
6. **经用量网关**：只有认 `ANTHROPIC_BASE_URL` / `OPENAI_BASE_URL` 的 agent 能经网关（`gateway::agent_env`），按预设的 `gateway_env` 决定给不给开；
   订阅登录走的是 agent 自己的端点，网关多半不生效，开关旁要说明。

## 十二、实施步骤

| 步 | 内容 | 验证 |
|---|---|---|
| 1 | `agents.rs`：预设表 + `discover_in` / `discover` | ✅ 单测：假 PATH、`.cmd` 垫片、版本过旧、缺 Node、版本串解析；实机跑一遍 `cargo test -p claudesktop-core --test discover_agents -- --ignored` |
| 2 | 设置：`AgentProfile` + 迁移；内核槽按档案 key 开；`KernelChoice::Acp(id)`；会话归属 | ✅ 实现；单测靠 CI |
| 3 | 设置页「本机 agent」一栏 + 首页引导 + 选过的模型按档案记（`last_model`，`Started` 后 agent 没报就切过去） | ✅ 实现；实机待验（检测、启用、新建会话连上） |
| 4 | 模型列表与切换（第七节） | 用 `tools/` 里的 mock ACP agent 加 `configOptions`，测 `Started.models`、`set_model`、`ConfigOptionUpdate` |
| 5 | 登录（第六节） | mock agent 先报 `auth_required` 再接受 `authenticate`；终端方式实机试 |
| 6 | API Key 导入（第九节） | 单测：环境变量检测 |

先做 1 → 2 → 3 → 4：已经登录过的用户一键就能用、能切模型；登录流程第 5 步再补。每步单独提交。

## 十三、参考

- OpenCode ACP：<https://opencode.ai/docs/acp>
- pi 的 ACP 适配器：<https://github.com/DxTa/pi-acp>、<https://docs.rs/pi-acpinator>
- Anthropic 第三方订阅认证：<https://alternativeto.net/news/2026/2/anthropic-officially-bans-using-subscription-authentication-for-third-party-claude-use>
- Google 封停借用 Antigravity OAuth 的账号：<https://venturebeat.com/ai/google-clamps-down-on-antigravity-malicious-usage-cutting-off-openclaw-users>
- ACP 登录方式与配置项：`agent-client-protocol-schema` 1.11 的 `AuthMethod`、`SetSessionConfigOptionRequest`、`ConfigOptionUpdate`
