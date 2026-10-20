# 调研报告：接入 GitHub（工作目录 / 云上开发）

> 调研时间：2026-10 · 结论：**云上（Codespaces 式）开发路线不做**；「从 GitHub 拉仓库当工作目录」
> 属于顺水推舟的小改动，留作待办，不单独排期。
> 本文留着是为了以后不用再从头调研一遍——真要动手，从第四节的最小改动清单开始。

## 一、结论

想做的形态是「像 Claude Code 那样在云上连 GitHub、直接在仓库里开发」。调研后分三层看：

| 层 | 做什么 | 可行性 | 量级 |
|---|---|---|---|
| A. GitHub 当**项目管理对象** | 克隆仓库当工作目录、改完推分支、开 PR | ✅ 现在基本就有，只差入口 | 1～2 天 |
| B. GitHub 当**代码来源** | 输入框上方「本地」多一档 `GitHub › owner/repo`，选中即克隆并切过去 | ✅ 顺理成章 | 2～3 天 |
| C. GitHub 当**执行环境** | 代码、shell、读写文件全跑在远端，本地只是客户端 | ⚠️ 要加一层虚拟文件系统，能做但代价大 | 2～4 周 |

**决定：不做 C。** A 的入口已由 `github` MCP + `gh` 覆盖大半，B 想做时再按第四节做；先不动代码。

不做 C 的理由：

1. **GitHub 没有给第三方 harness 的通用远端执行 API**。Codespaces 只能靠 `gh codespace ssh`（长连 SSH，
   要保活、要处理进程树）或 `gh codespace cp`（每次调用一个进程 + 一次网络往返），
   像 `grep` / `glob` 这种高频操作根本撑不住，体验会明显差于本地。
2. 本地执行的假设贯穿全仓（见第二节），改造成本不在「加个开关」，而在把整条 IO 路径抽象化。
3. 与项目既有取向冲突：检查点与回退（`设计说明-检查点与回退.md`）、逐块审查、`hard_block` 都建立在
   「我们能直接在 UI 进程里读写这些文件」之上；远端执行会让这三样同时失效或需要重做。
4. 现有护城河是本地性能（1 万行对话流式滚动、无 Electron）。云的收益是「不在这台机器上留代码」，
   这个需求用户目前没有。

## 二、现状调研（如果以后动手，这些是要紧的事实）

### 1. 工作目录是一个操作系统路径，贯穿整个代码库

- `Session.cwd: PathBuf`（`apps/desktop/src/session.rs:49`）、`SessionMeta.cwd: PathBuf`（`crates/core/src/store.rs:29`）
- 侧栏分组的 key 是目录字面量：`format!("dir:{}", cwd.display())`（`apps/desktop/src/app/sidebar.rs:842`）
- 输入框上方的目录 chip：`Self::chip(IconAll::Monitor, "本地")` + `menu_popover("workspace-menu", …)`
  （`apps/desktop/src/app/composer.rs:324`），菜单条目在 `workspace_menu_items()`（`composer.rs:641`）
- 切目录：`ChatApp::switch_workspace`（`app/mod.rs:658`）→ 写 `settings.workspace` →
  空会话原地 `replace_session`、已有对话就 `open_session` 另开一个（对话中途不换目录）
- 内核侧：`BuiltinConfig.cwd` 进系统提示词（`crates/builtin/src/lib.rs:1885`）、工具的相对路径以它解析
  （`crates/builtin/src/tools.rs:1436` `resolve`）；ACP 走 `session/new` 的 `cwd`

### 2. 执行全在本地直连

- 工具层全是 `std::fs::*` + `std::process::Command`：`read_file` / `list_dir` / `write_file` / `edit_file` /
  `run_shell`（`tools.rs:1511`～`1820`）、`glob` / `grep`（走 `ignore` crate 本地遍历，`tools.rs:1240`）
- 检查点在 UI 进程里直接读盘：`checkpoint::read_snapshot`（`crates/core/src/checkpoint.rs:26`）；
  逐块审查（`apps/desktop/src/app/review.rs`）同样
- 项目配置 / 说明 / 记忆都从工作目录读：`crates/core/src/project.rs`、`crates/builtin/src/instructions.rs`
- **目前没有任何 `Fs` / `Exec` 抽象**——这就是 C 的成本所在

### 3. 已经有的、能复用的

- `github` MCP 推荐项已内置（`crates/core/src/catalog.rs:114`，远程 Streamable HTTP，需要 PAT）：
  查仓库、提 issue、看/改 PR 已经能做
- git 子进程封装：`crates/core/src/plugin_git.rs`（`normalize_url` 支持 `作者/仓库` 简写、
  `GIT_TERMINAL_PROMPT=0` 不弹窗、`explain_git_error` 中文化）
- `cx.open_url`（`apps/desktop/src/app/mcp_view.rs:523`）可开浏览器

### 4. 「选过才算项目」的判定会被虚拟路径打坏

`is_project_dir`（`app/mod.rs:629`）、`recent_workspaces` 的 `filter(|d| d.is_dir())`（`app/mod.rs:740`）、
启动时的 `settings.workspace.filter(|dir| dir.is_dir())`（`app/mod.rs:287`）——`cwd` 一旦不是真实目录，
这些地方会**静默丢弃**它。做 B 路线时 `cwd` 仍保持真实本地路径，就是在绕开这一整片判定
（这也是 B 比 C 便宜的根本原因）。

## 三、真要做 C 需要动什么

```
crates/core/src/fs.rs（新）
  pub trait Workspace: Send + Sync {
      fn read(&self, path: &Path) -> Result<String>;
      fn write(&self, path: &Path, text: &str) -> Result<()>;
      fn list(&self, path: &Path) -> Result<Vec<Entry>>;
      fn exec(&self, cmd: &str, cwd: &Path, timeout: Duration) -> Result<Output>; // 流式 + 中断
  }
  LocalWorkspace / SshWorkspace(gh codespace ssh) / …
```

然后要改的：

- `crates/builtin/src/tools.rs`：约 30 处 `std::fs` / `Command` 调用，`resolve` / `display` / `read_for_preview`
- `crates/core/src/checkpoint.rs`、`apps/desktop/src/app/review.rs`：必须走同一抽象，
  否则检查点 / 回退 / 逐块审查在远端全部失效
- `crates/builtin/src/instructions.rs`、`crates/core/src/project.rs`、`crates/builtin/src/memory.rs`：
  项目说明 / 项目配置 / 项目记忆改成远端读
- `glob` / `grep`：远端没有 `ignore` 的遍历，只能靠 `git ls-files` + 远端 grep，或全量同步

## 四、若以后做 A / B（最小改动清单）

**A（入口）**

1. `workspace_menu_items()`（`composer.rs:641`）加一条「克隆 GitHub 仓库…」，
   弹 `Modal` + `form_field`（地址），后台线程调 `plugin_git::install` 那套逻辑克隆到
   `<数据目录>/workspaces/<repo>`，完成 `switch_workspace(那里)`
2. 失败复用手写中文化（`plugin_git::explain_git_error`）；克隆中用 `quiet_button.loading()`
3. 推分支 / 开 PR 不必自造轮子：`github` MCP，或让模型跑 `gh pr create`

**B（chip 两级）**

1. chip 改成 `本地 / GitHub` 两个 trigger + 项目菜单，条目分「本地目录 / 任务 / GitHub 仓库 / 克隆…」
2. `SessionMeta` 加可选字段（照 `task: bool` 的写法，`#[serde(default)]` 兼容老日志）：

   ```rust
   /// 仓库来源（`owner/repo`）；None = 本地文件夹
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub repo: Option<String>,
   ```

   **`cwd` 仍是真实本地路径**，`repo` 只是标签 + 用来找回克隆目录。
3. `sync_sidebar` 的指纹并进 `repo`，否则侧栏不刷新（见 AGENTS.md「侧栏是缓存 view」）；
   `project_label()`（`app/mod.rs:1891`）对 `owner/repo` 单独处理。

UI 约定照 AGENTS.md：菜单用 `menu_popover()` + `MenuList::pick`，不在 `ChatApp` 里加 `*_open`。

## 五、真要接的时候会踩的坑

1. **凭据明文**：`ProviderProfile.api_key` 已经是明文存 `settings.json`（`apps/desktop/src/settings.rs:87`），
   `mcp.json` 里的 GitHub PAT 也是明文（`catalog.rs:119`）。别再照抄这个模式——优先 `gh auth login`
   （凭据交给 Git Credential Manager），代码里只读 `gh auth status` / `gh auth token`。
2. **git 不能弹窗**：`plugin_git::git()` 设了 `GIT_TERMINAL_PROMPT=0` / `GCM_INTERACTIVE=never`，
   私有库克隆失败只会报「先在命令行配好凭据」。接入时要把一键 `gh auth login` 的引导补上。
3. **「自动」模式信任边界**：`auto.rs:40` 的信任边界写着「工作目录和它的 git remote」。
   远端 PR 上的代码不该算可信，切到 GitHub 项目时要往 `AutoModeConfig.environment` 补一句。
4. **`hard_block` 管不到远端**：`PROTECTED_DIRS`（含 `.git`，`tools.rs:1053`）只对本地路径生效。
5. **stdio MCP 仍在本地起**：远端会话里本地 MCP 的路径不通，只能走远程 MCP 或在远端起。

## 六、决定

- **不做 C**（云端执行 / Codespaces）。理由见第一节。
- **A / B 暂不排期**，需要时按第四节做，不需要重新调研。
- 想「在 GitHub 上开发」时，当前的可用路径是：本地目录 + `github` MCP（或 `gh` 命令），
  推分支和开 PR 交给模型。
