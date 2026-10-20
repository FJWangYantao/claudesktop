# 待办

还没开始做的方向，**从简单到难**排好了。动手前先在这里补上设计要点，做完删掉对应条目。

1. [ ] **桌面之外：远程连接 / 定时任务 / Linux TUI**（较难，约 9～12 天）：三件事共用一层「无头会话层」
   （把 `apps/desktop/session.rs` 的状态机 + `ChatItem` 搬进 `crates/core`，去掉 `SharedString` / `ListState`），
   然后在它上面加三个壳：`claudesktop-cli run` + 系统计划任务、`claudesktop-cli serve`（默认只监听回环，远程靠 SSH 隧道）、
   `ratatui` 的 `claudesktop-tui`。方案、成本和要拍板的六件事见「想法-桌面之外.md」。
   排期：地基 2～3 天 → 定时任务 1.5～2 天 → serve 1 天 → TUI 3～4 天 → TUI 接 serve + token 2 天。

## 已否决（别再从头调研）

- **云上连 GitHub 开发**（Codespaces 式：代码、shell、读写文件全在远端跑，本地只当客户端）：不做。
  GitHub 没有给第三方 harness 的通用远端执行接口，只能 `gh codespace ssh` / `cp` 硬凑；
  且本地直连盘假设贯穿工具层、检查点、逐块审查和 `hard_block`，要改就得先做一层 `Workspace` 抽象。
  详见「调研报告-GitHub接入.md」。
  真要做「在 GitHub 上开发」，走本地目录 + `github` MCP（或 `gh` 命令）就够；
  想省掉手动克隆再加一个「克隆仓库…」入口（同报告第四节，1～2 天）。
  **注意**：上面第 1 条里的「远程连接」是「远端跑、本地看」（工作目录仍是那台机器的本地盘），不是这一条，不冲突。

## 已完成

测试 / 截图默认在后台跑；用量明细可看原始请求；`PreCompact` 钩子；权限「总是拒绝」按项目落盘（命中不问直接拦）；
MCP 工具结果里的图片进模型；token 估算换准；`web_search` 工具；会话标题模型起名；
Anthropic 思考预算与服务端工具；OpenAI Responses API；`shell` 后台任务三件套；历史会话全文搜索；
项目级配置（内核侧 + 设置页「项目」）；记忆系统第 1 期（`memory.json` + `memory_write` + 设置页「记忆」）；
记忆系统第 2 期（预算截取与 `used_ms` 淘汰、`<memory-context>`、`memory_search` / `memory_forget`、界面新增 / 编辑 / 置顶 / 来源跳转）；
记忆系统第 3 期（回合结束提炼候选，设置页顶部「待确认」收下才落盘，开关默认关；见 `crates/builtin/src/remember.rs`）；
权限「自动」模式（分类器代理审批，见 `crates/builtin/src/auto.rs`，设置页「内核与模型」里配）。
