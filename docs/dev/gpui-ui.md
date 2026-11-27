# 界面手册：gpui-kit 组件、色板、rem、动效、滚轮

> 本文件从根 `AGENTS.md` 拆出（2026-11）。改 `apps/desktop/src/app/**`、`session/**`、
> `theme.rs`、`assets/**` 前先读这里；根 `AGENTS.md` 只保留索引和硬规范。

## UI：优先用 gpui-kit 自带组件，不要手写

本项目依赖 `gpui-kit 0.7`，其组件实体在 `gpui-component` crate 里（本地源码：
`~/.cargo/registry/src/*/gpui-component-0.7.3/src/`，文档站 gpui-kit.com，有中文）。
**先查现成组件再动手写 div 链**——组件清单包括 Button / Popover / Modal / Dropdown /
Tooltip / Switch / Tabs / theme / virtual_list / chart / dock 等 60 余个模块。

视觉与交互的取舍依据（色彩 / 圆角 / 层级 / 文案 / 反模式）见
[设计指南-Claude风格UI.md](../design/设计指南-Claude风格UI.md)，第七章是对照本项目的落地清单。
哪些网页上的 CSS 手法能搬进 gpui、怎么换算、哪些根本做不到，见
[调研报告-Claude前端CSS手法对照.md](../archive/调研报告-Claude前端CSS手法对照.md)。

模块位置：`session/` 下 `wheel.rs` 是滚轮曲线（欠量积分 / 视觉节拍，带纯时间单测），
`layout.rs` 是行划分、分组、估高与测量指纹，`replay.rs` 是历史记录重放与内核事件投影；
`app/settings_view/` 的 `kernel.rs` / `model.rs` / `provider.rs` 是三类分栏的绘制，
`general.rs` / `rules.rs` / `web_search.rs` 是通用、规则、联网搜索三栏，`lifecycle.rs` 管设置页开合、
外观生效与 Magpie 引导，`form.rs` 管表单读写与模型 / 服务商管理，`debug.rs` 是无头调试入口；
`mod.rs` 只留设置表单状态、分栏调度与总渲染。

### 已沉淀的封装与惯例

- **下拉 / 浮层菜单**：一律用 `app/menu.rs` 的 `menu_popover()`（trigger chip 用
  `MenuChip`）。open 状态由 Popover 自己持有（点外 / Esc 关闭，选中后
  `PopoverState::dismiss`），**不要**在 `ChatApp` 根 struct 里加 `*_open` bool、
  手摆 `absolute()` 坐标或手写互斥——那是 2026-10 之前的老写法，已清除。
  chip 里放长度不由我们定的文字（项目名 / 模型名 / 分支名）时加 `.shrink()`：它给 chip 和
  Popover 的 trigger 容器一起设 `flex_shrink` + `min_w`，文字打省略号；不加就会在窄窗口 /
  大字号下把同一行的发送键顶到卡片外面（见 HANDOFF.md）。
  同一帧里可能重名的 chip（项目名恰巧等于分支名）显式传 `.id(..)`；放在 `SIDEBAR` 色表面上时
  加 `.on_sand()`（浅色色板的 `HOVER` 与侧栏底色相同，悬停会看不出来）。
- **菜单条目**：两行式（图标 + 名称 + 灰色说明 + 当前项对勾）用 `menu.rs` 的 `menu_item()`，
  「选的是观感」的条目（如字体）用 `font_menu_item()`（上面灰字名称，下面用该字体本身排一行预览）；
  装进 `MenuList::pick(row, action)`（点了执行并收起，前 9 个自动标键帽、按数字直接选；动作要 `Window` 时用 `pick_in`）；
  不收起面板的内容用 `MenuList::child`；不需要数字快捷键的菜单在加条目前调用 `.without_shortcuts()`。
  固定在滚动列表下方的内容用 `MenuList::footer`，其中的可选项用 `footer_pick`（点了收起但不占列表的数字快捷键）。
  长列表需要常驻滚动条时用 `.scroll_with_bar(max_h)`（溢出才显示，右侧留位）。默认模型菜单用紧凑单行小字号。
  权限模式菜单宽 10.5rem、行高 1.5rem，用 `.compact()` 去掉条目间距，只留单行图标、名称和对勾，不显示说明和数字键帽，也不绑定数字选择。
  服务商列表和模型页来源菜单只显示单行名称，条目间加细分隔线。面板宽度用 rem。消息下方的小操作按钮（复制 / 编辑 / 重跑）用
  `widgets.rs` 的 `message_action()`。
- **设置页**：排版件在 `app/settings_kit.rs`——`section`（分区标题 + 右侧操作）、
  `setting_row` / `setting_group`（标题 + 右侧控件；标题下要挂路径 / 按钮用 `setting_row_with`）、`form_field`（左标签右输入）、
  `choice_panel` + `choice_row`（沙色面板里的单选列表）、`seg_control`。
  标题旁不挂 ⓘ，不铺固定解释和重复的操作提示；空状态保留必要操作指引，格式细节收进 `ChatApp::disclosure`。
  设置打开时只留分类导航（会话侧栏状态不变，退出恢复）；窄窗口把导航收进页头分类菜单，切分类复用 `select_settings_tab`，不重置草稿。分类页头固定，设置行紧凑排列。
  错误、未保存状态和已有条款风险提示仍显示；内核 / 服务商 / 模型共用草稿，有改动时页头出现「撤销更改 / 保存」，其余分类即时生效。
  离开设置页时保留未保存草稿并提示，重进若外部设置没变就继续编辑（分类标「未保存」），否则重载；手动重开分类会丢弃草稿。
  自动模式、补全的附属参数按开关展开，用量记录和单价表收进「高级」；文件路径按需查看。
  内核清单同模型页用 2.25rem 单行、细分隔线，名称 / 版本状态 / 启用复选框对齐；风险和错误完整放在列表下。配置组用 `compact_setting_group`，输入和开关用小号。
  内核标志用 `assets/kernels/` 内嵌的单色 SVG（来源和许可见同目录 `LICENSE`），经 `assets::kernel_logo_path` + gpui-kit `Icon` 着 `INK`，不重画品牌轮廓；没有专属标志时用通用终端图标。服务商标志同理放在 `assets/providers/`，按 `assets::provider_logo_for_url` 匹配；首页首次配置直接列这些标志，点哪一家就展开那一家的表单（本机 agent 只启用点中的那家）。
  内核检测结果在本次应用启动中缓存（未安装 / 未运行也缓存）：magpie 启动时探一次，agent 首进内核页测一次，之后用内核页头的「刷新」统一重测；切页、重开设置不重测。
- **视图模式**：新增对话行类型时，在 `ChatItem::visible_in` 里写清它在摘要模式下显示与否，
  在 `ChatItem::is_step` 里写清它算不算过程步骤（算的话会和相邻步骤收进一个可折叠的组，
  分组规则在 `Session::compute_layout`）；
  行高受别的状态影响时（如"是不是回合末尾"），把那个状态并进 `Session::row_key`（或 `RowLayout`），否则虚拟列表不会重测。
  权限请求带工具调用 ID，应答后结果并进那一行（`ToolCard::grant`，见 `Session::merge_permission`），权限行不再画。
  超过 4 KiB 的已完成回复在 `Session::rows` 里拆成多个虚拟行（`Row::Part`）：**列表下标 ≠ 记录下标**，
  `measured` / 列表用行下标、`layout` / `transcript` 用记录下标，换算走 `Session::row_of`；
  元素 id 用行下标，`reveal` / `pin` / 闪烁 / 搜索定位传记录下标。拆块规则在 `split_markdown`，由 `rebuild_rows` 在记录形态变化时重算，流式中不拆。
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
- **Markdown（`TextView`）**：字号、颜色、行高要直接设在 `TextView` 上（它实现了 `Styled`），
  设在外层 div 上不生效。
- **回复里的远程图片**（`app/remote_image.rs` + `src/http.rs`）：`![](https://…)` 由
  `MarkdownPlugin` 认领成占位卡「图片 · 域名」+「加载」，**默认不自动请求**（设置「外观」的
  `auto_load_images` 默认关）。真图**不走 `img("https://…")`**：点了才用 `http::get_bytes` 取字节，
  按内容哈希登记进 `assets::register_image` 再交给 `img()`——所以「先问用户」是结构上的保证，
  失败也能在卡片上写清原因。新加网络图片来源时走 `http::get_bytes`，别再开第二条请求路径；也别给 gpui 配 `with_http_client`（回复里 HTML `<img>` 这类插件认领不到的会被不问用户就请求）。
- **条目点击回调**：Popover 的 `content` 闭包里拿到的是 `Context<PopoverState>`，
  动作经 `Entity<ChatApp>.update(cx, |this, cx| ...)` 回根视图（`MenuList::pick` 已包好）；参照
  `composer.rs` 里 `mode_menu_items` 的写法。菜单打开时焦点在 `ChatApp::menu_focus`（数字键靠它），
  trigger 不是 `MenuChip` 的菜单自己组 Popover 时套 `menu_panel()`。
- **按钮**：用 `widgets.rs` 的 helper（都是 gpui-kit `Button`，键盘可达、按下不抢输入框焦点）：
  文字按钮 `text_button`（primary 强调色实底 / 默认卡片底细边，`.loading()` 表示进行中），
  透明底图标按钮 `quiet_button`（`Surface::Paper` / `Sand` 决定悬停色，`.small()` / `.xsmall()` 调尺寸，
  `.tooltip(..)` 直接给文字）。只有发送、回到底部这类特殊样式才手写 div；列表行、chip 不算按钮。
  带边框的容器（卡片、权限卡、输入框）里的按钮不再描边（单层边框）：次级用 `SAND` 实底
  （悬停 `SELECTED`），拒绝 / 危险项只留 `RED` 字、悬停出底色（红字在 `SAND` 上只有 4.0:1）。
- **颜色**：只用 `theme.rs` 的常量（默认 Kimi 中性灰 + 电光蓝；暖纸 / 赤陶等配色仍可切换），不要内联新的十六进制色值。
  - `Color` 是语义 token，取值查当前色板（十套配色，设置页「外观 · 配色」切换）；新颜色在 `Palette` 加一项、
    `WARM` 给值，别的色板不写就继承，写了要过 `palette_contrast` 测试。
  - 代码块语法高亮（`syntax.rs` / `CodeThemeChoice`）由 `theme::apply_to_theme` 写入 `Theme::highlight_theme`，在设置页「外观 · 代码配色」切换。
  - 每个常量是浅色 / 深色一对（`Shade`），界面里写 `rgb(INK)`；这个 `rgb` 是 `theme::rgb`（按当前色板 + 外观取值），
    文件里要 `use crate::theme::rgb;`，否则和 `gpui_kit::*` 里的同名函数冲突。新颜色两种外观都要给值并校对对比度。
  - 前景放在会随外观反转的实底上（如 `INK` 底的停止按钮）时用 `PAPER` 当前景，不用固定的 `WHITE`；
    强调色实底（`CORAL` / `CORAL_DEEP`）上的文字用 `ON_ACCENT`（有的配色深色版按钮底很亮）；
    `RED` 是前景色（深色下提亮），危险操作的实色按钮底用 `DANGER`。
  - 交互态：纸底 / 卡片上悬停用 `HOVER`，选中态和沙色底上悬停用 `SELECTED`，已选中行里再叠一层用
    `HOVER_STRONG`；不要拿 `LINE` / `PAPER` / 透明度当悬停。
    整块可点的卡片（输入框）悬停时加深边框而不是换底色：静止 `COMPOSER_LINE` → 悬停 `LINE_STRONG` →
    聚焦再套 `composer_focus_shadow()`。焦点几乎总在输入框上，「聚焦」要同时看 `window.is_window_active()`，
    否则切到别的程序后也一直显示聚焦态，悬停态永远看不到。
  - 带文字的主按钮底色用 `CORAL_DEEP`（悬停 `CORAL_DEEP_HOVER`）；`CORAL` 上白字只有 3.1:1，只给图标按钮。
  - `INK_FAINT` 只给占位、禁用、分隔符和装饰（纸底上 2.05:1），承载信息的文字用 `INK_SOFT`。
  - gpui-kit 组件的配色由 `theme::apply_colors` 从色板映射（`Button` 的 primary / secondary / danger 等直接可用）；
    组件颜色不对时先补映射，别在调用处逐个覆盖。派生的 `button_*` 色要和基色一起写。
- **圆角 / 宽度**：常规圆角用 gpui 档位 `rounded_xs` … `rounded_2xl`（各档用途见 `theme.rs`），
  不写 `rounded(px(..))`；输入卡片和附件用同心的 `COMPOSER_RADIUS` / `COMPOSER_INNER_RADIUS`。
  阅读列 / 设置页 / 侧栏宽度用 `READING_W` / `FORM_W` / `SIDEBAR_W`。
  `content_slot()` 铺满主区（保证全窗滚轮和右缘滚动条可用），对话行、首页和输入区经
  `ChatApp::reading_column` 限宽（760px，宽阅读模式 920px），默认居中，可在设置里切左对齐；设置 / 用量页独立布局。
  右侧辅助面板在 `app/right_panel.rs`：文件树 / 源码预览 / 逐文件 Diff，可拖拽调宽或展开全宽。
  顶栏只有一个「工作区面板」开关（`toggle_right_panel_open`，记住上次停的 Tab）；指定 Tab 的入口是
  `Ctrl+Shift+F` / `Ctrl+Shift+D` 和命令面板。面板自己的头里刷新只在「文件」Tab 出现，展开全宽用
  `Maximize` / `Minimize`，`X` 和系统三键之间有竖分隔线。
  文件树在目录、展开或筛选变化时重建扁平索引，固定行高用 `uniform_list` 按视口构造。目录在面板绑定会话、刷新或首次展开时读取。
  源码预览只索引前 256 KiB 解码后的源码行，变高 `List` 按视口测量实际换行，宽度或字号变化保留顶部行及行内比例。
  改动清单也用 `uniform_list`，按归一化路径合并完成的改动，按 `Session::diff_revision` 更新并在面板隐藏时释放。
  清单指向每个文件的最后一份 Diff，对话改动卡指向该轮的那份；定位用工具行和 Diff 下标，回退后旧定位失效。
  右侧 Diff 正文用变高 `List` 借用该定位的行，表头固定，截断提示是末行。修订变化重测，隐藏时释放列表，只保留定位与滚动位置。
  `READING_W` / `FORM_W` 是 measure，**保持 px**；`SIDEBAR_W` / `SETTINGS_NAV_W` / `CHROME_H`
  是「框」，是 `Rems`，拿它算加减时要 `.to_pixels(window.rem_size())`（设置页整页宽度用 `theme::settings_page_w()`）。
- **阴影**：`raised_shadow()`（贴地一档）/ `float_shadow()`（悬浮两层）在 `theme.rs`，σ 已按 CSS 减半；
  `.shadow_xs/sm/md/lg` 是 Tailwind 原值（没减半），别当设计值用。
- **焦点环**：`theme::focus_ring(accent)` 是三层，都在边框外（1px 外部底色的缝 + 1px 实边 + σ3px 光晕；
  元素在卡片里用 `focus_ring_on(.., CARD)`）。别用 `inset` 阴影画环：内阴影在边框底下，看不见。
  输入框这种「点了就是在这里打字」的用 `focus`；按钮类要「键盘 Tab 才出环」用 `.focus_visible(..)`，
  gpui-kit `Button` 自带（`focus_ring_style`），别在调用处覆盖。
- **状态色描边**取浅档：`theme::state_line(color)`（前景色 40%），不要拿 `RED` / `GREEN` 实色描边。
- **滚动渐隐**：`session::ScrollEdge` + `scroll_handle_edge` 给渐隐高度（贴顶 / 贴底时对应那条收成 0，滚不动时两条都不画），
  `widgets::edge_fade` 画（已用 `ColorSpace::Oklab`）。侧栏那条的档位要并进 `sync_sidebar` 的指纹。
- **字号 / 行高**：只用 `theme.rs` 的 `TEXT_*` 阶梯。**它们是 `Rems` 不是 `Pixels`**——
  字号、间距、圆角、图标全按窗口 rem 换算，而 `gpui-component` 的 Root 每帧把
  `Theme::font_size` 写进 `window.set_rem_size()`，所以设置页改一个字号的缩放，
  整套界面（含间距和圆角）等比变。新写的尺寸/间距也别用 `px(..)`，除非它**不该**跟着缩放
  （窗口框架、固定图标格、按像素算的 diff 行高）。
  标题这类只收 `Pixels` 的接口用 `.to_pixels(crate::theme::root_font_size())` 换算。
  正文一律无衬线；字体族用 `theme::sans()`（设置页可换，没选过时是内嵌的 Inter），
  代码用 `MONO`。会跳动的数字（用时、token、计数）加 `.font_features(tabular_nums())`。
- **界面文案**：一律中文（含占位符、模式名、问候语）；agent 自己上报的名称除外。
- **快捷键**：全局快捷键集中在 `app/keys.rs`（`actions!` + `KeyBinding`，键上下文 `ChatApp`），
  新快捷键加在那里，并在 `app/shortcuts.rs` 的速查表（Ctrl+/）里补一行；处理函数没事可做时 `cx.propagate()`，让给外层（gpui-kit Root 的 Tab 焦点切换等）。
  快捷键依赖焦点在界面里：**别在根视图上 `track_focus`**（按下鼠标时会把焦点从输入框抢走），
  焦点丢失由 `restore_focus` 兜底（输入框，设置页开着时是设置页的焦点锚）。
- **提示消息**：不属于某个对话的（导出结果、设置 / 文件读写失败、输入限制）用 `ChatApp::toast`
  （gpui-kit `Notification`，应用内）；会话自己的事（内核错误、停止、续接）才用 `Session::note` 进对话流。
  系统通知走 `notify.rs` 的 `notify_attention`：它注册的点击回调顶掉了 gpui-kit 的，所以别用 `Notification::system()`。
- **对话末尾状态行与实时预览槽**（`app/status.rs` + `session/layout.rs`）：上下分工——
  正在跑的非简单工具 / 并行工具 / 流式思考只在**底部**画；跑完立刻落进上面的过程组（`compute_layout` 里把活跃预览项从组内剔除）。
  思考与工具结束后底部原地驻留 `LIVE_PREVIEW_LINGER`（1.5 秒）再收起，新的预览接手即让位；驻留靠 `ChatApp` 排的准点定时器收起，不靠 1 秒心跳。
  思考**不逐字流式**：攒满 4 行或等够 `LIVE_THINK_REVEAL_AFTER`（1.2 秒，且出了完整段落）才整块亮出，之后冻结 `LIVE_THINK_HOLD`（3 秒）再整块替换；
  思考块无边框、`INK_SOFT` 72% 灰、斜体。思考 chunk 不再按 50ms 重画，只有首次创建时通知一次。
  进行中的过程组露最近 `LIVE_TAIL`（6）步；摘要模式保留一行折叠的过程记录栏头。
  截图预览：`CLAUDESKTOP_OPEN=live-preview:<thinking|tool-empty|tool-output|tool-other|parallel|tool-done>`。
- **命令面板**（Ctrl+K，`app/palette.rs`）：新的全局操作顺手在 `palette_groups` 里加一条。
  条目确认走 `on_confirm` 回调改 `ChatApp`，不用 `CommandItem::action`：对话框在 Root 浮层里，
  不在 `ChatApp` 键上下文下，派发的动作收不到。
- **快捷键提示**：用 `ChatApp::kbd(KEY)`（gpui-kit `Kbd`），按键串取 `keys.rs` 里和绑定共用的常量，不写死文案。
- **动效**一律走 `app/motion.rs`（都遵守「减少动态效果」= 系统「动画效果」或设置页「动画」）：
  - 「进行中」使用统一的弹性双块 `motion::spinner()`（`canvas` + `paint_quad` 画两块圆角方块）和少量线性状态图标；
    不使用卡通形象或品牌 Logo；左上角标题为「Kimi Code」，运行状态使用线性图标或统一双块。
    思考中和底部进行中状态行的扫光用 `shimmer_text()`（按像素分档着色）。
    内容区里的转圈和扫光只是占位格（prepaint 记位置），由叠在内容区上的 `motion::LoopOverlay` 画，节拍器只重画它、不重画整页对话；
    侧栏运行中会话行用闪烁圆点（占位格记窗口坐标，由 `SidebarSpinners` 的 `canvas` 调 `motion::paint_pulse_dot` 绘制），其余内容区外小覆盖层（`bgtasks_view` / `composer`）用 `spinner_in_place`。
    转圈换成结果图标用 `motion::settle` 淡入。**不用** gpui-kit 的 `Spinner` / `ShimmerText`：
    动画每帧都要重画它所在的整个 view，这两个按满帧率重画 `ChatApp`（实测回合中单核 50%+）；
    motion 版相位按全局时钟算，节拍器按 60 帧（16ms）跑。双块一个 3.2 秒一圈、8 个半步：
    伸出去贴满一条边、顿一下、收回下一个角位（每步前 78% 位移、后 22% 定格），主块顺时针、
    副块是同一轨迹错开半圈（4 个半步），所以两块永远在对角、都在动，首尾相位完全一致，透明度不跳；
    两块几何上永不重叠，副块用 0.52 的同色透明度。圆角按短边取 0.28，伸长的条也不会被拉成胶囊。
    绘制时按全局时钟取相位；限帧间隔从上一次发出重画通知（`LAST_LOOP_TICK`）算起，命令预览的布局耗时不会挤掉下一拍。
    虚拟列表在布局阶段构建可见行；行回调需要重新绑定面板的会话与动画位置池，每次整块内容重建时才清空位置池。
  - 展开 / 收起：`reveal_progress(id, open, ..)` 拿弹簧进度，内容 `progress > 0` 时包进 `Reveal::new(id, progress, ..)`；
    列表里的一步**即使进度已是 1.0 也一直挂着 `Reveal`**（它要量过一次高度，收起首帧才不按 0 高塌一下）；
    自动收起（新行涌入、正文到达）用 `reveal_progress_when(.., animate=false)` 直接落位，
    只有用户手动点过的组（`Session::animated_group`）才播弹簧。
    悬停才出现的操作用 `hover_opacity`；刚出现的内容用 `entering`。
  - 一次性过渡的时长 / 曲线取 `cx.theme().motion_tokens()`，不自己写数字。
  - 对话列表的滚轮在 `content.rs` 的 capture 阶段截住，走 `ChatApp::wheel_by` / `step_wheel`
    （gpui `List` 自己一格硬跳）。`session::WheelInput` 按剩余位移推进，补量上方行高后滚动距离不变。
    默认根字号下 Windows 三行一格走 96px。同向输入叠加独立曲线，反向清空，到边界作废朝外的余量。
    单格曲线按实测拟合为 184ms。连滚时按相邻输入间隔为新曲线选择 156–184ms，单帧最多 48px。
    「对话滚轮动画」默认跟随全局动画，也可以单独选标准、短程或关。短程档的正文鼠标滚轮
    三行一格走 64 逻辑像素、120ms 收尾，步长独立于界面字号。精确滚动输入直接应用位移。
    `CLAUDESKTOP_WHEEL_TRACE=<路径>` 记录输入分配、列表位移与内容区 paint 时间，适用于隔离窗口验收。
    Windows 优先用 `Window::frame_vsync_at()`，合成器同周期内的额外重画跳过。没有时间戳时用墙钟滞回量化。
    最后一步仍标记为 `transcript_scrolling`，避免落定时一帧重排整屏正文。精确输入前静态重排命中区，
    消息操作栏在停稳 120ms 后从当前光标位置淡入（见 HANDOFF）。
    设置页外层和模型清单另走 `settings_wheel_by` / `step_settings_wheel`（指数逼近的 `WheelScroll`），
    按指针位置和边界选择内层或外层，切分类 / 来源要清掉对应的 `*_wheel`。新的程序化滚动（跳转、跟随底部）
    也要清掉对应滚动状态，否则两边抢。
    展开的工具输出自带限高滚动区：每次内容区渲染把它的区域和句柄登记进 `ChatApp::tool_scroll_regions`，
    滚轮落在上面时 `scroll_tool_output` 在 capture 阶段先消耗内层能滚的距离，余量接给外层并照常拦下事件
    （只放行不拦会双滚动：`overflow_y_scroll` 和列表的 bubble 监听器都会各滚一次）。
- **侧栏分区**：代码空间默认**按项目**管（`SidebarGrouping::Project`，在「设置 · 通用 · 侧栏 · 会话管理」切换）：
  上半区是项目（`session_groups` 按工作目录分组，当前项目在前，`same_project_path` 比对路径，
  Windows 下大小写 / 斜杠 / `\\?\` 不同也算同一项目），下半区是**任务**（`TASKS_KEY` 一组：
  建立时没为它选过文件夹的会话，`Session::task` 落进 `SessionMeta::task`），最后是「已归档」。
  设成**按时间**（或聊天空间，没有工作目录）时不再拆任务区：项目和任务的会话一起进
  `今天 / 昨天 / 最近 7 天 / 更早` 四个桶，桶内按最近活动排，末尾仍补「已归档」。
  项目组标题右侧悬停出「+」「打开文件夹」「从侧栏移除」；点「+」直接在该项目里新建对话。
  任务组的「+」明确新建任务对话。顶部「+ 新会话」跟随当前会话：在项目里就落在同一项目，
  在任务里就落在任务（判定见 `ChatApp::add_session`）。
  **添加项目**在导航区「新会话」下面（只在按项目时出现，`ChatApp::pick_and_add_project`），
  登记的目录进 `Settings::sidebar_projects`：一条会话都没有也常驻一组；选文件夹、在项目里继续、
  组头「+」都会顺手登记（`register_project`）。**移除项目**（组头「X」）不丢数据：有真会话时
  组头换成行内确认，确认后真会话归档进「已归档」、纯空会话直接丢掉，登记 / 折叠记录和
  `workspace` 一起清掉；有会话正在跑则拦下来提示。老设置里的 `sidebar_by_date` 读盘时折算进
  `sidebar_grouping`，不再写回。
  判定在 `ChatApp::new_session`（`is_project_dir`：默认目录 `acp_base.cwd` 以外都算项目，
  默认目录要在设置里选过 `workspace` 或登记成项目才算；当前目录不是项目时侧栏不列它的空组）。
  输入框上方另有**项目顶盖**：只在尚未发消息的 Code 会话显示，可选项目 / 切本地分支 / 建 worktree；
  发出第一条消息后由 `motion::Reveal` 收起，对话中不再提供切换项目。worktree 会话的收尾入口随之
  交给顶栏分支 chip（`s.has_user_message() && s.worktree.is_some()`），两边不同时出现。
  分支读取、列表和切换都在后台线程跑，缓存进会话的 `git_branch`；菜单打开时才重列分支。
  新增对话行状态时记得把它并进 `sync_sidebar` 的指纹。
  无头验收：`CLAUDESKTOP_OPEN=sidebar-project|sidebar-time` 切分组方式截图，
  加 `:flow` 走一遍添加 / 移除项目并断言。
- **侧栏是缓存 view**（`SidebarView`，`.cached()`）：侧栏要显示新状态时，把它并进 `sync_sidebar` 的指纹，
  否则侧栏不会跟着变。侧栏里会动的东西（运行中会话的闪烁圆点）不画在侧栏里，见 `sidebar.rs` 文件头。
- **输入框上方的内容区也是缓存 view**（`ContentView`，见 `content.rs`）：`ChatApp` 每次 `cx.notify()` 都让它重画，
  打字时只有输入区重画。所以**只影响输入区的刷新**（补全、发送键、聚焦边框）用 `ChatApp::refresh_composer`，
  不用 `cx.notify()`；画在 `ChatApp` 里的循环动画要包成单独的小 view（如侧栏闪烁圆点的 `SidebarSpinners`），
  否则节拍器每拍通知 `ChatApp`，内容区跟着重画。

### gpui-kit 踩坑备忘

- **`use gpui_kit::*` 的文件里写单元测试必须用 `#[core::prelude::v1::test]`**，
  裸 `#[test]` 会被 glob 导入的 gpui `test` 宏劫持，展开时无限递归
  （报 "recursion limit reached while expanding #[test]"，提高 recursion_limit 没用）。
- **`TabBar` 和 `PopupMenu`（`ContextMenu` / `DropdownMenu` 的底层）的高度 / 内边距写死 px，不随界面缩放**：
  分段切换用 `widgets.rs` 的 `seg()`，菜单用 `menu_popover()`。`Command`、`Button`、`Dialog` 内容是 rem，可用。
  `Button` 的尺寸是 rem，圆角取 `Theme::radius`（`theme::apply_to_theme` 按根字号换算）；
  它默认 `cursor_default`，helper 里补了 `cursor_pointer`。
- **重画的范围是 view**：`request_animation_frame` 通知当前 view，祖先 view 一并变脏重画；
  `.cached()` 的 view 在自己重画时会让里面嵌套的缓存 view 全部重画（`window.refreshing`），**嵌套缓存没用**。
  缓存 view 要定尺寸（从 style 排版，不按内容量），按内容长高的区域（输入框）缓存不了。
- **`Animation::with_max_fps` 在 Windows 上不准**：靠计时器排下一帧，计时器按 15.6ms 一档取整再等垂直同步，
  要 30 帧实际 21~25 帧，还和别的动画错开多出重画。限帧用 `motion.rs` 的节拍器（`on_next_frame` 数帧）。
- `MotionReveal`（gpui-base）首次出现、还没量过高度时按 0 高排一帧，在虚拟列表里会跳；用 `motion::Reveal`。
- **滚一格就重画整个 view、重排整页**（不是虚拟列表的滚动区）：所以滚动区里每帧要排的东西得便宜。
  内容宽度不定时 taffy 会按不同约束把整页来回排好几遍，用 `widgets.rs` 的 `MeasuredWidth` 给内容定宽；
  一排小段别用 `flex_wrap` + 每段一个元素，拼成一段 `StyledText` 按段着色；
  归组、汇总这类按数据算的东西别在 render 里每帧重算，按输入缓存（见 `requests_view.rs` 的 `derived_requests`）。
  页面中间的长列表按页面的 `ScrollHandle` 只排视口附近的行、上下垫空白（见 `requests_view.rs` 的 `ListWindow`）；
  排出来的行直接挂在原容器里，多包一层 taffy 会把文字多量几遍。
- `overflow_y_scroll()` 在 `StatefulInteractiveElement` 上——链式调用前必须先 `.id(...)`。
- trigger 要作为 `Popover::trigger` 使用需实现 `gpui_kit::base::Selectable`（平凡
  builder trait，见 `MenuChip`）。
- `Popover` 的 `Styled` 样式落在弹层面板上（bg / border / p / w 都能直接链）。
  **弹层外观（底色 / hairline ring / 两层阴影 / 圆角）由 `appearance`（默认开）在渲染时套
  `popover_style`**，`refine_style` 在它之后合并，所以 `menu_panel` 只写 `p_1` 就能盖掉默认的 `p_3`；
  不要在调用处写 `bg(rgb(CARD))` / `border_color(rgb(LINE))`，配色不对去 `theme::apply_colors` 补映射。
- **`role()` 要挂在带 id 的 stateful div 上**（`Div` 本身没有这个方法）；`ElementId` 只认
  `&'static str` / `usize` 这类，`("前缀", SharedString)` 拼不出来，用 `ElementId::Name(..)`。
- 组件路径：`gpui_kit::component::popover::Popover`、`gpui_kit::base::{Selectable, PopoverState}`
  （`component::*` 的 glob 不含子模块里的类型）。
- **不用 gpui-kit `TitleBar`**：窗口框架自绘（`app/chrome.rs`），侧栏顶到窗口顶，
  会话区顶栏兼做拖拽区和最小化 / 最大化 / 关闭。Windows hit-test 只看鼠标下的元素链里
  有没有 `WindowControlArea::Drag`——**拖拽区只能是按钮的兄弟（`drag_area()` 填空白），
  不能包住按钮**，否则按钮收不到点击。
- **输入框是 `TextareaState`**（`auto_grow` + `submit_on_enter`）：Shift+回车插入换行后**同样会发**
  `InputEvent::PressEnter { shift: true }`，只在 `shift: false` 时发送。输入法组字时的回车由系统输入法消化，
  不会变成 `Enter` 动作，不用自己判断（已用微软拼音实测）。
- `TextView::code_block_actions` 给了之后组件不再自己画右上角的语言名，要在 actions 里一起画；
  actions 叠在代码块第一行上，代码块的上内边距要留出它的高度（`markdown_style` 里的 `pt`）。
- **输入框的键盘接管**（补全列表开着时的上下键 / 回车 / Tab / Esc）：输入框自己把这些键绑成了
  `MoveUp` / `Enter` / `IndentInline` / `Escape` 动作，动作先于原始按键事件分发，所以 `on_key_down`
  截不到；要在输入框的祖先上 `capture_action`（capture 阶段从根到焦点），处理了 `cx.stop_propagation()`，
  没处理就什么都不做（放行）。见 `app/complete_view.rs`。
  `/` `@` 面板锚在输入框上沿往上长，**高度按量到的卡片上沿限死**（`widgets::MeasuredTop`，
  `composer_top`），超出在面板里滚、上下键把选中项 `scroll_to_item` 滚回可见——不然窗口矮 /
  字号大时整个面板顶出窗口，最上面几项看不见也点不到。
  细节（`设计说明-输入框方向键.md`）：↑↓ **只有两项以上才截**（一项时放行移光标）；←→ 一律放行，
  在 capture 里 `cx.defer_in` 一次 `sync_completion`——`InputEvent` 没有光标事件，只能键放行后
  自己再对一次触发词（移出 `/` `@` 范围就收列表）；Ctrl+回车不用另绑，输入框的
  `secondary-enter` 已经是 `Enter { secondary: true }` 且照常发 `PressEnter`；Alt+← / → 展开 / 收起列表
  （macOS 上那对键被输入框占了按词移动，走 Ctrl+Alt+← / →）。
- **输入框的 Tab 补全**（`app/suggest.rs` + `crates/builtin/src/suggest.rs`）：停顿一下就让模型
  猜一句下文，灰字画在光标后面，Tab 接上（`complete_view.rs` 里 `IndentInline` 的第二个分支，
  排在 `/` `@` 列表和首页 ghost prompt 之后）。灰字不能自己算位置——光标在第几行、横向滚了多远
  只有输入框知道：叠在输入框上包一层 `deferred(canvas(..))`，prepaint 读
  `state.prepaint_caret_geometry()`（当前帧的光标框 + 行高 + `input_bounds`），paint 里
  `with_content_mask(input_bounds)` 裁边再 `shape_line().paint()`。字号用 `TEXT_BODY`
  （gpui-kit 输入框 Medium 档就是 `text_sm`），行高取输入框排出来的那份。
  触发条件收得很紧（光标在末尾、没有选区、一轮没在跑、`/` `@` 列表没开），接上之后不连猜。
  每次停顿就是一次请求：默认关，开关和补全模型在「设置 · 内核」（`suggestions` / `suggest_model`），
  只带最近 3 轮；过期的请求经 `should_stop` 当场中断；总要思考的模型（`suggest::is_reasoning_model`）不补。
  调试钩子 `CLAUDESKTOP_SUGGEST=<灰字>`（值 `-` 只填草稿不摆灰字，用来前后对比截图）。
- 文件对话框与系统联动：`cx.prompt_for_paths(PathPromptOptions)`（注意有 `prompt`
  字段）、`cx.prompt_for_new_path(dir, name)`，均返回 oneshot，配 `cx.spawn_in` +
  `this.update_in` 回 UI；`cx.reveal_path(&path)` 在资源管理器中打开。
- **`gap_2p5` / `px_5` / `py_2p5` 这类"分数后缀"helper 确实存在**，只是全在宏里生成，
  静态搜索（grep 源码）找不到定义：`gpui-pre-macros` 的 `padding_style_methods!` /
  `margin_style_methods!` 按 `BoxStyleSuffix` 表展开，`2p5` = `rems(0.625)` = 10px、
  `5` = `rems(1.25)` = 20px（表在 `gpui-pre-macros-*/src/styles.rs`）。
  它们按 **rem** 取值，`0.5` 档是 8px 而非 4px——算间距时注意基准不是 16px 的整数倍。
  不要再把"搜不到定义"当成渲染塌陷的证据：早先正是据此误判顶栏渲染坏了，
  实际是探针里的 `top_bar()` 从未被 `render()` 调用（死代码）。
- **gpui-pre / gpui-pre-windows 是打过补丁的本地拷贝**（`vendor/`，经 `[patch.crates-io]`；改了中文避头尾、Windows 上文字纵向位置不算字体 lineGap）。
  改 gpui 本身的行为照此办理：改动标 `claudesktop patch`，记进 `vendor/README.md`，升级时按标记重打。
- **gpui 没有 CSS**，抄网页上的写法前先看这张表（对照结论见「调研报告-Claude前端CSS手法对照.md」）：
  - **做不到**：`mask-image`（只有矩形 `ContentMask`，渐隐只能拿背景色渐变覆盖层近似）、
    `letter-spacing`、`font-variation-settings`（`TextStyle` 没有这两项）、`text-wrap`、
    元素级 `backdrop-filter`（只有整窗的 `WindowBackgroundAppearance`）、
    `:has()` / `@container` / `view-transition`、按语言切 CJK fallback（`FontFallbacks` 是平铺列表）。
  - **hairline 也做不到**：`round_stroke_to_device_pixel` 把任何非零描边夹到 ≥1 物理像素
    （`gpui-pre/src/util.rs`）。要更淡就降颜色的对比度，别想细线。
  - **`box-shadow` 的 blur 要减半**：gpui 的 `BoxShadow::blur_radius` 是高斯 σ，CSS 是 2σ。
    `.shadow_xs/sm/md/lg` 是宏生成的、直接用了 Tailwind 数值（**没减半**，比原版宽一倍）；
    要贴原版观感就自己写 `BoxShadow`，参照 `gpui-component` 的 `popover_shadow` / `raised_shadow`。
  - **`linear_gradient` 只有两个色标**（`Background.colors: [LinearColorStop; 2]`），
    多段渐变要么叠条、要么用 `.color_space(ColorSpace::Oklab)` 让两色过渡更均匀。
  - **`aria-*` 是有的**（`role()` / `aria_selected` / `aria_expanded` / `aria_keyshortcuts` …），
    别照网上「gpui 没有无障碍属性」的说法。状态用 ARIA 表达，样式和行为就不会脱节。
