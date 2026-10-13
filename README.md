# claudesktop

Claude 风格高性能桌面 harness（Rust + GPUI），内核可更换。

- 调研报告：[调研报告-GPUI-Claude风格桌面Harness.md](./调研报告-GPUI-Claude风格桌面Harness.md)
- 技术栈：`gpui-kit 0.7`（UI，内含 pin 版 GPUI）+ `agent-client-protocol`（内核边界，M1 引入）+ 自研 Rust 内核（进程内）

## 里程碑

- [x] **M0（核心已过）** 走通框架：窗口 + 中文渲染 + IME（微软拼音全绿：候选框跟随/组词 Enter·Backspace 正常/无豆腐块；进程内存 ~103MB）
  - [ ] 开放项：搜狗拼音实测（报告预判存疑，不影响推进）；1 万条虚拟列表滚动压测；高 DPI/多显示器
- [ ] **M1** Kernel trait + KernelEvent（复用 ACP 类型）+ AcpKernel 驱动真实 agent
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
