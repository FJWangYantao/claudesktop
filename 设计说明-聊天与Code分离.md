# 设计说明：聊天与 Code 模式分离

> 状态：**方案（未实施）**，写于 2026-10-22。第十节的八件事要先拍板，文中按「推荐」写。
> 代码落点：`crates/core/src/store.rs`（`SessionKind`、`SessionMeta::kind`）、`crates/core/src/lib.rs`（`Kernel` 的 `SessionSpec`）、
> `crates/builtin/src/lib.rs`（聊天会话的起法、系统提示词、工具集）、`crates/builtin/src/tools.rs`（`chat_specs`）、
> `apps/desktop/src/session.rs`（`Session::kind`）、`settings.rs`（`chat` 一栏、`space`）、`app/mod.rs`（建会话按空间）、
> `app/sidebar.rs`（两个空间、按日期分组）、`app/overview.rs`（聊天首页）、`app/composer.rs`（工具栏按会话类型取舍）、
> `app/attach.rs`（新增：聊天附件内联）、`app/chrome.rs`（面包屑）、`app/rewind.rs`（聊天只回退对话）、
> `app/settings_view.rs`（「内核与模型」里的「聊天」一组）、`app/palette.rs`。

## 一、要解决什么

现在每个会话都是**写代码的 agent**，就算只是问一句「这段英文怎么翻」：

- **系统提示词**开头是「You are a coding agent … Working directory: …」，附 `BACKGROUND`（无头跑测试那段）、
  项目里的 AGENTS.md / CLAUDE.md、技能清单；**工具定义** 16 个内置工具 + `task` + 全部 MCP 工具，
  每次请求都带（只算 16 个内置的，`tools::specs` 的定义粗估就有 3～4k token）。
  模型被提示成「先 glob、read_file 再回答」，闲聊时也会去翻工作目录。
- **「任务」不是聊天**：没选文件夹的会话跑在默认目录（`acp_base.cwd`）里，工具、权限、检查点一样不少；
  记忆按 `memory::project_file_or(cwd)` 写进**默认目录**下的 `.claudesktop/memory.json`，那份文件谁都不认。
- **界面全是 Code 向的**：首页建议是「梳理项目结构 / 补单元测试」，输入框工具栏有目录、权限模式，
  回复末尾是改动卡片，回退浮层问「还原文件吗」，附件只是把路径贴进输入框（等着 agent 去 `read_file`）。

目标：分成两种会话。

| | 聊天 | Code |
|---|---|---|
| 用途 | 问答、写作、翻译、头脑风暴、看图 | 改代码、跑命令、在某个目录里干活 |
| 工作目录 | 没有 | 有（项目，或者「任务」用的默认目录） |
| 工具 | 只有联网（`web_search` / `web_fetch`，可关）和全局记忆 | 现在那一整套 |
| 权限 | 没有权限模式，没有权限卡 | 现在那套（逐项确认 / 自动 / …） |
| 内核 | 只有自研内核 | 自研内核或 ACP agent |
| 系统提示词 | 通用助手 + 用户写的「聊天说明」 + 全局记忆 | 现在那份 |

不做：一次会话中途切换类型（建好就定了，和内核一样）；ACP agent 跑聊天；聊天里的 MCP / 插件技能 / 钩子（见第十节第 4 条）；
聊天里的「项目」（Claude.ai Projects 那种：一组共享资料的聊天），以后再说。

## 二、参考：别家怎么分

| | 怎么分 | 入口 |
|---|---|---|
| Claude 桌面版 | Chat 和 Code 是两个**独立的区**，各有自己的会话列表和首页 | 窗口顶部切换 |
| ChatGPT / Codex | 聊天和写代码是两个产品（Codex 另有入口和任务列表） | 分开 |
| Cursor | 同一个侧边对话框里切 Ask（只读）/ Agent 模式 | 输入框里的模式下拉 |

Cursor 那种「同一个会话里切模式」我们已经有了（权限模式里的「计划模式」= 只读），它解决不了上面的问题：
提示词还是 coding agent、工具还是那套、首页还是 Code 向。所以走 Claude 桌面版的路子：**两个区**。

## 三、会话类型怎么定义

```rust
// crates/core/src/store.rs
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    /// 写代码：有工作目录、全套工具和权限（加这一项之前的会话都是它）
    #[default]
    Code,
    /// 纯聊天：没有工作目录，只有联网和全局记忆
    Chat,
}

pub struct SessionMeta {
    // ……原有字段
    /// 会话类型；老元信息没有这一项，读成 Code
    #[serde(default, skip_serializing_if = "SessionKind::is_code")]
    pub kind: SessionKind,
}
```

- **「任务」保留，算 Code**：它是「没挑项目的 Code 会话」（整理下载文件夹、跑个脚本），仍要工具。
  `task` 只对 Code 有意义，聊天会话恒为 `false`。
- **`cwd` 字段不改成 `Option`**：`SessionMeta::cwd` 是 `PathBuf`，改类型要动一串调用方，老版本也读不了。
  聊天会话的 `cwd` 填**数据目录下的 `chat/`**（建好、空着）：
  任何漏网的相对路径（比如以后谁加了个工具忘了判断类型）落进一个无害的空目录，而不是用户的启动目录。
  界面上一律不显示它。
- **类型建好就定**：和 `Session::kernel` 一样，空会话可以换，发过第一条消息就固定。

`apps/desktop/src/session.rs` 的 `Session` 加 `pub kind: SessionKind`，建档时写进元信息；
`Session::group_dir()` 对聊天不用（聊天不按目录分组）。

## 四、内核：怎么起一个聊天会话

### 4.1 接口

`Kernel::new_session` / `resume_session` 的 `cwd: Option<PathBuf>` 换成一个结构体：

```rust
// crates/core/src/lib.rs
/// 开会话时交给内核的东西（以后再加会话级选项只往这里添字段）
#[derive(Debug, Clone, Default)]
pub struct SessionSpec {
    /// None = 内核的默认目录（聊天会话也传 `<数据目录>/chat`，见 store.rs）
    pub cwd: Option<PathBuf>,
    pub kind: SessionKind,
}

pub trait Kernel {
    fn new_session(&self, token: u64, spec: SessionSpec) -> anyhow::Result<()>;
    fn resume_session(&self, token: u64, session_id: &str, spec: SessionSpec, history: &[Record])
        -> anyhow::Result<()>;
    /// 能不能开聊天会话（[`SessionKind::Chat`]）。ACP agent 都是写代码的，不能
    fn supports_chat(&self) -> bool { false }
}
```

- ACP 内核收到 `kind == Chat` 直接回 `SessionFailed`（「这个 agent 不支持纯聊天」）。界面本来就不会这么发，这是兜底。
- 调用方：桌面 3 处（`mod.rs` 的 `request_pending_sessions` 和续接），其余是 `tests/kernel.rs` 里的十几处，机械替换。
- 不另加 `new_chat_session`：续接、回退、压缩都要知道类型，两套方法会越分越多。

### 4.2 自研内核

`SessionState` 加 `kind: SessionKind`。`open_session` 对聊天会话：

| 步骤 | Code（现在） | 聊天 |
|---|---|---|
| 项目配置 `.claudesktop/settings.json`（模型、权限规则合并、停用的 MCP、插件范围） | 读 | **不读** |
| 说明文件（`~/.claude/CLAUDE.md` + 项目的 AGENTS.md / CLAUDE.md） | 读 | **不读**，换成设置里的「聊天说明」（`chat.instructions`） |
| 技能 | 按插件取 | 不取（技能要 `read_file` 读文件，聊天没有） |
| 模型 | 项目配置 → 全局默认 | 设置里的聊天模型（`chat.model`，空 = 全局默认） |
| 记忆 | 项目 + 全局 | **只有全局** |
| 模式 | `modes(..)` 一张表 | **空表**（`Started.modes` 为空，界面据此不画模式 chip） |

`run_steps` 选工具时按类型分：

```rust
let mut specs = match kind {
    SessionKind::Chat => tools::chat_specs(web),   // web_search、web_fetch（web 开着时）+ memory_*（只全局）
    SessionKind::Code => { /* 现在那段：specs + MCP + task */ }
};
```

- **工具执行再拦一道**：`run_tool` 开头，聊天会话收到不在 `chat_specs` 里的工具名（模型照着续接前的
  `[tool] …` 文字瞎编、或者别家模型幻觉）一律回错误结果「This tool is not available in a chat conversation」，不进 `authorize`。
  只靠「不给定义」不够：有的端点不校验工具名。
- **`memory_write` 的 `scope` 去掉**：聊天会话的定义里没有这个参数，`remember` 里按类型强制写全局，
  不会再往 `<数据目录>/chat/.claudesktop/` 里写项目记忆。`memory_forget` 照旧只删本会话记的。
- **联网要不要问**：`web_fetch` 是 `Access::Fetch`，Code 会话里每次都问。聊天里用户在输入框打开了「联网」
  就算同意了，`authorize` 对聊天会话的 `Fetch` 直接放行；关着时这两个工具根本不在定义里。
  已保存的「总是拒绝」规则照样生效（它们按目录记，聊天的目录是 `chat/`，所以只有在聊天里点过的才命中）。
- **钩子不跑**：`run_hooks`（会话开始 / 提交消息 / 工具前后 / 压缩前）是插件配的 shell 命令，按工作目录跑，聊天会话一律跳过。
- 不变的：自动压缩和手动 `/compact`、标题起名（`name_session`）、猜下一句、记忆候选（`memory_suggest`，候选只进全局）、
  思考档位（`set_thinking`）、`set_model`、回退（`rewind`，只有对话没有文件）、用量网关的打标（`gateway::tag` 用途照旧是 `turn`）、
  Anthropic 档案里的服务端工具（`params.server_tools`，是档案级的，聊天照样带上——那本来就是联网搜索）。

### 4.3 系统提示词

新写一份 `chat_system_prompt`，和 `system_prompt` 并列（不在原函数里加一堆 `if`）：

```text
You are a helpful assistant inside claudesktop, a desktop app. Today is {本地日期}.
Answer in the user's language. Use Markdown for structure and code. Be direct; skip filler.
{web 开着: You can search the web with web_search and read pages with web_fetch. Use them for recent events,
 facts you are unsure of, and URLs the user gives; cite the pages you used.}
{web 关着: You have no internet access; say so when a question needs current information.}
```

后面依次接：「聊天说明」（`# User instructions`）、全局记忆那一节（`memory::prompt_section(&[], global)`）、
续接说明（`restored`）、压缩摘要。**没有**工作目录、平台、`BACKGROUND`、待办清单、技能、计划 / 自动模式那几段。

日期按天写（不写到分钟）：一天之内系统提示词不变，不废提示缓存。

## 五、界面

### 5.1 入口：侧栏顶部「聊天 | 代码」

侧栏品牌行下面一个分段切换（`widgets.rs` 的 `seg()`），切的是**当前空间**（`Settings::space`，切换即写回，下次启动回到上次那个）：

- 「新对话」、Ctrl+N、首页、会话列表都跟着当前空间走：在「聊天」里新建的是聊天，在「代码」里新建的是 Code 会话。
- 切空间时回到那个空间上次的会话（`ChatApp::last_active: [Option<usize>; 2]`，按 `Session::token` 记，删会话不会错位）；
  没有就开一个空会话（复用 `add_session` 里「已经有空会话就切过去」的逻辑，按 `kind` 匹配）。
- 另一个空间里有会话在跑 / 等授权 / 有未读时，分段上那一格带个小点（`CORAL`，等授权时 `RED`），别切走了就忘了它在跑。
  并进 `sync_sidebar` 的指纹。
- 打开设置页、用量页时空间不变；从搜索结果、系统通知点进别的空间的会话时自动切过去。

为什么不做「一个列表里混着放、行首加图标」：两种会话的首页、默认值、分组方式都不一样，混在一起每个地方都要判断；
而且「问个问题」和「在 claudesktop 项目里干活」本来就是两种心思，Claude 桌面版也是这么分的。

### 5.2 侧栏列表

| | 聊天空间 | 代码空间 |
|---|---|---|
| 分组 | 按日期（今天 / 昨天 / 最近 7 天 / 更早，复用 `usage::day_bucket`） | 现在那样：项目 → 任务 → 已归档（或者按日期，`sidebar_by_date`） |
| 行 | 标题 + 运行状态 | 现在那样（worktree 图标等） |
| 已归档 | 末尾一组 | 末尾一组 |
| 筛选（进行中 / 待查看） | 有 | 有 |

`session_groups` 加一个参数 `space: SessionKind`，先按 `s.kind == space` 筛，再走各自的分组；
聊天空间的组名旁不显示「按日期 / 按项目」切换按钮（聊天只有按日期）。

### 5.3 首页

`overview.rs` 按当前空间分两种：

- **代码首页**：不变（问候语、最近会话、用量概览、四条 Code 向建议）。
- **聊天首页**：问候语 + 输入框 + 一排聊天向建议，**不放用量概览**（用量在侧栏「用量」里）。
  最近会话只列聊天的。建议（`CHAT_SUGGESTIONS`，现在的 `SUGGESTIONS` 改名 `CODE_SUGGESTIONS`）：

  | 图标 | 短名 | 填进输入框 |
  |---|---|---|
  | `MessageSquare` | 帮我改写 | 把下面这段话改得更清楚、更简洁，保留原意： |
  | `Globe` | 翻译 | 把下面这段翻译成英文，语气自然： |
  | `Lightbulb` | 解释一个概念 | 用通俗的话解释一下：（概念），再举一个例子 |
  | `ListChecks` | 列个计划 | 帮我列一个计划：（目标），按先后排好，写清每一步要多久 |

  图标以 gpui-kit 里实际有的为准，没有就换近义的。占位文字：「有什么想聊的？回车发送，Shift+回车换行」。
- **聊天没法用时**（没配自研内核端点，只配了 ACP agent）：聊天首页把建议换成一张引导卡
  「聊天直接连模型，要先配一个端点」+「去设置」按钮（跳「内核与模型」）。分段切换照样显示，不藏起来（让人知道有这个）。
  只配了自研内核时，代码空间照常可用（自研内核两样都能跑）。

### 5.4 输入框工具栏

| 控件 | Code | 聊天 |
|---|---|---|
| 目录 chip（项目 / 任务 / worktree） | 有 | **没有** |
| 权限模式 chip、Shift+Tab 切模式 | 有 | **没有**（快捷键 `cx.propagate()`） |
| 模型 chip、思考 chip | 有 | 有 |
| 附件（图片 / 文件） | 有 | 有，但文件是**内联**的（5.5） |
| 「联网」chip（`Globe`，开 / 关） | 没有 | **有**：会话级，默认值取设置 `chat.web` |
| `@` 文件补全 | 有 | **没有**（没有工作目录） |
| `/` 命令 | 有 | `/compact` 和插件的斜杠命令照旧（展开成文字，和内核无关） |
| Tab 补全、猜下一句 | 有 | 有 |

「联网」是会话级状态（`Session::web`），改了经新方法 `Kernel::set_web(session, bool)` 交给内核（`SessionState::web`），
和思考档位一个套路：会话还没 `Started` 时先记在 `Session::pending_web`，`Started` 时补交；内核重连（`detach_kernel`）时重新挂上。
下一轮开头生效，进行中的一轮不变（工具定义中途变了，有的端点会报错）。

### 5.5 聊天附件：内联文件内容

Code 会话的附件是把路径贴进输入框、等 agent 自己 `read_file`；聊天没有这个工具，要把内容直接带上。
新文件 `app/attach.rs`（纯函数部分放 `crates/core`，好单测）：

- **文本文件**（按内容判断：前 8 KB 没有 NUL、能按 UTF-8 解，带 BOM 的去掉）：读进来，单个最多 256 KB、一条消息合计最多 1 MB，
  超了 toast「xxx 太大（1.2 MB），聊天里最多带 256 KB；要处理大文件请在代码空间里开会话」。
- **图片**：和现在一样走 `PendingImage`。
- **其余**（PDF、docx、压缩包……）：toast「聊天里暂时只能带文本文件和图片」。PDF 以后可以走 Anthropic 的 document 块，这期不做。
- 输入框上方和图片缩略图排在一起显示文件 chip（文件名 + 大小，可删）；发送时拼进 `sent`：

  ```text
  <attachment name="notes.md">
  ……文件内容……
  </attachment>

  用户打的字
  ```

  日志里 `Record::User.text` 是用户打的字 + 附件名列表（界面显示用），`sent` 是全文（续接重建上下文用）——
  这正是 `sent` 现在给斜杠命令展开用的那个字段，不加新记录。对话里用户那条消息下方画附件 chip，不展开内容。
- 拖文件进窗口（`drop_paths`）在聊天里也走这套。

### 5.6 对话流与顶栏

- **工具行**：聊天里只会出现联网和记忆的工具行，照现在的样子画（`web_search` 汇总带后端名和「降级过」）。
- **没有**：改动卡片（`render_changes_card`）、逐块审查、检查点的「还原文件」、计划（待办）、子任务卡片、后台命令标点。
  这些行在聊天里本来就不会产生，不用改 `ChatItem::visible_in`；只有回退浮层要按类型改：聊天只给「回退对话」一种。
- **顶栏面包屑**：聊天写「聊天」（`MessageSquare` 图标）+ 标题，没有分支 chip。视图模式下拉（摘要 / 标准 / 详细）保留。
- **回复下方的「在 X 上重跑」**：只列能跑聊天的内核（就是自研内核的各个模型）。

### 5.7 设置页

「内核与模型」（要点保存的那一页）加一组「聊天」：

| 设置 | 字段 | 默认 | 说明（ⓘ 里的那一句） |
|---|---|---|---|
| 聊天用的模型 | `chat.model` | 空 = 跟随上面的默认模型 | 聊天可以用便宜些的模型 |
| 默认联网 | `chat.web` | 开 | 新建的聊天默认能搜索网页，输入框里可以随时关 |
| 聊天说明 | `chat.instructions` | 空 | 每次聊天都带上的话，比如「回答尽量简短」「我是前端工程师」 |

模型下拉复用「默认模型」那一行的组件和模型列表。三项都只影响**之后新建**的聊天（说明和联网例外：说明每轮重读，改了下一轮就生效）。

### 5.8 命令面板与快捷键

- 命令面板（`palette_groups`）加：「新建聊天」「新建代码会话」「切到聊天」「切到代码」。
- 快捷键：Ctrl+N 不变（在当前空间新建）。**这期不加**切空间的快捷键：Ctrl+1 / Ctrl+2 之类的组合和菜单里的数字键选项容易混，
  用一段时间看需不需要再定。速查表（`shortcuts.rs`）里 Ctrl+N 那行改成「在当前空间新建对话」。

## 六、聊天转 Code

聊着聊着想动手改代码，是最常见的跨空间需求。聊天顶栏的「…」菜单加一项「在项目里继续…」：

1. 弹目录菜单（最近的项目 + 选择文件夹… + 任务）选一个目录；
2. 新建一个 **Code 会话**，把这个聊天的日志**复制**一份当它的开头（新会话 ID、`kind: Code`、选的 `cwd`），
   接着在 Code 空间里打开它。自研内核续接时按日志重建上下文（`history::rebuild`），所以新会话「记得」前面聊过什么，
   而且现在有了工具；
3. 原来的聊天不动。

复制而不是转换：转换要改元信息里的类型和目录，原聊天就没了；复制干净，也不用处理「同一份日志两种类型」。
日志里没有文件改动和检查点记录，复制过去回退照样对。反过来（Code 转聊天）不做：Code 会话的上下文里全是工具调用，带进聊天没有意义。

这一步放到最后（第十一节第 5 步），不做也不影响前面。

## 七、兼容与迁移

- **老会话**：元信息没有 `kind`，读成 `Code`，侧栏、行为都和现在一样。「任务」区的会话不自动迁成聊天（它们可能用过工具）。
- **老设置**：`space`、`chat` 用容器级 `#[serde(default)]` 补齐；`space` 默认 `Code`——升级后打开还是熟悉的样子，
  分段切换摆在那里，点了才进聊天。
- **降级到老版本**：老版本不认 `kind`（serde 默认忽略多余字段），聊天会话读成 Code 会话，目录是数据目录里的 `chat/`，
  侧栏会多出一个叫「chat」的项目组。能用、不丢数据，可以接受。
- **只用 ACP 的用户**：聊天空间里是 5.3 的引导卡；代码空间不受影响。
- **演示内核**（`KernelChoice::Demo`）：只跑 Code 会话。
- **AGENTS.md**：落地后在「侧栏分区」「视图模式」两条里补上聊天空间，「新增对话行类型」那条加一句「想清楚聊天里出不出现」。

## 八、和别的待办的关系

- **「桌面之外」**（TODO 第 3 条）要把 `session.rs` 的状态机搬进 `crates/core`。两件事都改 `session.rs`，**别同时做**。
  推荐先做这一件：改动小，而且 `SessionKind` 本来就放在 core 里，搬家时跟着走；CLI / TUI 将来也能直接开聊天会话。
- **本机 agent 第 4 步**（ACP 模型切换）：不冲突。
- **项目级配置**：聊天不读项目配置，所以项目里写的 `model` 不影响聊天，这是对的。

## 九、已定的取舍

1. **两个空间，不是一个列表混放**：首页、默认值、分组都按空间分开写，每处少一个 `if`；代价是在空间之间跳要点一下，用分段上的小点弥补。
2. **聊天只跑自研内核**：ACP agent 都是 coding agent，强行让它「别用工具」既不可靠也浪费它的订阅额度。
3. **聊天会话有一个占位目录 `<数据目录>/chat`**：不改 `cwd` 的类型，漏网的相对路径落进空目录。
4. **工具双重收口**：不给定义 + 执行前按名单拦。聊天会话改不了文件、跑不了命令，不依赖模型自觉。
5. **联网开着就不问**：用户在输入框里打开「联网」就是同意；逐次问对聊天太吵。命令和写文件根本不存在，不需要权限模式。
6. **附件内联、有上限**：聊天没有 `read_file`，只能带内容；上限保证一条消息不会把上下文撑爆，大文件引去代码空间。
7. **聊天转 Code 用复制日志**：不改原会话，靠现有的续接重建上下文，几乎不加新机制。

## 十、需要你拍板

1. **入口形态**：推荐**侧栏顶部「聊天 | 代码」两个空间**（第五节）。另一个办法是一个列表混放、「新对话」按钮变成两个选项，改动小一些但首页和分组要处处判断。
2. **聊天带不带联网**：推荐**带，默认开，开着不逐次确认**。保守一点可以默认关，或者开着也每次问。
3. **聊天里的记忆**：推荐**只用全局记忆**（读 + `memory_write` 只写全局）。也可以聊天完全不碰记忆。
4. **聊天里的 MCP / 插件技能 / 钩子**：推荐**这期都不带**。MCP 工具副作用不可知，带了就得把权限卡请回来；
   以后要接（比如只接标了只读的 MCP 工具），在设置「聊天」一组加个开关即可，内核那边就是 `chat_specs` 里多并一段。
5. **「任务」区留不留**：推荐**保留**，它是「没挑项目的 Code 会话」。如果你本来就把任务当聊天用，也可以把任务区去掉、
   默认目录里的会话都并到聊天——但那些会话就没有工具了，要想清楚。
6. **聊天转 Code（第六节）做不做**：推荐做，放最后。
7. **「聊天说明」要不要**：推荐要（一个多行输入框，没什么成本）。不要的话聊天的系统提示词就只有通用那几句加记忆。
8. **聊天首页放不放用量概览**：推荐**不放**，聊天首页干净一点；用量在侧栏「用量」里一样能看。

## 十一、测试

- **`crates/core`**：`SessionMeta` 带 / 不带 `kind` 的读写（老文件读成 `Code`、`Code` 不写这个字段）；
  附件判断（NUL、UTF-8、BOM、单个 / 合计上限、`<attachment>` 拼接）。
- **`crates/builtin/tests/kernel.rs`**（`serve` 捕获请求体）：
  - 聊天会话的第一次请求里工具只有 `web_search` / `web_fetch` / `memory_*`，关掉联网后只剩 `memory_*`；
  - 系统提示词不含 `Working directory`，默认目录里放一份 AGENTS.md 也不读进去，`chat.instructions` 读进去了；
  - 模型返回 `shell` 调用时回错误结果、不发 `PermissionRequest`、不执行；
  - `memory_write` 落在全局文件，`chat/` 下没有 `.claudesktop/`；
  - 联网开着时 `web_fetch` 不发 `PermissionRequest`；
  - 插件钩子在聊天里不跑；
  - 聊天会话续接（`resume_session`）、回退（`rewind`）、`/compact` 正常；`set_web` 下一轮生效；
  - ACP 内核收到 `kind: Chat` 回 `SessionFailed`。
- **`apps/desktop`**：`session_groups` 按空间筛（聊天按日期、代码按项目）；切空间回到上次的会话；
  分段上的小点（另一个空间有运行中 / 待授权 / 未读）；聊天转 Code 复制出来的日志能续接。
  侧栏改动前后各跑一次 `CLAUDESKTOP_STRESS=<行数>`、首页改动前后各跑一次 `CLAUDESKTOP_STRESS_VIEW=home`，看帧间隔没变差。
- **手工**：同一个端点分别开聊天和 Code 会话问同一句话，在「请求」页签的原文里对比请求体大小（聊天应该少几千 token）。

## 十二、工作量与顺序

| 步 | 内容 | 估时 |
|---|---|---|
| 1 | core：`SessionKind`、`SessionMeta::kind`、`SessionSpec`、`supports_chat`；builtin：聊天会话的起法、`chat_specs`、工具执行拦截、`chat_system_prompt`、记忆只写全局、钩子跳过、`set_web`；ACP 兜底；测试 | 1 天 |
| 2 | desktop 数据层：`Session::kind` / `web`、建档、`new_session` / `add_session` 按类型、`pending_web`、设置的 `space` / `chat` | 半天 |
| 3 | 侧栏两个空间 + 按日期分组 + 小点、聊天首页、输入框工具栏按类型取舍、面包屑、回退浮层 | 1.5 天 |
| 4 | 附件内联、「联网」chip、设置页「聊天」一组、命令面板 | 1 天 |
| 5 | 聊天转 Code（复制日志续接） | 半天 |

合计约 4.5 天。第 1 步不改变任何现有行为（界面还没有建聊天会话的入口），可以先合；第 2～4 步一起合（中间状态界面不完整）；第 5 步单独提交。
