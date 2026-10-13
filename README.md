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
  - [ ] 用真实 agent 跑通一次完整任务（需先登录 Claude 凭据）
  - [x] 权限审批 UI：agent 请求权限时界面弹卡片（允许 / 拒绝），内核挂起等待；`ACP_AUTO_APPROVE=1` 可退回自动批准
  - [x] 权限模式下拉接 `session/set_mode`（真实适配器实测接受）
  - [x] 修复：提示词不再阻塞指令循环，一轮进行中「停止」可即时生效
  - [ ] Windows job object 防孤儿进程（目前靠 stdin 关闭退出，强杀应用后实测无残留）
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
