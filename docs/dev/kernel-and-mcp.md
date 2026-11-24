# 内核手册：自研内核、自动审批、MCP / 插件、记忆、思考强度、休眠

> 本文件从根 `AGENTS.md` 拆出（2026-11）。改 `crates/builtin/**`、`crates/core/**`
> 的内核侧逻辑前先读这里。条目逐字搬过来，只在根文件做索引。

## 自动模式与向用户提问

- **权限模式「自动」**（`crates/builtin/src/auto.rs`）：由一次独立的模型调用（分类器）代替用户审批。
  `authorize` 里 `Access::Read` 和工作目录内的编辑**不进分类器**（省一个往返），其余走
  「硬熔断（`tools::hard_block`：删关键路径、写受保护文件）→ 已保存规则 → 分类器」。
  分类器读取用户消息、提交的提问答案和待执行动作。答案带对应题目与实际选中的文案，标明题目和选项来自模型，
  其它助手文本和工具结果剥掉（防自我说服、防注入）；
  判不出来一律按拒绝。拒绝的理由连同「别换个马甲重试」回给模型，它自己决定换路子还是问用户。
  放行**不发事件**（不刷「已自动批准」）；拦下的理由并进那条失败的工具行（`Verdict::AutoDeny` →
  `ToolOutput::fail`），不另发 `Notice`——用户看到的就是这一行。
  连续 3 次 / 累计 20 次拒绝就停下来等用户（用户发消息或提交答案后恢复判定）。续接 / 回退时从有效日志重建
  分类器看的输入（`history::Rebuilt::user_inputs`）。没在设置里配好时这个模式不出现在模式列表里。
  设置里开着时，**新建的对话默认就是「自动」**（`ChatApp::new_session` → `Session::default_to`）：
  会话还没连上内核时先把这一项补进模式表（`model::auto_mode_entry`）并选中，`Started` 时走
  `Session::pending_mode` 交给内核（内核不认就退回它报的那个）；用户在下拉里改了就以此为准，
  内核重连（`detach_kernel`）时把当前模式重新挂上 `pending_mode`。
  会话模式 ID 存 `SessionMeta.mode`，切换时立即保存，续接的 `Started` 再交给内核；历史会话不套新对话默认值。
  自研会话在连接前就用 `builtin::permission_modes` 补齐菜单（自动是否可用仍看设置）；旧元信息没存模式时保留逐项确认。
- **向用户提问（自研 `ask_user`）**：在 `Inner::run_tool` 开头拦截，不走 `start_tool` / `authorize`——「自动」模式的分类器
  不能替用户答，也不发 `ToolCallStarted`（提问不占工具行）。事件是 `KernelEvent::UserQuestion`，应答走
  `Kernel::resolve_question`；`Inner::pending` 按 `Reply` 分权限 / 提问两种。界面在输入框上沿的提问面板
  （`composer.rs` 的 `render_question_panel`）：一次一题，点一个选项就进下一题、最后一题选完自动交给模型
  （没有「就这样」按钮）；「自己说」/「你来定」是面板底边的两个小链接。对话流里只留一行 `ChatItem::Question`
  （摘要模式也显示，`is_step` 为假），答完那一行写上所选的；`Record::QuestionAnswered` 记答案，`history::rebuild`
  折成 `[tool] ask_user → …` 一行。选项「自己说」/ 取消用 `ToolOutput::fail_stop` 让这一轮当场结束。
  子会话没有这个工具；聊天会话有。

## magpie 网关、MCP、插件

- **magpie 网关**（`crates/builtin/src/magpie.rs`，「设计说明-本机agent.md」第十四、十五节）：把本机订阅/key
  汇成 `http://127.0.0.1:<port>/v1` 的 Anthropic 端点，自研内核当普通档案用（`settings.rs` 的 `magpie_profile`，
  `ProviderProfile.magpie` 标记自动建的）。「设置 · 内核」页是**本机内核清单**（对齐 magpie 的 Agents 页）：
  检测到的 agent 列出、勾选即接入；新会话自动选择真实内核（`Settings::kernel`），不提供演示回放，旧演示设置迁回自动。
  「设置 · 服务商」页是单行来源清单，选中的 Profile 在对应行下展开；行内笔图标切换编辑，垃圾桶先弹「确认删除？」（点击阻止冒泡，确认时按输入框实体定位来源），新增来源自动进入编辑。
  Profile 的 API Key 只显示是否已设置；改动走页头「保存」。「设置 · 模型」只有一个跨来源的默认模型，
  选模型同时确定来源（`builtin.active` + 该档案的 `model`，兼容原设置）；思考下拉只改这个模型所属档案，不跟清单筛选走。
  清单默认「全部来源」，来源筛选和搜索不改变默认；行内「设为默认」执行选择，复选框决定进不进输入框菜单（`ProviderProfile.menu_hidden`）。
  手填模型先筛选一个来源；同名模型按来源区分。默认模型菜单带搜索、来源分组和滚动条；清单只渲染视口附近的固定行。刷新按输入框实体与连接参数核对归属。
  **所有来源的模型在输入框菜单都能挑**（按来源分组，挑别家的自动切过去 `choose_model_from` =
  改 `builtin.active` + 重启自研内核）。启动引导（`ChatApp::magpie_bootstrap`）：探活 `/api/hello`（用户自己开的直接复用，
  端口读它 settings.json 的 `port`）→ 自动建档案 + 拉 `/v1/models`；当前档案是 magpie 而网关没跑、sidecar 已下载 →
  `auto_start_magpie_if_active` 顺手起 serve（`magpie serve`，进程挂 `ChatApp::magpie_child`，job object 之外的显式兜底）。
  下载走 GitHub release（哈希校验；产物地址和 SHA256SUMS 必须同一次 API 应答里拿）。凭据读取全部发生在 magpie 里，我们不碰。
- MCP 客户端在 `crates/builtin/src/mcp.rs`（配置是数据目录的 `mcp.json`，格式同 Claude Code 的 `.mcp.json`；
  本地命令走 stdio，远程的 Streamable HTTP / 旧版 SSE 在 `mcp_http.rs`；连上过又断了的自动退避重连）。
  首次连接把工具定义按配置 / 插件版本指纹缓存到 `mcp-tools.json`；启动命中缓存时显示「按需连接」，
  先把工具给模型，首次调用再连接；手动刷新仍强制重连。10 分钟无调用且无进行中的调用时断开，缓存保留。
  工具定义用 `provider::ToolSpec` 共享；MCP 清单更新替换定义，进行中的请求保留原快照。`specs_for` 先按会话清单筛选再取引用，搜索用 `descriptions`，不复制 schema。
  三种 provider 借用工具定义序列化，保留字段顺序、Anthropic 缓存断点、Responses 的 `strict` 和 `extra.tools` 覆盖。
  工具以 `mcp__服务器__工具` 交给模型、走 `Action::Mcp` 和正常的权限流程，
  状态经 `KernelEvent::McpStatus` 到设置页「MCP」（`app/mcp_view.rs`）。改 MCP 用 `tools/mock-mcp-server.mjs` 测（`--http` / `--sse` 起远程模式，测试在 `tests/mcp.rs`）。
- 设置页「MCP」「插件」分「已安装 / 推荐」两栏带搜索（`app/catalog_view.rs`），推荐的内容在 `crates/core/src/catalog.rs`：
  MCP 推荐项点「添加」写进 mcp.json（`mcp::add_server`，还有 `remove_server` / `set_server_disabled`）；插件推荐项从官方市场
  稀疏克隆单个插件（`plugin_git::install_subdir`）并直接启用。新增推荐插件后跑一次 `cargo test -p claudesktop-core catalog -- --ignored`（联网真装一遍）。
- 插件读取在 `crates/core/src/plugin.rs`（数据目录 `plugins/`，格式兼容 Claude Code，说明见「设计说明-插件.md」；
  从 git 装 / 更新 / 卸载在 `plugin_git.rs`），
  启用状态在设置的 `plugins_enabled`，经 `Kernel::set_plugins` 交给内核。斜杠命令在界面 `submit_to` 展开
  （日志 `Record::User.sent` 存展开后的全文，重建上下文用它）；技能、子 agent、钩子只有自研内核接（`run_hooks`）；
  MCP 服务并进 `McpHub`，ACP 内核在开会话时经 `mcpServers` 交给 agent。

## 搜索、子 agent、待办与汇报

- 网页搜索后端在 `crates/builtin/src/websearch.rs`：`SearchBackend`（DeepSeek / Brave / DuckDuckGo，声明顺序就是固定降级顺序）
  按 `SearchConfig::order()` 依次试，失败 / 没配 key 就退到下一个，**实际用了哪个写进结果第一行**（工具行汇总带「降级过」）。
  选择存在数据目录的 `search.json`（`load()` 每次现读，改完即生效，不走 `Kernel::set_*`），**只存选择不存 key**：
  Brave key 在 `brave_key` 文件，DeepSeek 搜索的 key 在 `deepseek_key` 文件，都没有就用 provider 里 DeepSeek 官方端点（host `api.deepseek.com`，中转站不算）档案那把
  （`Settings::deepseek_search_key` → `ProviderConfig::deepseek_key` → 内核起时 `websearch::set_key_hint`）。
  默认 Brave + DuckDuckGo（和加 DeepSeek 之前一样）；DeepSeek 按次计费，只看 `allow_deepseek` / `prefer`，**有 key 不算允许**。新的搜索后端 = 加一个枚举值 +
  `order()` 里的开关 + `run()` 里一个分支，**不要做成 MCP**（见「设计说明-DeepSeek官方搜索.md」）。
- 自研内核默认不打扰前台：系统提示词里的 `BACKGROUND` 要求测试 / 浏览器 / 截图无头跑，`tools::QUIET_ENV` 给命令带上
  不弹窗口的环境变量（用户自己设了的不覆盖）；推荐的浏览器类 MCP 带 `--headless`。
- 用量：首页概览（`app/overview.rs`）的统计卡、热力图格子、模型行和右上角箭头都跳到用量页（`app/usage_view.rs`，
  一整页不分页签：总数、趋势、模型 / 会话排行、时段、请求明细，时间范围和首页共用；画在设置页的位置上但不带分类导航）。
  点一天 / 一个模型 / 一个会话是给整页加筛选（存在 `RequestsView` 的 `day` / `model` / `session`），汇总在 `usage.rs` 的 `board`；
  费用、缓存命中从网关日志算（`request_costs`）；`requests::costs` 每种模型匹配一次单价，一次遍历更新总额、模型和会话。
  跑着的轮也要算进去：读用量统计用 `ChatApp::usage_stats`，不直接读 `usage`。网关日志在报用量、一轮结束时合批按偏移补读（`refresh_requests_soon`），
  日志删改、切时间范围或点「重新读取」时全读。补读保留旧下标，`requests::Groups` 只更新涉及的回合，按发出时间排列（允许迟到）。
  连续的请求下标存为区间。日志逐行读取，缓冲按剩余字节分配至多 64 KiB。解码时借用 JSON 行里的短字符串，
  和上一条保留记录的同一字段相同就复用，否则查有界共享表。加载占位时量请求明细的位置，若仍在视口和预排区下方，首帧仅占位；
  列表排出来后测实际位置与行高，首页和用量页切换时重新量位置。请求与归组两个数组占用合计达到 16 MiB 时，离页约 30 秒后释放明细缓存。
  保留范围、筛选和展开键，返回按当前范围后台全读。新的用量入口一律走 `ChatApp::open_usage`。
- 子 agent 在 `crates/builtin/src/subagent.rs`：`task` 工具开子会话（ID `主会话/调用 ID`），和主会话共用 `run_steps`；
  内置角色 `general-purpose` / `explore`（只读），何时派写在主会话提示词（`subagent::DELEGATION`）；子会话模型按「角色写的 → 设置 `subagent_model` → 跟主会话」取。
  子会话的事件经 `subagent::forward` 转到主会话（工具行带 `parent`，回复 / 思考换成 `SubagentChunk`，
  开头发 `SubagentStarted` 带任务说明；`history::rebuild` 都跳过）。同一步的几个 `task` 并行跑（`run_tools`）。
  界面上子 agent 的事件按 task 调用 ID 收进 `Session::subagents`，不进 transcript；对话里只有一张子任务卡片，
  点开是详情页（`app/subagent_view.rs`，`Session::viewing_task`）。
- 待办清单（`todo_write`）：内核在 `SessionState::todos` 记着当前清单（续接 / 回退从日志的 `PlanReplaced` 恢复）；
  还有没做完的项、连续 `TODO_REMIND_STEPS` 步没更新时，在那次请求的最后一条消息上临时挂提醒（不进历史，`todo_nudge`）；
  压缩时内核把当前清单附在摘要后面（`tools::summary_with_todos`），不靠总结的模型抄。
  `todo_write` 常驻（不在 `DEFERRED` 里）：清单是用户看进度的主渠道，搜一次才能用会让模型干脆不建。
- **中途汇报的兜底**（`run_steps` 里的局部计数器）：连续 `PROGRESS_REMIND_STEPS` 步只调工具不说话、
  或连续 `FAILURE_REMIND_STEPS` 步的工具调用全部失败时，同样在那次请求的最后一条消息上挂 `<progress-reminder>`
  （`tools::progress_reminder` / `blocker_reminder`，不进历史，失败优先并同时清零静默计数）。
  失败**按步数**不按调用数：一步里并行试探几条路径都不存在是查证，不是卡住。
  只提醒主会话的 Code 会话（子会话用户看不到、聊天会话没有长工具链）；同一段「中途汇报」的提示词也
  只注入主会话的 `preamble`（子会话被明确告知没人看得到它、也不能提问）。标签**别改回 `<system-reminder>`**：
  清单提醒的测试按那个子串断言。
- **历史与用量按需读取**：启动时 `Session::placeholder` 只给最近 100 个会话建元信息占位，首屏不读 jsonl；
  激活后 `ChatApp::ensure_history_loaded` 后台读日志，界面移动有效记录重放，读取期间发送进现有队列。
  磁盘搜索逐行扫描，只留短标题和有限命中，遇回退撤销失效命中，跳转等加载完成再定位。
  `has_user_message` 对未加载占位也为真，别把占位误删成空会话。用量由 `usage::build_index` 在后台读
  `usage-cache.json`，复用行缓冲按偏移补扫，末尾半行留待下次；当前格式的 `event` 行按标签跳过。删除会话要换代，迟到的索引结果不能覆盖新轮次。

## 休眠、渲染缓存与流式

- **自研历史休眠**（`crates/builtin/src/hibernate.rs`）：默认开启，`CLAUDESKTOP_HIBERNATE=0` 可关闭，
  借用 `SessionState.history` 逐条写入 64 KiB 缓冲的临时文件，保留 v1 JSON 格式，刷写、同步并关闭句柄后原子改名到 `session-hibernation/`。
  恢复保留会话状态、`acp_id` 和 `restored`，不发 `Started`。界面保留当前 + 最近 2 个温会话，其余闲置 10 分钟后台休眠
  （压测可用 `CLAUDESKTOP_HIBERNATE_IDLE_SECS` 改阈值），激活后台 `wake_session`，发送在内核线程兜底恢复。
  发送 / 压缩 / 回退 / 关闭共用每会话 `history_io`，轮末标题和记忆提炼另算 `history_activity`；磁盘 I/O 不持全局会话锁。
  界面显示数据另由 `session/cache.rs` 无损暂存到 `display-cache/`（正文、子任务、工具图片和文本附件），
  只搬显示缓存，保留同一个 `Session` 的内核绑定、模式、思考档位和滚动 / 展开状态；恢复不走 `from_records`。
  保存期间切回或发送先等写完再读取；加载期间的消息排队、迟到事件缓存在会话里，恢复后按序处理。`CLAUDESKTOP_DISABLE_DISPLAY_RECLAIM=1` 仅关闭这层回收，便于 A/B。
  读取失败保留快照且明确报错，不自动降级。`history_stats` 只给估算值，真实内存仍用进程工具测；
  性能日志是 `[perf] session_hibernate` / `session_wake`，`[mem]` 另有 resident / dormant 数。完整桌面 3 轮 A/B 与 30 分钟混合验收已过，数据见 HANDOFF.md。
- **渲染与温会话缓存**：远程 / 工具图片按内容哈希共用 `assets` 原始字节（24 MiB LRU），`ToolImage` 只存资源名；
  `app/image_cache.rs` 按全部解码帧字节限制非钉住部分 64 MiB，渲染引用和加载中项钉住，淘汰逐窗口 `drop_image`，开关
  `CLAUDESKTOP_DISABLE_IMAGE_RECLAIM`。公式节点只存哈希 / 尺寸 / 源码，`core::math` 按实际字节 4 MiB LRU，
  开关 `CLAUDESKTOP_DISABLE_MATH_RECLAIM`。非活动且空闲会话的单项 256 KiB 以上回复 / 思考 / 启动信息 / diff /
  文本附件由 `session/blocks.rs` 外置，预览 4 KiB；激活先校验全部块和引用，再把正文搬到最后一处引用。发送兜底恢复，冷休眠先还原成原快照。
  读失败保留块与错误并留队，开关 `CLAUDESKTOP_DISABLE_WARM_RECLAIM`。压测 `tools/hibernate-bench.mjs`
  有 `--sample=render|math|warm` 和对应 `--compare=image|math|warm`，原始结果与每阶段分类账都存 `.claudesktop/bench/`。
- 流式文本合批上屏（`Session::stream_pending` / `STREAM_FLUSH`）：chunk 先进缓冲，短消息每 50ms 并进正文一次，
  间隔随消息长度爬坡到 250ms（`stream_flush_interval`，8KB 起爬、64KB 封顶）——每次并进是整篇重解析，长消息砍刷新率省解析；
  没并进的 chunk 不重画（`on_kernel_event` 的 `skip_notify`）。`flush_stream` 不分思考 / 正文一次全并，
  正文 chunk 来了只想收尾思考时按 `stream_pending_thought` 判断后再调。flush 点要全：
  实质事件进 `apply_event` 前、轮末 `close_turn`、停止、回退（`truncate_to` 直接清空）、切内核、`on_fatal`。
  背景：`reading_markdown` 流式时补齐半截标记，交给 TextView 的文本每帧都不是上一帧的前缀，
  gpui-kit 的增量解析（`push_str`）用不上，不合批就是整篇 markdown 每个 chunk 重解析一次。

## 工具输出、审查、记忆、检查点

- 命令运行中的输出：`run_shell` 每 0.5 秒把最新几行经 `KernelEvent::ToolCallProgress` 报给界面（不进日志），跑完由最终的 `ToolCallUpdated` 覆盖；
  命令行（`ToolCard::shows_command`）随时可点开，展开后先列完整命令，所以行上不挂悬停提示。长文本的悬停提示用 `chat.rs` 的 `text_tip`（限宽、截断），别直接 `Tooltip::new`。
- 逐块审查（`crates/core/src/review.rs` + `app/review.rs`）：只审最新一轮、跑完了才行；撤回 / 恢复写盘后追加带 `reverted` 的 `Record::FileChanged`（回退照常算），
  下一条消息的 `sent` 前面附 `<user-review>` 说明（`Session::review_pending` 只是提示，说明从日志现算）。磁盘现状对不上就只读，不合并。
  入口在回复末尾的改动卡片（`render_changes_card`）：整轮「撤回」/「恢复」也走这套（`undo_turn`），撤回状态记在 `Session::undo`（进行指纹）。
  块的划分只有一份：`diff::hunks_with_context`（`review::hunks` 就是它），「授权时按块同意」也用它——
  权限卡勾掉一部分块后 `resolve_permission_with(.., PartialGrant)` 交给内核，`Action::run(.., keep)` 用
  `diff::splice_ranges` 只写放行的块。只有「允许一次」按块写（「总是允许」是存规则）。ACP 的 `hunks` 恒为 None。
- 记忆（`crates/core/src/memory.rs` + `crates/builtin/src/remember.rs` + `app/memory_view.rs`）：两份 JSON（数据目录 / 项目根 `.claudesktop/`），
  文件是唯一真相、每轮重读。**系统提示词那一节只放预算内的**（`pick_missed` 按 `pinned` → 最近用过 / 改过排），这一轮相关的另附 `<memory-context>` 在用户消息尾
  （`Inner::attach_memory`，system 变了会废掉提示缓存）；超预算的条数写在提示词里让模型用 `memory_search` 找。
  `used_ms` 不在每轮写（进 git 的项目记忆会一直变），只在本来就要写盘时由 `mark_used` 刷。`memory_forget` 只删本会话记下的（别的会话的让用户去设置页删）。
  第 3 期的候选只进设置页顶部「待确认」（`KernelEvent::MemorySuggested`，不进日志、不进对话流），用户收下才落盘；开关 `memory_suggest` 默认关。
- 内核新增会写文件的工具时，执行前后各 `checkpoint::read_snapshot` 一次并发 `KernelEvent::FileChanged`，
  否则检查点回退还原不了它改的文件（见「设计说明-检查点与回退.md」）。

## 思考强度

- 思考强度（`crates/builtin/src/provider.rs` 的 `ThinkingLevel` / `thinking_spec` / `apply_thinking`）：
  对外的唯一选项是**档位**（默认 = 不发字段 / 关 / 极低 / 低 / 中 / 高 / 极高 / 最高），预算和 effort 只是翻译出来的值。
  **`ThinkingFormat` 是结构体不是枚举**：`toggle`（厂商开关字段）+ `effort`（档位字段）+ `budget`（`budget_tokens` 老格式）
  三件事分开，因为 DeepSeek V4 / GLM-5.2+ / MiniMax-M3.1 / Doubao 都是「开关和档位一起发」（`ThinkingFormat::toggle_with_effort`）。
  `ThinkingLevel::Max` 是最高档（`max`），`XHigh` 比它低——DeepSeek / GLM / Kimi K3 / MiniMax / Doubao / Grok 的最高就是 `max`。
  能力表按「协议 + 模型名」给能选哪些档和字段怎么写；**认不出的模型不发任何思考字段、设置页也不显示那一行**
  （发错了就是 400，宁可没这个选项）。`ThinkingSpec::accepts_temperature == false` 的模型（Opus 4.7 以后）
  不管开不开思考都不发 `temperature`，这条和「开思考不发 temperature」一起在 `apply_thinking` 里管，
  所以新增 provider 只要组完体调一次 `apply_thinking`、`extra` 仍最后合。
  「关」的写法按格式分：有 `toggle` 的发 `disabled`（那几家默认开着思考，不发等于没关），
  纯 effort 的才发 `none`。
  旁路小请求（补全 / 记忆提炼 / 分类器）一律 `suggest::short_thinking(protocol, model)`（按实际协议查表）：能关就关、关不掉取最低一档。
  **会话级档位**：输入框只保留模型 chip，思考选项并进模型弹层底部的固定横栏（`composer.rs` 的 `model_menu_items`，
  ACP 内核不显示）；模型列表滚动时刷新入口和横栏不滚。选项改的是 `Session::thinking`，经
  `Kernel::set_thinking(session, Option<&str>)` 存进内核的 `SessionState`，`run_steps` 每轮开头取一次、
  覆盖档案里的那份（`None` = 跟随档案；进行中的一轮用开头那档，工具循环中途开 / 关思考 Anthropic 报 400）。
  会话还没建立 / 内核重连（`detach_kernel`）时记在 `Session::pending_thinking`，`Started` 时由 `ChatApp::flush_pending_thinking` 补交。
  能选哪些档用 `session_thinking_spec` 按**当前模型**现算，底栏高亮用 `active_thinking`（实际生效的那档）；认不出的模型只给「默认 / 兜底」。
  加新模型 = 能力表加一条，同时想清楚 `can_disable`（关不掉的那代别给「关」）和 `accepts_temperature`。

## worktree 会话

- **worktree 会话**（`crates/core/src/git.rs` + `apps/desktop/src/app/worktree.rs`，见「设计说明-worktree.md」）：
  目录在**数据目录** `worktrees/<仓库名>-<短哈希>/<分支尾段>`，不往用户仓库里塞东西；分支名 `claudesktop/<月日-时分>-<3 位随机>`。
  `cwd` 就是 worktree 目录，侧栏分组用 `Session::group_dir()`（有 worktree 时是主仓库根），worktree 会话归到主项目组、行尾带 `GitBranch` 图标。
  **项目根认 worktree**（`.git` 是文件也算，`project_root` 照样认），但**项目记忆和权限规则按 `project::shared_root`**（主仓库根），
  在 worktree 里记的事回主目录也算数。分支状态只在**一轮结束和点开菜单时**跑 `git status`（`Session::worktree_state`），不在 render 里跑。
  **合并不在主目录里直接 merge**：菜单「合并回主目录」往输入框填一句话让 agent 去做；主目录脏 / 不在 base 时用 `git::merge_blocker` 说原因。
  建 worktree 期间发的消息进 `Session::queued`，会话还没就绪时 `dispatch_queued` 会留着等 `Started`。
  后台 git 任务回来时按 `Session::token` 找会话（`session_index`），别存下标（删会话会挪）；菜单里的 git 结果（`worktree_blocker`）在 `refresh_worktree` 里算好，render 不跑 git。
  删除 worktree 走 `ask_remove_worktree`：不确定干净就先弹框，确认了才 `--force` / `-D`。
  建好后的 `worktree.copy` / `worktree.setup` 在 `app/worktree_setup.rs`：setup 写在仓库里，**没确认过的不自动跑**
