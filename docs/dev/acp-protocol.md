# ACP 协议手册：外部 agent 通道、工具内容、登录与结构化提问

> 本文件从根 `AGENTS.md` 拆出（2026-11）。改 `crates/core/src/acp.rs` 或界面里的
> ACP 相关面板（登录、提问、子任务）前先读这里。条目逐字搬过来。

- **ACP 工具内容与文件委托**：`content` 和 `rawOutput` 是两条通道，分别落到 `ToolCallContent` 和
  `ToolCallUpdated`，不能让后到的原始输出覆盖资源文本；图片走 `ToolCallImages` + 资源表，Diff 走
  `ToolCallDiff` 并持久化。带 Diff 的工具卡也要画结构化内容和图片，图片数量进行高指纹。
  `fs/read_text_file` 必须实现 `line/limit` 并返回真实错误，写失败不能回成功；图片提示按握手的
  `promptCapabilities.image` 放行或在发送前明确拒绝。资源链接只展示，不自动请求；实时终端与音频播放尚未实现。
- **ACP 配置与早到通知**：`config_options` 保留分组和稳定选项 ID，切换走 `session/set_config_option`，
  模型菜单只显示 agent 上报的选项；`usage_update` 的 used / size / cost 不进本轮 token。
  `available_commands_update` 接 `/` 补全（Skills 原样发给 agent，不做本地展开），晚到命令要刷新已打开的面板。
  **在 `Started` 之前到达的通知先在 `AcpShared::early` 缓存，绑定后按序回放**，不能按 sessionId 找不到会话就丢。
  压缩按 `compactionId` 原位更新。
- **ACP 登录与结构化提问**：`auth_required` 在输入框上方显示登录面板，Agent 方法走 `authenticate`，成功后重试原会话；
  Terminal 方法另起进程并等待退出（Windows `start /wait`，Linux `x-terminal-emulator`；macOS 明确报不支持），不读凭据，等待不阻塞其他会话。
  `elicitation/create` 的 form 模式复用提问面板，但走 `QuestionAnswer::Form`，保留字段 ID 和文本 / 浮点 / 整数 / 布尔 / 字符串数组类型；
  多选显式确认，文本和数字逐题暂存、校验后整表提交，不能沿用自研 `ask_user` 的 `Choices` 语义。URL 模式与实时终端仍未实现。
- **外部 agent 延迟与回收**：空白 ACP 会话不启动进程，首次发送存 `autoprompt`，`Started` 后自动发。
  仅当 agent 握手声明 resume / load、所有会话 `Session::idle_detachable`、无后台任务且 10 分钟无活动时，
  `reclaim_idle_processes` 才关闭；builtin 的 MCP 回收走同一个周期入口。
