# 网关与压测手册：用量网关、内存 / 帧率压测、独立验收包

> 本文件从根 `AGENTS.md` 拆出（2026-11）。动用量网关、`tools/*bench*.mjs`、
> 压测或验收脚本前先读这里；发布前按这里的开关跑一遍。

## 性能压测

- `CLAUDESKTOP_STRESS=<行数>` 逐帧滚动出帧间隔和每帧工作量（`work`，render 到画完）；
  再加 `CLAUDESKTOP_STRESS_BUSY=1` 改成置忙 5 秒（末尾挂着思考中和跑着的工具），看转圈常驻时每秒重画几次、每帧工作量（CPU 在外面量）；
  加 `CLAUDESKTOP_STRESS_TYPE=1` 改成逐帧往输入框打字；加 `CLAUDESKTOP_STRESS_STREAM=1` 改成逐帧推
  `AgentMessageChunk`（末尾另报多少个 chunk 真正并进正文，合批后远小于 chunk 数）；
  加 `CLAUDESKTOP_STRESS_VIEW=settings:N` / `home` /
  `requests`（`requests-all` 全部列出）改成逐帧滚设置页 / 首页 / 「请求」页签（`<行数>` 是假请求的轮数）；
  `CLAUDESKTOP_STRESS_SESSIONS=<会话数>`（配 `CLAUDESKTOP_STRESS_ROUNDS`，默认 3）测冷会话回收：
  生成 N 个历史会话（每个约 1 MB 正文）逐个激活跑几轮，结束后等 70 秒看冷态稳态再自动退出，
  配 `CLAUDESKTOP_MEM_REPORT=1` 和 `CLAUDESKTOP_DISABLE_RECLAIM=1`（关回收跑基线）做 A/B。
  动画 / 重画范围、设置页和首页的改动前后各跑一次。启动 / 历史读取 / 用量索引 / MCP 连接 / 空闲回收计时用
  `CLAUDESKTOP_BENCH_STARTUP=1`；内存用 `tools/memwatch.ps1 -Tree`，主进程和进程树分开记；
  `CLAUDESKTOP_MEM_REPORT=1` 让 30 秒回收周期打一行 `[mem]`（各会话显示数据保留字节、
  资源登记表条数 / 字节 / 挤到磁盘缓存的条数，冷会话回收时另有 `session_evict` 行）。
  无头测量要保持窗口时用 `CLAUDESKTOP_SCREENSHOT_HOLD_MS=<毫秒>`（仅截图构建）；手动绘制必须 `window.draw(cx).clear(cx)` 清掉帧 arena。
  自研休眠完整验收：release + `headless-screenshot` 后跑 `node tools/hibernate-bench.mjs --mode=all`
  （默认 50 会话、3 轮交替 A/B + 1800 秒混合；`--mode=smoke --sessions=4` 快查）。隔离数据 / 本地模拟端点 / 隐藏窗口，
  `CLAUDESKTOP_HIBERNATE_BENCH` 是仅截图构建可用的文件控制入口；采样按精确 PID，PB 峰值读 Windows 保存值，原始结果在 `.claudesktop/bench/`。
  界面回收 A/B 加 `--compare=display --mode=ab --sessions=50`（200 会话同理）；两边都开内核休眠，只切界面回收。长测可 `--resume` 复用已完成组。

## 用量网关

- 网关在 `crates/builtin/src/gateway.rs`：provider 经 `gateway::route` 发请求，开着（`ProviderConfig::gateway`）就转给本地网关、
  每个请求记一行 `gateway/<日期>.jsonl`（按内核会话 + 第几轮归组，首页「请求」页签读它，见 `requests.rs`；费用按设置里的单价表估算，见 `pricing.rs`，分峰谷价的模型在单价行尾写 `x2`，峰时段用 `@peak` 行按本地时间或指定时区判定）。新的请求出口（新 provider、新接口）也要走 `route`，内核里新的请求用途用 `gateway::tag` 标上。
  ACP agent 可选经网关：起进程时设 base URL（`gateway::agent_env`，端点编码在 `/via/` 路径里），轮次靠 `gateway::note_turn`。
  请求体和响应原文另存（`request_bodies.rs`，日志行 `body` 指过去），在用量「请求明细」的行尾点「原文」（`app/raw_request.rs`）。
  请求与返回分开读，返回首次切入才加载。请求按 gzip 尾部的有界大小提示预留缓冲。大请求可按顶层数组逐项排版，复杂形状走整包解析并复用解压缓冲。
  小正文用只读 `Editor`，大正文在后台建字节索引，分块后用变高列表按视口排版。
  分块不改原文，查找按完整正文的字节位置跳转，复制与导出取全文；切页释放列表测量状态并保留位置。
  请求统计走 `gateway/request_stats.rs`，只保留所需字段、不展开整包 `Value`；跳过正文时仍校验完整 JSON，保留重复键和字段类型容错。
  Windows 本地网关的已接受连接在 `gateway/socket.rs` 设 1 MiB 接收缓冲，减轻大请求的发送阻塞；设置失败退回系统默认，不能算零内存成本（系统 TCP 缓冲上限每连接比本机默认多约 960 KiB）。
  内核 / 普通隐藏桌面 A/B 分别用 `tools/gateway-memory-bench.mjs` / `gateway-desktop-bench.mjs`，保存原文的开关单独对照。
  性能测量加 `node --expose-gc`（只清测试控制进程）；请求校验走旁路 worker 和可转移缓冲池，不阻塞模拟响应。
  诊断可加 `--observer=idle` 把完整校验移到轮后（内核需带握手入口的测试构建）；解析交替对照用忽略式测试 `compare_parse_latency`，不能替代端到端延迟门槛。
  模拟端点的 Node 读取水位与内核 TCP 缓冲分开测；`--receive-buffer` + `--tcp-buffer-helper` 只调本次测试端点并逐连接读回验证，改变条件后先做 A/A，旧失败记录保留。`--precise-timers=1` 仅给测试控制进程申请 1ms 定时精度（退出恢复），不改产品进程。
  独立验收包走 `tools/gateway-validation.mjs`（冻结 Node / exe、逐文件校验、同机仅允许 smoke）；`--comparison=aa` 要求 exe 哈希相同、延迟绝对差 ≤20ms，不套内存收益条件；A/B 的原门槛不变。CI 手动给 `gateway_asset_id` 才切到独立验证。

## 错误文案与网关周边

- 读用户手写的 JSON（mcp.json、插件清单）出错时用 `claudesktop_core::json_error_text` 转成「第几行第几列：什么问题」的中文；
  git / 进程 / 网络的常见报错同样先译成能照着做的话（见 `plugin_git::explain_git_error`、`mcp::spawn_error`、`mcp_http::send_error`）。

## 请求历史共享与内存对照

- **请求历史共享**：`ChatMessage` 按消息共享 `Arc`，修改时写时复制；临时提醒和图片占位只改请求快照。
  三种 provider 借用历史内容直接序列化，公共出口在 `crates/builtin/src/request.rs`，`extra` 仍最后覆盖。
  请求内存 A/B 用 `tools/request-memory-bench.mjs` 对比两个 release 测试二进制，结果只代表内核测试进程。
