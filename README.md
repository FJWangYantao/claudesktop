# claudesktop

Claude 风格高性能桌面 harness（Rust + GPUI），内核可更换。

- 调研报告：[调研报告-GPUI-Claude风格桌面Harness.md](./调研报告-GPUI-Claude风格桌面Harness.md)
- 技术栈：`gpui-kit 0.7`（UI，内含 pin 版 GPUI）+ `agent-client-protocol`（内核边界，M1 引入）+ 自研 Rust 内核（进程内）

## 里程碑

- [x] **M0（核心已过）** 走通框架：窗口 + 中文渲染 + IME（微软拼音全绿：候选框跟随/组词 Enter·Backspace 正常/无豆腐块；进程内存 ~103MB）
  - [ ] 开放项：搜狗拼音实测（报告预判存疑，不影响推进）；高 DPI/多显示器
  - [x] 1 万条虚拟列表滚动压测：对话区改用 GPUI `list`（只布局可见行，按行指纹只重测变化的行），1 万行（用户消息 / markdown / 工具行混合）上下滚动 300 帧，帧间隔与空会话相同（p50 33ms，受压测驱动的定时器粒度限制），内存 +12MB；`CLAUDESKTOP_STRESS=10000` 复现
- [ ] **M1（进行中）** Kernel trait + KernelEvent（复用 ACP 类型）+ AcpKernel 驱动真实 agent
  - [x] 桌面端接入 AcpKernel：事件流 → 消息 / 思考 / 工具行 / 计划；默认 mock agent，`CLAUDESKTOP_KERNEL=demo` 退回演示回放
  - [x] 真实适配器握手通过（`ACP_AGENT="cmd /c npx -y @agentclientprotocol/claude-agent-acp"`，Windows 实测；未发提示词）
  - [x] 用真实 agent 跑通一次完整只读任务：pi（`ACP_AGENT="cmd /c npx -y pi-acp"`，模型由 pi 配置决定）经同一套 UI 完成「列目录 + 读 README + 总结」，未改动任何文件
  - [x] Claude Code 适配器同样跑通：只读任务（ls + 读 README）与写文件任务（Manual 模式弹出权限卡片，点「Yes」后在隔离目录创建了文件）均成功
  - [x] 多会话：一条连接上开任意多个 ACP 会话（事件带 sessionId 路由），界面每个会话各自的对话 / 忙闲 / 模式 / 模型；「新会话」真正发 `session/new`；pi 与 Claude 适配器均实测支持
  - [x] 下拉与模型名由 agent 上报：Claude = 权限模式，pi = 思考强度；pi 启动时推送的 Skills 列表折叠为「启动信息」
  - 注：pi 没有逐项权限审批（工具直接执行），只读性靠提示词约束；`CLAUDESKTOP_AUTOPROMPT` 可指定自动发送的提示词
  - [x] 权限审批 UI：agent 请求权限时界面弹卡片（允许 / 拒绝），内核挂起等待；`ACP_AUTO_APPROVE=1` 可退回自动批准
  - [x] 权限模式下拉接 `session/set_mode`（真实适配器实测接受）
  - [x] 修复：提示词不再阻塞指令循环，一轮进行中「停止」可即时生效
  - [x] 会话持久化：每会话一份追加式事件日志（`%APPDATA%\claudesktop\sessions\<id>.jsonl` + 元信息 `<id>.json`；`CLAUDESKTOP_DATA_DIR` 可改位置），流式块合并后写入；启动时重放恢复，侧栏按工作目录分组并显示最近活动时间；演示回放模式不落盘
  - [x] 恢复的会话续聊：在历史会话里发消息时先续接内核会话——优先 ACP `session/resume`，其次 `session/load`（agent 重放的历史直接丢弃，本地已有），都不支持或失败则新开会话并提示「看不到之前的上下文」；仅当记录的 agent 命令与当前一致时才尝试续接。mock 用 `MOCK_RESUME=resume|load` 模拟两种能力
  - 注：真实适配器（Claude / pi）的续接能力尚未实测
  - [x] Windows job object 防孤儿进程：启动时把本进程放进 `KILL_ON_JOB_CLOSE` job，子孙进程自动继承；强杀应用后实测 `cmd → powershell Start-Sleep`（不读 stdin）整棵树被回收
- [x] **M2** 自研内核 `crates/builtin`（OpenAI 兼容 provider；Anthropic 原生接口不做，需要时经兼容网关接入）
  - [x] OpenAI 兼容流式对话：正文 / 思考（`reasoning_content` 或 `reasoning`）/ 用量；429·5xx 在出字前退避重试（1s、3s）；取消即时生效且之后不再发事件；多轮历史
  - [x] 桌面端接入：`CLAUDESKTOP_KERNEL=builtin`，配置走 `CLAUDESKTOP_BASE_URL` / `CLAUDESKTOP_MODEL` / `CLAUDESKTOP_API_KEY`
  - [x] 真实端点实测：Antigravity Tools 本地网关（`http://127.0.0.1:8045/v1`）的 `gemini-3.8-flash`，单测与桌面端均跑通
  - [x] 工具调用循环：`read_file` / `list_dir` / `write_file` / `edit_file`（精确替换，兼容 CRLF）/ `shell`（Windows 用 pwsh，超时、取消连子进程一起结束）；一轮最多 50 步
  - [x] 权限：读工具直接放行；写和命令按模式（Manual / Accept edits / Plan 只读 / Bypass）→ 已保存规则 → 弹卡片询问（允许一次 / 总是允许 / 拒绝）。「总是允许」按项目存到数据目录的 `permissions.json`（编辑 = `edit`，命令 = `shell:<程序> [子命令]` 前缀，复合命令不提供）；工作目录外的写入永远询问
  - [x] 工具循环实测：gemini-3.8-flash 读 → 写（弹权限）→ 回答，桌面端工具行与权限卡片正常
  - [x] 历史会话续聊：用本地日志重建上下文（工具调用以 `[tool] 标题 → 状态：摘要` 行并入，总量超 20 万字符从最早的轮次丢）；换了端点 / 模型也能续接；上次没完成的工具调用恢复为「已中断」
  - [x] 改文件显示 diff：`write_file` / `edit_file` 执行前算出改动预览，挂在工具行下（先于权限卡片，看着 diff 决定是否放行）；ACP agent 上报的 `ToolCallContent::Diff` 走同一条路径。事件和日志里只存带 3 行上下文的 hunk（单份最多 400 行），界面默认显示前 40 行可展开
  - [x] 设置页（侧栏「设置」）：选内核（自研 / ACP agent / 演示回放），自研内核填端点、API Key、模型，可一键获取端点的模型列表，检测到 Antigravity Tools 时可直接导入它的本地网关；ACP 填启动命令。保存到数据目录的 `settings.json`，「保存并应用」时重启内核，有对话的会话下次发消息时自动续接。启动时环境变量优先于设置文件
  - [x] 会话内切模型：输入框旁的模型名可点开，列出端点的模型，选中后当前会话下一步起生效，并记为新会话的默认模型
- [x] **M3** 双内核收口：同一 UI、同一任务、两个内核
  - [x] 内核按需启动、可同时在线：自研 / ACP / 演示回放各一个槽，每个会话绑定自己的内核（侧栏会话行标出「自研 / ACP」，底部列出每个已启动内核的状态）；改了某个内核的设置只重启那一个
  - [x] 输入框旁的内核下拉：空会话直接换内核（并记为新会话默认）；有对话的会话固定在原内核上，可把首条提示词放到另一个内核上新开会话重跑；用户消息悬停时也有「在 X 上重跑」
  - [x] 历史会话按记录的 agent 标识回到原来的内核续接（内核没启动就先启动）
  - [x] 实测：同一条提示词先在自研内核（gemini-3.8-flash）跑完，再一键放到 ACP（mock agent）上重跑，两个会话并排、各自的事件互不串
- [ ] **打磨**（M3 之后，把界面上剩下的演示数据换成真的）
  - [x] 首页用量概览接真实数据：每轮结束时日志里记一条 `turn`（时间、模型、输入 / 输出 token），启动时汇总全部会话；会话数 / 消息数 / Token 总量 / 活跃天数 / 高峰时段（本地时间）/ 最常用模型，可按全部 / 30 天 / 7 天筛选；热力图为最近 26 周（行 = 周一到周日）；「模型」页签列出各模型的轮数和 token。旧日志没有 `turn` 记录的按会话更新时间计、模型从内核标识里取
  - [x] 设置页列出已保存的「总是允许」规则（按项目分组），可逐条删除；内核每次判断前重读 `permissions.json`，删除不用重启即生效，写入也先读后改不会覆盖
  - [x] 侧栏：写死的「任务」换成真实的「进行中」（所有正在跑 / 等授权的会话，标出内核，点击切换）；会话列表可滚动（之前超出高度会被截掉）
  - [x] 会话重命名 / 删除：侧栏会话行悬停出现按钮；重命名回车或点别处提交，写回元信息；删除先确认，停掉进行中的一轮并删除日志与元信息（用量统计同步去掉），删光时自动补一个新会话
  - [x] 顶栏的模型名也能点开模型下拉（与输入框旁的是同一个，锚在顶栏下方）
  - [x] 自研内核新增只读搜索工具 `glob`（按文件名 / 路径模式找文件，按修改时间排序）与 `grep`（正则搜内容，可限定 include、忽略大小写、只返回文件名）：遵守 `.gitignore`、跳过隐藏 / 二进制 / 超过 2MB 的文件，结果有上限；Plan 模式可用；实测 gemini 会主动用它们代替 shell 搜索
  - [x] 对话区虚拟列表：长对话只渲染视口附近的行；自动跟随改为「在底部才跟随」，往上翻看历史时不再被拽回底部，发消息时重新跟随

## 开发

```bash
cargo run -p claudesktop                       # ACP 内核（默认 mock agent）

# 自研内核 + OpenAI 兼容端点（例：Antigravity Tools 本地网关）
CLAUDESKTOP_KERNEL=builtin CLAUDESKTOP_BASE_URL=http://127.0.0.1:8045/v1 CLAUDESKTOP_MODEL=gemini-3.8-flash CLAUDESKTOP_API_KEY=sk-... cargo run -p claudesktop --bin claudesktop

# CLAUDESKTOP_KERNEL 现在是「新会话的默认内核」；其他内核在会话用到时再启动

# 真实端点测试（默认忽略）
cargo test -p claudesktop-builtin -- --ignored
```

## 结构（随里程碑生长）

```
apps/desktop   # UI 壳（gpui-kit）
crates/core    # 会话编排层：Kernel trait + KernelEvent（M1）
crates/builtin # 自研内核（进程内）：OpenAI 兼容 provider、工具循环、权限规则（M2）
```
