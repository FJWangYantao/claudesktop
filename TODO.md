# 待办

还没开始做的方向，**从简单到难**排好了。动手前先在这里补上设计要点，做完删掉对应条目。
标了文档的，方案已经写清（含取舍和测试），照着做即可，不用重开调研。

难度是按三条一起估的：**文档写到什么程度**（有没有逐节的做法和测试清单）、**要不要动前置**、
**改动面有多大**（一个文件 / 一个子系统 / 内核 + 界面一起 / 架构级）。

## 零、已知 bug（要修）

- [x] **一轮跑着的时候 `/compact` 被吃掉**（小）：`ChatApp::compact_session` 开头 `if s.busy { return }`
  一声不响就退出，而命令面板 / 补全选中时已经把输入框里的 `/compact` 换掉了，所以看起来是「发了、什么也没发生」。
  现在和发消息一样排队：`Session::compact_queued`，`clean_end` 时由 `queued_compaction` 在 `dispatch_queued`
  **之前**压（压缩自成一轮），输入框上方那行队列文字也标出来、可以「不压了」；
  用户停下 / 这一轮出错时撤掉并在对话里说明一句。

## 界面打磨（对照 Claude 前端 CSS，见「调研报告-Claude前端CSS手法对照.md」）

只列**核对过 gpui 能力**、确定能做的。色彩 / 动效 / 组件契约已经对齐，别重抄。
每条的 gpui 实现要点、换算和「为什么不能照抄 CSS」都在报告里。
按什么顺序做、每步改哪几行、怎么验收，见「方案-界面打磨.md」。

**2026-10 这一批（1～7 全部落地）**：焦点环三层（`theme::focus_ring`）、块间距 / 侧栏 / 导航 / 顶栏改 rem、
滚动渐隐三档跟手 + Oklab、菜单面板交给 gpui-kit 的弹层外观、`role` / `aria-*`、60ms 悬停令牌、
`float_shadow` / `raised_shadow` / `state_line`、拖放高亮两层。方案里第 6、7 步的素材类（空状态配图、
外观预览、提示音）还没做。

1. [x] **焦点环升级成三层**：`theme::focus_ring(accent)`，输入框聚焦时换成它；按钮类用 gpui-kit
   自己的 `focus_ring_style`（它按 `focus_visible` 判键盘）。
2. [x] **块间距改成跟字号缩放**：`STEP_GAP` / `BLOCK_GAP` / `STEP_INDENT` 改 `Rems`（`ICON_COL` 留 px）。
3. [x] **侧栏 / 设置页导航 / 顶栏宽度改 rem**：`SIDEBAR_W` / `SETTINGS_NAV_W` / `CHROME_H`；
   阅读列 / 设置页内容 / 概览仍 px（measure）。整页宽度用 `theme::settings_page_w()`。
4. [x] **滚动渐隐跟手**：`session::ScrollEdge` + `scroll_handle_edge`，三档（贴顶 / 中间 / 贴底），
   侧栏档位并进了 `sync_sidebar` 的指纹。
5. [x] **菜单面板改用弹层外观**：`menu_panel` 只留 `track_focus` + `p_1`，外观交给 Popover 的 `appearance`。
6. [x] **补 `role` / `aria-*`**：侧栏会话行、组头 / 思考头（`aria_expanded`）、菜单条目、`MenuChip`、权限卡。
7. [x] **渐隐条换 Oklab 插值**：`widgets::edge_fade` 和侧栏那条都加了 `.color_space(ColorSpace::Oklab)`。

**留后（不急）**：密度轴（只改间距不改字号）；输入框单独一档圆角（原版 20px，现在 16px）；
素材类（空状态 200×200 配图、外观选项现画的迷你窗口预览、权限请求提示音）。

## 一、单个子系统（一天上下）

1. [x] **插件市场补齐**（小～中，约一天）：推荐项里指向别的仓库的条目（`EXTERNAL_PLUGIN_PICKS`，按市场清单里的
     `ref` 钉版本）、按版本号装 / 换版本（`plugin_git::install_at` / `versions` / `checkout`，插件卡片上的版本 chip）。
     见「设计说明-插件.md」。

## 二、跨层改动（内核 + 界面一起动）

1. [ ] **向用户提问的 `ask_user` 工具**（中，约一天）：模型问一句话 + 2～4 个选项，界面上出选项卡片，
     用户点一下就是工具结果，这一轮继续往下跑（现在只能在回复里问，模型常常自问自答）。
     新开一条 `KernelEvent::UserQuestion` + `Kernel::resolve_question`，**不复用权限请求**（「自动」模式的分类器会代答）；
     卡片照 `render_permission` 画，键位复用 `AnswerPermission1..4`。方案见「设计说明-用户提问.md」（第十一节四件事先拍板）。

2. [x] **逐块审查第 2 期**（约 1.5 天）：自研内核授权时按块部分同意——权限卡上的 diff 按块勾选，
     工具结果写明只写了哪几块。块的划分和逐块审查共用 `diff::hunks_with_context`（`review::hunks` 就是它），
     按块拼内容用 `diff::splice_ranges`。见「设计说明-逐块审查改动.md」第十节。
3. [ ] **桌面之外：远程连接 / 定时任务 / Linux TUI**（较难，约 9～12 天）：三件事共用一层「无头会话层」
   （把 `apps/desktop/session.rs` 的状态机 + `ChatItem` 搬进 `crates/core`，去掉 `SharedString` / `ListState`），
   然后在它上面加三个壳：`claudesktop-cli run` + 系统计划任务、`claudesktop-cli serve`（默认只监听回环，远程靠 SSH 隧道）、
   `ratatui` 的 `claudesktop-tui`。方案、成本和要拍板的六件事见「想法-桌面之外.md」。
   排期：地基 2～3 天 → 定时任务 1.5～2 天 → serve 1 天 → TUI 3～4 天 → TUI 接 serve + token 2 天。

## 已完成（2026-11 这一批）

`/compact` 排进队列；插件市场补齐（别的仓库的条目、按版本装 / 换版本）；逐块审查第 2 期（授权时按块同意）；
聊天与 Code 分离（第 1～8 步：会话类型、公式、图片、聊天转 Code、回复里的网络图片）。

## 三、大改（多天）

4. [x] **聊天与 Code 模式分离**（最难，已完成）：纯聊天（不碰文件、不跑命令、不需要工作目录）和写代码（带工作目录、工具和权限）
     分成两种会话，各自有合适的默认设置和界面。会话类型、内核起法（聊天不带工具）、首页和侧栏的组织方式都做了；
     公式渲染、图片发送（含网络图片的占位卡与「自动加载」开关）也一并落地。
     方案见「设计说明-聊天与Code分离.md」（第 1～8 步全部完成）。

## 已否决（别再从头调研）

- **云上连 GitHub 开发**（Codespaces 式：代码、shell、读写文件全在远端跑，本地只当客户端）：不做。
  GitHub 没有给第三方 harness 的通用远端执行接口，只能 `gh codespace ssh` / `cp` 硬凑；
  且本地直连盘假设贯穿工具层、检查点、逐块审查和 `hard_block`，要改就得先做一层 `Workspace` 抽象。
  详见「调研报告-GitHub接入.md」。
  真要做「在 GitHub 上开发」，走本地目录 + `github` MCP（或 `gh` 命令）就够；
  想省掉手动克隆再加一个「克隆仓库…」入口（同报告第四节，1～2 天）。
  **注意**：上面第 3 条里的「远程连接」是「远端跑、本地看」（工作目录仍是那台机器的本地盘），不是这一条，不冲突。

## 分期里明确留后的（不排期，别重开调研）

- 检查点：shell 命令改的文件不覆盖（第 3 期，做成设置项的前提是每轮隐藏 git 快照）；`撤销回退`；
  ACP 会话的「回退对话」（或「从这里分叉一个新会话」）。见「设计说明-检查点与回退.md」第八节。
- 配色二期：`<数据目录>/themes/*.json` 用户自定义色板；代码块按各套原版的语法色。见「设计说明-多套配色.md」第五节。
- 用量网关：把重试挪进网关（「一次请求」和「一条日志」的对应关系要重新定义）、让网关改请求体 / 限速（另一种产品形态）。
  见「想法-用量统计网关.md」第七节。

## 已完成

测试 / 截图默认在后台跑；用量明细可看原始请求；`PreCompact` 钩子；权限「总是拒绝」按项目落盘（命中不问直接拦）；
MCP 工具结果里的图片进模型；token 估算换准；`web_search` 工具；会话标题模型起名；
Anthropic 思考预算与服务端工具；OpenAI Responses API；`shell` 后台任务三件套；历史会话全文搜索；
项目级配置（内核侧 + 设置页「项目」）；记忆系统第 1 期（`memory.json` + `memory_write` + 设置页「记忆」）；
记忆系统第 2 期（预算截取与 `used_ms` 淘汰、`<memory-context>`、`memory_search` / `memory_forget`、界面新增 / 编辑 / 置顶 / 来源跳转）；
记忆系统第 3 期（回合结束提炼候选，设置页顶部「待确认」收下才落盘，开关默认关；见 `crates/builtin/src/remember.rs`）；
权限「自动」模式（分类器代理审批，见 `crates/builtin/src/auto.rs`，设置页「内核与模型」里配）；
项目根认 worktree（`.git` 是文件也算）+ `project::shared_root` / `shared_dir`：项目记忆和权限规则跨 worktree 共享；
git worktree 隔离（新建会话可选「在新 worktree 中开始」，独立分支干活，分支 chip 里合并 / 删除；
见「设计说明-worktree.md」）；
思考强度档位（关 / 低 / 中 / 高 / 极高，按模型能力表翻译成 `thinking` / `output_config.effort` / `reasoning_effort`，
见「设计说明-思考强度.md」）；
MCP 远程服务 OAuth 登录（发现 + DCR + PKCE、刷新与 401 去重、手填配置、设置页「登录 / 清除登录状态」，
401 文案先说点登录）；逐块审查改动第 1 期（最新一轮逐块撤回 / 恢复）；每轮结束时猜下一句（灰字，Tab 接上）；
多套配色（六套色板各带浅深两版，「外观 · 配色」切换；见「设计说明-多套配色.md」）。
