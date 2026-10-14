# claudesktop

Claude 风格高性能桌面 harness（Rust + GPUI），内核可更换。

- 调研报告：[调研报告-GPUI-Claude风格桌面Harness.md](./调研报告-GPUI-Claude风格桌面Harness.md)
- 技术栈：`gpui-kit 0.7`（UI，内含 pin 版 GPUI）+ `agent-client-protocol`（内核边界，M1 引入）+ 自研 Rust 内核（进程内）

## 里程碑

- [x] **M0（核心已过）** 走通框架：窗口 + 中文渲染 + IME（微软拼音全绿：候选框跟随/组词 Enter·Backspace 正常/无豆腐块；进程内存 ~103MB）
  - [ ] 开放项：搜狗拼音实测（报告预判存疑，不影响推进）；1 万条虚拟列表滚动压测；高 DPI/多显示器
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
- [ ] **M2** 自研内核（OpenAI 兼容 provider → Anthropic SSE）
- [ ] **M3** 双内核收口：同一 UI、同一任务、两个内核

## 开发

```bash
cargo run -p claudesktop
```

## 结构（随里程碑生长）

```
apps/desktop   # UI 壳（gpui-kit）
crates/core    # 会话编排层：Kernel trait + KernelEvent（M1）
crates/kernels # builtin（进程内）/ acp（子进程）（M1-M2）
```
