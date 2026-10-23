# 设计说明：聊天与 Code 模式分离

> 状态：**第 1～8 步全部落地**（第 1～3 步 `f59665c`；第 4～7 步 PR #32；第 8 步「回复里的网络图片」本批）。
> 写于 2026-10-22。第十二节的十一件事已按文中的「推荐」实现。
> 代码落点：`crates/core/src/store.rs`（`SessionKind`、`SessionMeta::kind`）、`crates/core/src/lib.rs`（`Kernel` 的 `SessionSpec`）、
> `crates/builtin/src/lib.rs`（聊天会话的起法、系统提示词、工具集）、`crates/builtin/src/tools.rs`（`chat_specs`）、
> `apps/desktop/src/session.rs`（`Session::kind`）、`settings.rs`（`chat` 一栏、`space`、`auto_load_images`）、`app/mod.rs`（建会话按空间）、
> `app/sidebar.rs`（两个空间、按日期分组）、`app/overview.rs`（聊天首页）、`app/composer.rs`（工具栏按会话类型取舍）、
> `app/attach.rs`（新增：聊天附件内联）、`app/chrome.rs`（面包屑）、`app/rewind.rs`（聊天只回退对话）、
> `app/settings_view.rs`（「内核与模型」里的「聊天」一组、「外观」里的「自动加载回复里的网络图片」）、`app/palette.rs`；
> 公式（两种会话共用）：`crates/core/src/math.rs`（新增：RaTeX 排版、写法归一、缓存）、`app/math.rs`（新增：`MarkdownPlugin` + SVG 资源）、
> `main.rs`（`AssetSource` 包一层）、`app/chat.rs`（`TextView` 挂插件）；
> 图片（两种会话共用）：`crates/core/src/image_prep.rs`（新增：转正 / 缩小 / 重编码）、`crates/builtin/src/provider.rs`（`ModelInfo::vision`、能力表）、
> `model.rs`（`PendingImage` 异步处理）、`app/composer.rs`（附件按钮、看不了图的提示）、`app/chat.rs`（回复里的图片）；
> 远程图片（第 8 步）：`src/http.rs`（新增：`get_bytes` 取字节，走 `ureq`）、`app/remote_image.rs`（新增：占位卡 + 加载状态 + `MarkdownPlugin`）、
> `assets.rs`（`register_image` / `cached`，按内容哈希登记字节）、`app/chat.rs`（`ImageView` + 图片浮层的「另存为…」）。

## 一、要解决什么

现在每个会话都是**写代码的 agent**，就算只是问一句「这段英文怎么翻」：

- **系统提示词**开头是「You are a coding agent … Working directory: …」，附 `BACKGROUND`（无头跑测试那段）、
  项目里的 AGENTS.md / CLAUDE.md、技能清单；**工具定义** 16 个内置工具 + `task` + 全部 MCP 工具，
  每次请求都带（只算 16 个内置的，`tools::specs` 的定义粗估就有 3～4k token）。
  模型被提示成「先 glob、read_file 再回答」，闲聊时也会去翻工作目录。
- **「任务」不是聊天**：没选文件夹的会话跑在默认目录（`acp_base.cwd`）里，工具、权限、检查点一样不少；
  记忆按 `memory::project_file_or(cwd)` 写进**默认目录**下的 `.claudesktop/memory.json`，那份文件谁都不认。
- **界面全是 Code 向的**：输入框工具栏有目录、权限模式，
  回复末尾是改动卡片，回退浮层问「还原文件吗」，附件只是把路径贴进输入框（等着 agent 去 `read_file`）。
- **公式不显示**：数学、物理、算法、统计的回答里全是 LaTeX，现在行内公式原样显示 `$\frac{a}{b}$` 源码，块公式成了代码块，
  `\[ … \]` 写法连反斜杠一起露出来（第六章）。
- **图片发得糙**：原图不缩不压、BMP 原样发（接口不收）、照片带着拍摄地点、竖拍的方向不对、看不了图的模型直接报英文 400（第七章）。

目标：分成两种会话。

| | 聊天 | Code |
|---|---|---|
| 用途 | 问答、写作、翻译、头脑风暴、看图 | 改代码、跑命令、在某个目录里干活 |
| 工作目录 | 没有 | 有（项目，或者「任务」用的默认目录） |
| 工具 | 只有联网（`web_search` / `web_fetch`，可关）和全局记忆 | 现在那一整套 |
| 权限 | 没有权限模式，没有权限卡 | 现在那套（逐项确认 / 自动 / …） |
| 内核 | 只有自研内核 | 自研内核或 ACP agent |
| 系统提示词 | 通用助手 + 用户写的「聊天说明」 + 全局记忆 | 现在那份 |
| 公式 | 渲染（第六章），提示词要求按 `$` / `$$` 写 | 同样渲染，提示词不改 |
| 图片 | 发送前转正、缩小、重编码，先看模型能不能看图（第七章） | 同左 |

不做：一次会话中途切换类型（建好就定了，和内核一样）；ACP agent 跑聊天；聊天里的 MCP / 插件技能 / 钩子（见第十二节第 4 条）；
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
Write math in LaTeX: $...$ inline and $$...$$ on its own lines for display. Do not use \( \) or \[ \].
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

- **代码首页**：问候语 + 最近会话 + 用量概览（原先的四条 Code 向建议入口已去掉）。
- **聊天首页**：问候语 + 输入框，**不放用量概览**（用量在侧栏「用量」里）。
  最近会话只列聊天的。占位文字：「有什么想聊的？回车发送，Shift+回车换行」。
  （原先聊天向的 `CHAT_SUGGESTIONS` 一排建议入口也一并去掉了。）
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
- **图片**：走 `PendingImage`，发送前按第七章处理。
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

## 六、公式渲染

聊天里问数学、物理、算法、统计，回复里全是公式。**这一章和第七章不只给聊天用**：渲染器挂在所有画回复的 `TextView` 上，
Code 会话（推导复杂度、写数值代码）同样受益；聊天的系统提示词再额外要求模型按约定的写法出公式。

### 6.1 现状

gpui-kit 的 Markdown（`gpui-base` 的 `text/format/markdown.rs`）**已经把公式解析出来了**，只是没人画：

- 解析开着 `math_text` / `math_flow`：`$x^2$` 是 `InlineMath` 节点，`$$…$$` 是 `Math` 节点；
- 没有插件认领时，行内公式**原样显示源码**（连 `$` 一起），块公式**退成代码块**（`format/markdown.rs` 的 `Node::Math` 分支）；
- 留了扩展口：`MarkdownPlugin`（`parse` 在后台解析时跑，`render_inline` 返回带基线的 `InlineElement`，
  块级插件 `is_block() == true`），经 `TextView::markdown_extensions` 挂上。没人认领的 `$5 and $10` 会被重新当成普通文字
  （`flatten_unclaimed_math`），所以**插件拒绝认领 = 安全的退路**。
- 模型常用的 `\( … \)`、`\[ … \]`（DeepSeek、GPT 系列尤其爱用）Markdown 根本不认，显示成带反斜杠的乱码。

### 6.2 用什么排版：RaTeX

| | RaTeX（`ratex-*` 0.1.15） | Typst + mitex | KaTeX / MathJax |
|---|---|---|---|
| 语言 | 纯 Rust | 纯 Rust | JavaScript（要嵌 JS 引擎） |
| LaTeX 语法 | 对齐 KaTeX（自称覆盖 99.5%），含 `align` / 矩阵 / `\ce` 化学式 / `\tag` 编号 | mitex 把 LaTeX 转成 Typst 数学，常见写法可以，边角对不上 | 最全 |
| 输出 | `DisplayList`（字形 / 线 / 矩形 / 路径，em 单位，带宽高深）→ SVG / PNG | Typst 文档 → SVG / PNG | HTML / SVG |
| 字体 | KaTeX 字体 20 个文件，约 550 KB，`embed-fonts` 打进程序 | Typst 数学字体 + 整个 Typst 编译器（依赖重、编译慢） | 同 KaTeX |
| 依赖 | `tiny-skia` 0.11、`ttf-parser` 0.25、`png` 0.17——**Cargo.lock 里都已经有了** | 上百个新 crate | 要 `quickjs` 之类的 C 依赖 |
| 成熟度 | 2026 年的新项目，更新很勤（0.1.15，2026-10-16），MIT | Typst 成熟，mitex 2024 年后没再发版 | 成熟 |

选 **RaTeX**：模型输出的公式是按 KaTeX / MathJax 的习惯写的，语法对齐 KaTeX 最省事；依赖几乎都现成。
风险是项目新：版本钉死，接口全包在我们自己的 `crates/core/src/math.rs` 里（对外只有「源码 → 排好的公式 / 出错原因」），
将来要换 Typst 只改这一个文件。

### 6.3 怎么画：SVG 当作单色图标交给 gpui

1. **解析与排版在 `MarkdownPlugin::parse` 里做**（gpui-kit 在后台线程解析 Markdown，不占 render）：
   `ratex_parser::parse` → `ratex_layout::layout` → `to_display_list`，再用 `ratex_svg::render_to_svg`（`embed_glyphs: true`，
   字形转成路径，不依赖系统字体）生成 SVG。结果连同宽 / 高 / 深（em）放进 `MarkdownNode` 的 data。
   按「源码 + 行内 / 块」缓存（全局 LRU，512 条）：流式输出时整段回复每来一块就重新解析一次，前面的公式不重排。
2. **渲染**：SVG 以 `math/<哈希>.svg` 的名字登记进我们自己的 `AssetSource`（新的 `Assets` 包一层 gpui-kit 的 `AllAssets`，
   `math/` 前缀从内存表里取，其余照旧转发），然后就是一个 `svg().path(..)`：
   - gpui 画 SVG 是**单色蒙版 + 着色**（图标就是这么画的），颜色在画的时候给——跟着 `INK` 和外观、配色走，换主题不用重新生成；
   - 尺寸按 em × 当前字号算，gpui 按实际像素栅格化并进图集缓存——跟着界面缩放（rem）一起变，任何缩放都不糊；
   - 滚动时只是贴图集里的图，不重新排版、不重新栅格化。内容区缓存 view 的约束（`content.rs`）照样成立。
   - 公式里写了 `\color{red}` 的会丢颜色（单色蒙版）。这种很少，这期接受；要支持再对它们单独走 `img()` 彩色位图。
3. **行内公式**：`render_inline` 返回 `InlineElement::new(svg).with_baseline(高 × 字号)`，和正文基线对齐；字号取正文的 1.1 倍
   （KaTeX 默认 1.21 倍，配无衬线正文偏大）。超高的（`\dfrac`、矩阵）会把那一行撑高，这是对的。
4. **块公式**：居中；比阅读列（`READING_W`）宽的不缩小，横向滚动（`.id(..).overflow_x_scroll()`）；
   悬停出一个 `message_action` 风格的「复制 LaTeX」。`\tag` / `align` 的编号 RaTeX 自己排在右边。
5. **复制**：选中一段带公式的回复复制，得到的是 `$…$` 源码（`MarkdownNode` 的 `as_markdown` 就是原文），粘到别处还能用。
   导出 Markdown（「分享」）同样保留源码。

### 6.4 写法归一

不同模型的公式写法五花八门，画之前统一一遍（`math::normalize`，纯函数，好单测），**只在代码块和行内代码之外**改：

| 模型写的 | 换成 | 备注 |
|---|---|---|
| `\( … \)` | `$…$` | |
| `\[ … \]`（可跨行） | `$$\n…\n$$` | 前后补空行，保证是块 |
| `\begin{equation} … \end{equation}`（裸环境，外面没包 `$$`） | 包进 `$$` | `align`、`gather` 同理 |
| `$ x $`（两头有空格） | 不动 | 见下面的货币规则 |

**货币和普通美元符号**：插件在 `parse` 里按 Pandoc 的规则决定认不认领——开头的 `$` 后面不能是空白，结尾的 `$` 前面不能是空白、
后面不能紧跟数字。`花了 $5 和 $10` 不认领，交回 gpui-kit 当普通文字；`$x$`、`$\alpha+1$` 认领。

聊天的系统提示词加一句（第 4.3 节）：

```text
Write math in LaTeX: $...$ inline and $$...$$ on its own lines for display. Do not use \( \) or \[ \].
```

Code 会话的提示词不加（不改现有缓存命中），靠 6.4 的归一兜底。

### 6.5 出错与流式

- **语法错**（模型写了 RaTeX 不认的命令）：原样显示源码，等宽字体、`INK_SOFT`，悬停 `text_tip` 显示「公式没能排版：<原因>」
  （`ParseError` 的英文信息前加一句中文）。不显示红色：错的是模型的输出，不是用户的。
- **流式时写了一半**：`$$` 还没闭合时 markdown-rs 不会出 `Math` 节点（或者出一个到结尾的），这时这一块按「还在写」处理——
  显示淡色源码、不报错；这一轮结束后再按正常规则画。判断「还在写」用回复行现有的流式状态（`chat.rs` 里画 agent 文字那段知道）。
- **太大的公式**（几百行的矩阵）：RaTeX 自带 `MAX_LAYOUT_OUTPUT_ITEMS` 上限，超了按出错处理（显示源码）。

### 6.6 不做

图表（Mermaid / 函数图像）、化学结构式（`\ce` 的反应式能画，分子结构图不行）、用户消息里的公式（用户消息是纯文字，按原样显示）、
公式编辑器。Mermaid 以后可以照同样的路子（Rust 排版 → SVG → 单色或彩色贴图）做，这期不排。

## 七、图片发送

聊天里发图的场景比 Code 多得多：拍一道题、截一张报错、发一张图表让它读数、手写公式让它转 LaTeX（和第六章正好接上）。
同样**两种会话共用**这一套。

### 7.1 现状（`model.rs` 的 `PendingImage`、`composer.rs` 的 `paste_to_attachments`、`mod.rs` 的 `store_images`、`history.rs` 的 `user_message`）

- 粘贴、拖放能收图（png / jpg / gif / webp / bmp），一条消息最多 8 张（`MAX_PENDING_IMAGES`）；
- **原样发**：不缩小、不压缩、不转正。手机照片 4000×3000、5 MB 以上很常见，base64 之后更大——
  Anthropic 接口单张上限 5 MB、长边 8000 px，超了整轮 400；官方建议长边 1568 px 以内（约 1.15 MP），更大的图服务端也会先缩，
  发原图只是白白多传几 MB、多等首字（会话越长越明显：续接和后面每一轮都会把历史里的图再发一遍）；
- **BMP 原样发**：Anthropic 和 OpenAI 都只收 png / jpeg / gif / webp，BMP 必报错；
- **EXIF 原样带**：手机照片里有拍摄地点（GPS），跟着请求发给了模型服务商；方向标记没处理，竖拍的照片模型看到是横的；
- **不知道模型能不能看图**：纯文本模型（不少 DeepSeek、GLM、Kimi 的文本型号）直接报 400，报错是英文原文；
- 输入框上的「附件」按钮选了图片也只是把**路径**贴进输入框（`attach_files`），不当图片发；
- 回复里的图片：`data:` 内嵌图能显示；`https://` 远程图应用没给 gpui 配 HTTP 客户端，显示不出来（待实测确认现在是空白还是替代文字）。

### 7.2 发送前处理（`crates/core/src/image_prep.rs`，纯函数）

用 `image` 0.25（gpui 已经依赖它，Cargo.lock 里有；我们直接依赖同一版本，按需开 jpeg / png / gif / webp / bmp / tiff 特性）：

1. **解码 + 转正**：读 EXIF 方向（`ImageDecoder::orientation`）、`apply_orientation`；
2. **缩小**：长边 ≤ 1568 px 且总像素 ≤ 1.15 MP（两家的建议都满足）；设置里「图片清晰度」选「原图」时只在超过接口硬上限时缩；
3. **重新编码**（顺带去掉全部元数据）：有透明或颜色少（截图、公式、图表）→ PNG；照片 → JPEG 质量 85；
   动图 GIF 在上限内原样，超了取第一帧；BMP / TIFF → PNG；
4. **兜底上限**：处理完单张仍超过 3.75 MB（base64 后 5 MB）就降质量 / 再缩一档，直到放得下；
5. **读不了的格式**（HEIC、AVIF、RAW）：toast「这种格式（HEIC）读不了，先另存为 JPG 或 PNG」，不加进附件。

处理在后台线程跑（`cx.background_spawn`；一张 1200 万像素的照片解码 + 缩放要一两百毫秒），缩略图先显示半透明占位
（不放转圈：输入区画在 `ChatApp` 里，循环动画会带着内容区一起重画，见 AGENTS.md「动效」），处理完换成真图；还有图没处理完时发送键置灰。日志里存的是**处理后**的那份（模型实际看到的），不另存原图。

缩略图悬停提示：`1568×1176 · 约 2.5k token`（Anthropic 的估法；OpenAI 系按它的切块算，误差不大，都只是提示）。

### 7.3 模型能不能看图

- `ModelInfo` 加 `vision: Option<bool>`：模型列表接口报了就用（OpenRouter 的 `architecture.input_modalities` 含 `image`，
  有的网关有 `modalities` 字段），没报就查**能力表**（`provider.rs`，和思考档位的表一个写法：按「协议 + 模型名」），表里也没有就是 `None`（不知道）。
- **知道看不了**（`Some(false)`）：附上图时输入框上方出一行「当前模型 xxx 看不了图片」+「换模型」chip（打开模型菜单）；
  这时按发送不发，toast 一句同样的话，图留在输入框里。
- **不知道**（`None`）：照发。端点回 400 且错误文本认得出是「不支持图片」（各家关键词收进 `explain_*` 一类的翻译函数，
  如 `image_url is not supported`、`does not support image`、`multimodal`），就翻译成中文
  「这个模型看不了图片，换一个能看图的模型重发」，并把这个模型记进设置 `no_vision_models`，下次直接按「看不了」处理。
  设置页模型那一行旁边能看到、能清掉（万一是误判）。
- 续接 / 换模型后历史里有图、新模型看不了：发请求前把历史里的图换成文字占位 `[image omitted: the current model cannot view images]`，
  不让整轮报错（只影响发出去的请求，日志不动，换回能看图的模型又是原图）。

### 7.4 入口

- **「附件」按钮**：选到的图片走 7.2 进图片附件；聊天里的文本文件按 5.5 内联；Code 会话里的非图片照旧贴路径。
- 粘贴、拖放：不变，都过 7.2。
- 输入框上方的缩略图（56 px）点了用现有的 `open_image` 浮层看大图（现在只有发出去的才能点）。
- 一条消息最多 8 张不变（Anthropic 一次请求超过 20 张时单张上限降到 2000 px，8 张离它很远）。

### 7.5 回复里的图片

- 点回复里的图片同样打开 `open_image` 浮层，浮层里加「另存为…」（`cx.prompt_for_new_path`，
  本地图读文件、远程图写已经取回的那份字节）。
- **远程图片**（`https://…`）：`src/http.rs` 的 `get_bytes` 取字节，底下复用 `ureq`，代理按 `ureq` 的默认规则读环境变量。
  **不给 gpui 配 `HttpClient`**（保持 `NullHttpClient`）：插件认领不到的远程图（回复里原始 HTML 的 `<img src="https://…">`）
  否则会被 `img()` 不问用户就请求；现在它们取不回来，「先问用户」没有旁路。
  但**默认不自动加载**：远程图片会把用户的 IP 和「正在看这条回复」告诉图片所在的服务器，模型给的链接也可能是编的。
  默认显示一张占位卡「图片 · example.com」+「加载」，设置「外观」里一个开关「自动加载回复里的网络图片」（默认关）。
  落地时多做了一步：**真图不走 `img("https://…")` 的隐式加载**——`app/remote_image.rs` 的 `MarkdownPlugin`
  把远程地址认领成自定义节点，点了「加载」（或开关打开）才用 `crate::http::get_bytes` 取字节，
  按内容哈希登记进 `assets::register_image` 再交给 `img()`。这样「先问用户」是结构上的保证，
  同一张图只请求一次、只占一份内存，失败也能在卡片上写清原因（HTTP 状态、主机名解析不了、超过 4 MiB）。
- 不做：模型生成图片（文生图）、图片编辑、PDF（以后走 Anthropic 的 document 块 / OpenAI 的 file 输入，另写一期）。

## 八、聊天转 Code

聊着聊着想动手改代码，是最常见的跨空间需求。聊天顶栏的「…」菜单加一项「在项目里继续…」：

1. 弹目录菜单（最近的项目 + 选择文件夹… + 任务）选一个目录；
2. 新建一个 **Code 会话**，把这个聊天的日志**复制**一份当它的开头（新会话 ID、`kind: Code`、选的 `cwd`），
   接着在 Code 空间里打开它。自研内核续接时按日志重建上下文（`history::rebuild`），所以新会话「记得」前面聊过什么，
   而且现在有了工具；
3. 原来的聊天不动。

复制而不是转换：转换要改元信息里的类型和目录，原聊天就没了；复制干净，也不用处理「同一份日志两种类型」。
日志里没有文件改动和检查点记录，复制过去回退照样对。反过来（Code 转聊天）不做：Code 会话的上下文里全是工具调用，带进聊天没有意义。

这一步放到最后（第十四节第 7 步），不做也不影响前面。

## 九、兼容与迁移

- **老会话**：元信息没有 `kind`，读成 `Code`，侧栏、行为都和现在一样。「任务」区的会话不自动迁成聊天（它们可能用过工具）。
- **老设置**：`space`、`chat` 用容器级 `#[serde(default)]` 补齐；`space` 默认 `Code`——升级后打开还是熟悉的样子，
  分段切换摆在那里，点了才进聊天。
- **降级到老版本**：老版本不认 `kind`（serde 默认忽略多余字段），聊天会话读成 Code 会话，目录是数据目录里的 `chat/`，
  侧栏会多出一个叫「chat」的项目组。能用、不丢数据，可以接受。
- **只用 ACP 的用户**：聊天空间里是 5.3 的引导卡；代码空间不受影响。
- **演示内核**（`KernelChoice::Demo`）：只跑 Code 会话。
- **AGENTS.md**：落地后在「侧栏分区」「视图模式」两条里补上聊天空间，「新增对话行类型」那条加一句「想清楚聊天里出不出现」。

## 十、和别的待办的关系

- **「桌面之外」**（TODO 第 3 条）要把 `session.rs` 的状态机搬进 `crates/core`。两件事都改 `session.rs`，**别同时做**。
  推荐先做这一件：改动小，而且 `SessionKind` 本来就放在 core 里，搬家时跟着走；CLI / TUI 将来也能直接开聊天会话。
- **本机 agent 第 4 步**（ACP 模型切换）：不冲突。
- **项目级配置**：聊天不读项目配置，所以项目里写的 `model` 不影响聊天，这是对的。

## 十一、已定的取舍

1. **两个空间，不是一个列表混放**：首页、默认值、分组都按空间分开写，每处少一个 `if`；代价是在空间之间跳要点一下，用分段上的小点弥补。
2. **聊天只跑自研内核**：ACP agent 都是 coding agent，强行让它「别用工具」既不可靠也浪费它的订阅额度。
3. **聊天会话有一个占位目录 `<数据目录>/chat`**：不改 `cwd` 的类型，漏网的相对路径落进空目录。
4. **工具双重收口**：不给定义 + 执行前按名单拦。聊天会话改不了文件、跑不了命令，不依赖模型自觉。
5. **联网开着就不问**：用户在输入框里打开「联网」就是同意；逐次问对聊天太吵。命令和写文件根本不存在，不需要权限模式。
6. **附件内联、有上限**：聊天没有 `read_file`，只能带内容；上限保证一条消息不会把上下文撑爆，大文件引去代码空间。
7. **聊天转 Code 用复制日志**：不改原会话，靠现有的续接重建上下文，几乎不加新机制。
8. **公式在后台排版、画成单色 SVG**：排版跟着 Markdown 解析走后台线程，渲染时只贴 gpui 图集里的图；颜色跟主题、清晰度跟缩放，
   代价是 `\color` 丢色。排不出来就显示源码，绝不让一个错公式把整段回复弄坏。
9. **公式和图片两种会话共用**：它们是「内容怎么显示 / 怎么发」，和有没有工具无关；只有「要求模型按 `$` 写」只进聊天的提示词。
10. **图片只存处理后的那份**：日志里是模型实际看到的，回放、续接、换模型都一致；不为「留原图」多占一倍磁盘。

## 十二、需要你拍板

> 已在 PR #32 里按下面每条的「推荐」实现；不同意的再单独改。

1. **入口形态**：推荐**侧栏顶部「聊天 | 代码」两个空间**（第五节）。另一个办法是一个列表混放、「新对话」按钮变成两个选项，改动小一些但首页和分组要处处判断。
2. **聊天带不带联网**：推荐**带，默认开，开着不逐次确认**。保守一点可以默认关，或者开着也每次问。
3. **聊天里的记忆**：推荐**只用全局记忆**（读 + `memory_write` 只写全局）。也可以聊天完全不碰记忆。
4. **聊天里的 MCP / 插件技能 / 钩子**：推荐**这期都不带**。MCP 工具副作用不可知，带了就得把权限卡请回来；
   以后要接（比如只接标了只读的 MCP 工具），在设置「聊天」一组加个开关即可，内核那边就是 `chat_specs` 里多并一段。
5. **「任务」区留不留**：推荐**保留**，它是「没挑项目的 Code 会话」。如果你本来就把任务当聊天用，也可以把任务区去掉、
   默认目录里的会话都并到聊天——但那些会话就没有工具了，要想清楚。
6. **聊天转 Code（第八章）做不做**：推荐做，放最后。
7. **「聊天说明」要不要**：推荐要（一个多行输入框，没什么成本）。不要的话聊天的系统提示词就只有通用那几句加记忆。
8. **聊天首页放不放用量概览**：推荐**不放**，聊天首页干净一点；用量在侧栏「用量」里一样能看。
9. **公式引擎**：推荐 **RaTeX**（语法对齐 KaTeX、依赖几乎现成、字体 550 KB）。另一个选择是 Typst + mitex：更成熟，
   但要拉进整个 Typst 编译器，LaTeX 的边角写法转不过去。
10. **图片默认清晰度**：推荐**「标准」**（长边 1568 px、约 1.15 MP，两家的建议值）。密密麻麻的小字截图可能要「原图」，
    做成设置项，默认标准。
11. **回复里的网络图片**：按推荐做了**默认不自动加载**，点「加载」才请求（隐私，也防模型编的链接）；
   设置「外观」里的开关默认关。

## 十三、测试

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
- **公式**（`crates/core/src/math.rs` 单测）：写法归一（`\(` / `\[` / 裸 `align` 环境，代码块和行内代码里不动）；
  货币规则（`$5 和 $10` 不认领、`$x$` 认领、`$ x $` 不认领）；常见公式能排（分式、根号、积分、矩阵、`align`、`\ce`）且宽高深非零；
  语法错返回原因不 panic；缓存命中。界面：一段 200 个公式的长回复跑 `CLAUDESKTOP_STRESS=<行数>`，滚动帧间隔和没有公式时比不能明显变差；
  切换浅 / 深色、改界面缩放后公式颜色和清晰度跟着变。
- **图片**（`crates/core/src/image_prep.rs` 单测，测试图在代码里现生成）：带 EXIF 方向 6 的 JPEG 转正；4000×3000 缩到上限内；
  BMP → PNG；带透明的保持 PNG；输出不含 EXIF；超大图兜底降到 3.75 MB 以内；HEIC 报「读不了」。
  provider：能力表、`input_modalities` 解析、「不支持图片」的 400 认得出并翻译；看不了图的模型请求里历史图片换成占位文字。
- **手工**：同一个端点分别开聊天和 Code 会话问同一句话，在「请求」页签的原文里对比请求体大小（聊天应该少几千 token）。
- **远程图片**（`app/remote_image.rs` 单测）：只有 `http(s)` 认领（`file://` / `data:` / 相对路径放行给 gpui）；
  主机名取裸域名（端口、`user:pass@`、查询串都剥掉），取不出来时退回整条地址、截到 60 字符；
  状态按地址各算各的；「自动加载」默认关。`assets.rs`：字节按内容哈希登记、同图同名、非图片字节返回 `None`。
  手工：回复里放一个 `![](https://…)` → 先出占位卡，点「加载」才发请求（可在代理 / 抓包里确认）；
  关掉网络或写个 404 地址时卡片上显示原因并能「重试」；点开的浮层里「另存为…」存下来的是真实图片；
  设置打开「自动加载」后重开对话直接出图。

## 十四、工作量与顺序

| 步 | 内容 | 估时 |
|---|---|---|
| 1 | core：`SessionKind`、`SessionMeta::kind`、`SessionSpec`、`supports_chat`；builtin：聊天会话的起法、`chat_specs`、工具执行拦截、`chat_system_prompt`、记忆只写全局、钩子跳过、`set_web`；ACP 兜底；测试 | 1 天 |
| 2 | desktop 数据层：`Session::kind` / `web`、建档、`new_session` / `add_session` 按类型、`pending_web`、设置的 `space` / `chat` | 半天 |
| 3 | 侧栏两个空间 + 按日期分组 + 小点、聊天首页、输入框工具栏按类型取舍、面包屑、回退浮层 | 1.5 天 |
| 4 | 附件内联、「联网」chip、设置页「聊天」一组、命令面板 | 1 天 |
| 5 | **公式**：`math.rs`（RaTeX 排版 + 写法归一 + 货币规则 + 缓存）、`MarkdownPlugin`（行内 / 块）、`AssetSource` 包一层、出错与流式、复制 LaTeX、测试与压测 | 2 天 |
| 6 | **图片**：`image_prep.rs`、后台处理与占位、附件按钮收图、`ModelInfo::vision` + 能力表 + 400 识别与记忆、历史图片占位、待发送缩略图可点开、回复里的图片另存为 | 2 天 |
| 7 | 聊天转 Code（复制日志续接） | 半天 |
| 8 | 回复里的网络图片：取字节（`http.rs`）、占位卡与「加载」（`remote_image.rs`）、`assets` 登记、浮层「另存为…」、设置开关 | 1 天 |

聊天分离本身（第 1～4 步）约 4 天；加上公式、图片和聊天转 Code 约 8.5 天，带上第 8 步约 9.5 天。

- 第 1 步不改变任何现有行为（界面还没有建聊天会话的入口），可以先合；第 2～4 步一起合（中间状态界面不完整）；第 7 步单独提交。
- **第 5、6 步不依赖聊天分离**，可以排在最前面单独做、单独合：现在的 Code 会话马上就能用上公式渲染和图片处理，
  聊天空间做好时这两样已经是现成的。推荐顺序：5 → 6 → 1～4 → 7 →（8）。
