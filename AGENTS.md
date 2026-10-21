# claudesktop 开发约定

## UI：优先用 gpui-kit 自带组件，不要手写

本项目依赖 `gpui-kit 0.7`，其组件实体在 `gpui-component` crate 里（本地源码：
`~/.cargo/registry/src/*/gpui-component-0.7.3/src/`，文档站 gpui-kit.com，有中文）。
**先查现成组件再动手写 div 链**——组件清单包括 Button / Popover / Modal / Dropdown /
Tooltip / Switch / Tabs / theme / virtual_list / chart / dock 等 60 余个模块。

视觉与交互的取舍依据（色彩 / 圆角 / 层级 / 文案 / 反模式）见
[设计指南-Claude风格UI.md](./设计指南-Claude风格UI.md)，第七章是对照本项目的落地清单。

### 已沉淀的封装与惯例

- **下拉 / 浮层菜单**：一律用 `app/menu.rs` 的 `menu_popover()`（trigger chip 用
  `MenuChip`）。open 状态由 Popover 自己持有（点外 / Esc 关闭，选中后
  `PopoverState::dismiss`），**不要**在 `ChatApp` 根 struct 里加 `*_open` bool、
  手摆 `absolute()` 坐标或手写互斥——那是 2026-10 之前的老写法，已清除。
- **菜单条目**：两行式（图标 + 名称 + 灰色说明 + 当前项对勾）用 `menu.rs` 的 `menu_item()`，
  「选的是观感」的条目（如字体）用 `font_menu_item()`（上面灰字名称，下面用该字体本身排一行预览）；
  装进 `MenuList::pick(row, action)`（点了执行并收起，前 9 个自动标键帽、按数字直接选；动作要 `Window` 时用 `pick_in`）；
  不收起面板的内容用 `MenuList::child`。面板宽度用 rem。消息下方的小操作按钮（复制 / 编辑 / 重跑）用
  `widgets.rs` 的 `message_action()`。
- **设置页**：排版件在 `app/settings_kit.rs`——`section`（分区标题 + 一句说明 + 右侧操作）、
  `setting_row` / `setting_group`（标题 + 右侧控件，说明收进标题旁 ⓘ 的悬停提示；标题下要挂路径 / 按钮用 `setting_row_with`）、`form_field`（左标签右输入）、
  `choice_panel` + `choice_row`（沙色面板里的单选列表，同首页配置引导）、`seg_control`。
  说明只写一句「这是干什么的」（`section` / `setting_row` / `form_field` 都放进 ⓘ，页面上不铺灰字）；怎么用写成 `how_to` 编号步骤放空状态，格式细节收进 `ChatApp::disclosure`。
  「内核与模型」要点保存（底栏固定，`form_dirty` 决定能不能点），其余分类即时生效。
- **视图模式**：新增对话行类型时，在 `ChatItem::visible_in` 里写清它在摘要模式下显示与否，
  在 `ChatItem::is_step` 里写清它算不算过程步骤（算的话会和相邻步骤收进一个可折叠的组，
  分组规则在 `Session::compute_layout`）；
  行高受别的状态影响时（如"是不是回合末尾"），把那个状态并进 `Session::row_key`（或 `RowLayout`），否则虚拟列表不会重测。
  权限请求带工具调用 ID，应答后结果并进那一行（`ToolCard::grant`，见 `Session::merge_permission`），权限行不再画。
- **权限模式「自动」**（`crates/builtin/src/auto.rs`）：由一次独立的模型调用（分类器）代替用户审批。
  `authorize` 里 `Access::Read` 和工作目录内的编辑**不进分类器**（省一个往返），其余走
  「硬熔断（`tools::hard_block`：删关键路径、写受保护文件）→ 已保存规则 → 分类器」。
  分类器只喂用户说过的话 + 要执行的动作，**剥掉助手文本和工具结果**（防自我说服、防注入）；
  判不出来一律按拒绝。拒绝的理由连同「别换个马甲重试」回给模型，它自己决定换路子还是问用户。
  连续 3 次 / 累计 20 次拒绝就停下来等用户（用户再发消息就重新计数）。分类器看的用户消息续接 / 回退时从日志重建（`history::Rebuilt::user_messages`）。没在设置里配好时这个模式不出现在模式列表里。
  设置里开着时，**新建的对话默认就是「自动」**（`ChatApp::new_session` → `Session::default_to`）：
  会话还没连上内核时先把这一项补进模式表（`model::auto_mode_entry`）并选中，`Started` 时走
  `Session::pending_mode` 交给内核（内核不认就退回它报的那个）；用户在下拉里改了就以此为准，
  内核重连（`detach_kernel`）时把当前模式重新挂上 `pending_mode`。
- **Markdown（`TextView`）**：字号、颜色、行高要直接设在 `TextView` 上（它实现了 `Styled`），
  设在外层 div 上不生效。
- **条目点击回调**：Popover 的 `content` 闭包里拿到的是 `Context<PopoverState>`，
  动作经 `Entity<ChatApp>.update(cx, |this, cx| ...)` 回根视图（`MenuList::pick` 已包好）；参照
  `composer.rs` 里 `mode_menu_items` 的写法。菜单打开时焦点在 `ChatApp::menu_focus`（数字键靠它），
  trigger 不是 `MenuChip` 的菜单自己组 Popover 时套 `menu_panel()`。
- **按钮**：用 `widgets.rs` 的 helper（都是 gpui-kit `Button`，键盘可达、按下不抢输入框焦点）：
  文字按钮 `text_button`（primary 赤陶实底 / 默认卡片底细边，`.loading()` 表示进行中），
  透明底图标按钮 `quiet_button`（`Surface::Paper` / `Sand` 决定悬停色，`.small()` / `.xsmall()` 调尺寸，
  `.tooltip(..)` 直接给文字）。只有发送、回到底部这类特殊样式才手写 div；列表行、chip 不算按钮。
  带边框的容器（卡片、权限卡、输入框）里的按钮不再描边（单层边框）：次级用 `SAND` 实底
  （悬停 `SELECTED`），拒绝 / 危险项只留 `RED` 字、悬停出底色（红字在 `SAND` 上只有 4.0:1）。
- **颜色**：只用 `theme.rs` 的常量（纸底 / 赤陶橙那套），不要内联新的十六进制色值。
  - `Color` 是语义 token，取值查当前色板（六套配色，设置页「外观 · 配色」切换）；新颜色在 `Palette` 加一项、
    `WARM` 给值，别的色板不写就继承，写了要过 `palette_contrast` 测试。
  - 每个常量是浅色 / 深色一对（`Shade`），界面里写 `rgb(INK)`；这个 `rgb` 是 `theme::rgb`（按当前色板 + 外观取值），
    文件里要 `use crate::theme::rgb;`，否则和 `gpui_kit::*` 里的同名函数冲突。新颜色两种外观都要给值并校对对比度。
  - 前景放在会随外观反转的实底上（如 `INK` 底的停止按钮）时用 `PAPER` 当前景，不用固定的 `WHITE`；
    强调色实底（`CORAL` / `CORAL_DEEP`）上的文字用 `ON_ACCENT`（有的配色深色版按钮底很亮）；
    `RED` 是前景色（深色下提亮），危险操作的实色按钮底用 `DANGER`。
  - 交互态：纸底 / 卡片上悬停用 `HOVER`，选中态和沙色底上悬停用 `SELECTED`，已选中行里再叠一层用
    `HOVER_STRONG`；不要拿 `LINE` / `PAPER` / 透明度当悬停。
    整块可点的卡片（输入框）悬停时加深边框而不是换底色：静止 `LINE` → 悬停 `LINE_STRONG` →
    聚焦 `INK_FAINT` + 阴影。焦点几乎总在输入框上，「聚焦」要同时看 `window.is_window_active()`，
    否则切到别的程序后也一直显示聚焦态，悬停态永远看不到。
  - 带文字的主按钮底色用 `CORAL_DEEP`（悬停 `CORAL_DEEP_HOVER`）；`CORAL` 上白字只有 3.1:1，只给图标按钮。
  - `INK_FAINT` 只给占位、禁用、分隔符和装饰（纸底上 2.05:1），承载信息的文字用 `INK_SOFT`。
  - gpui-kit 组件的配色由 `theme::apply_colors` 从色板映射（`Button` 的 primary / secondary / danger 等直接可用）；
    组件颜色不对时先补映射，别在调用处逐个覆盖。派生的 `button_*` 色要和基色一起写。
- **圆角 / 宽度**：圆角只用 gpui 档位 `rounded_xs` … `rounded_2xl`（各档用途见 `theme.rs`），
  不写 `rounded(px(..))`；阅读列 / 设置页 / 概览 / 侧栏宽度用 `READING_W` / `FORM_W` / `OVERVIEW_W` / `SIDEBAR_W`。
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
- **命令面板**（Ctrl+K，`app/palette.rs`）：新的全局操作顺手在 `palette_groups` 里加一条。
  条目确认走 `on_confirm` 回调改 `ChatApp`，不用 `CommandItem::action`：对话框在 Root 浮层里，
  不在 `ChatApp` 键上下文下，派发的动作收不到。
- **快捷键提示**：用 `ChatApp::kbd(KEY)`（gpui-kit `Kbd`），按键串取 `keys.rs` 里和绑定共用的常量，不写死文案。
- **动效**一律走 `app/motion.rs`（都遵守「减少动态效果」= 系统「动画效果」或设置页「动画」）：
  - 「进行中」只有两种样子：运行状态行用呼吸圆点 `motion::breathing_dot()`，其余（工具行、侧栏、压缩）一律圆弧转圈
    `motion::spinner()`，都是 `canvas` + `PathBuilder` 矢量画；思考中的扫光用 `shimmer_text()`（按像素分档着色）。
    内容区里这三个只是占位格（prepaint 记位置），由叠在内容区上的 `motion::LoopOverlay` 画，节拍器只重画它、不重画整页对话；
    内容区以外自己就是小覆盖层的 view（侧栏转圈）用 `spinner_in_place`。
    转圈换成结果图标用 `motion::settle` 淡入。**不用** gpui-kit 的 `Spinner` / `ShimmerText`：
    动画每帧都要重画它所在的整个 view，这两个按满帧率重画 `ChatApp`（实测回合中单核 50%+）；
    motion 版相位按全局时钟算，由节拍器每两帧重画一次（约 30 帧）。
  - 展开 / 收起：`reveal_progress(id, open, ..)` 拿弹簧进度，内容 `progress > 0` 时包进 `Reveal::new(id, progress, ..)`；
    和标题之间的空隙算进内容（`pt_1` 而不是外层 `gap`），收到底才不剩一截。过程组里的步骤在列表那层
    （`RowLayout::member`）统一处理。悬停才出现的操作用 `hover_opacity`；刚出现的内容用 `entering`。
  - 一次性过渡的时长 / 曲线取 `cx.theme().motion_tokens()`，不自己写数字。
  - 对话列表的滚轮在 `content.rs` 的 capture 阶段截住，走 `ChatApp::wheel_by` / `step_wheel` 平滑滑动
    （gpui `List` 自己一格硬跳 60px）；新的程序化滚动（跳转、跟随底部）要清掉 `Session::wheel`，否则两边抢。
- **侧栏分区**：对话历史上半区是**项目**（`session_groups` 按工作目录分组，当前项目在前），
  下半区是**任务**（`TASKS_KEY` 一组：建立时没为它选过文件夹的会话，`Session::task` 落进
  `SessionMeta::task`），最后是「已归档」。判定在 `ChatApp::new_session`（`is_project_dir`：
  默认目录 `acp_base.cwd` 以外都算项目，默认目录要在设置里选过 `workspace` 才算；当前目录不是项目时侧栏不列它的空组）；输入框工具栏的目录菜单里有「不选项目（任务）」
  （`ChatApp::as_task`，已经跑在默认目录就只改归类），chip 和顶栏面包屑对任务显示「任务」。
  新增对话行状态时记得把它并进 `sync_sidebar` 的指纹。
- **侧栏是缓存 view**（`SidebarView`，`.cached()`）：侧栏要显示新状态时，把它并进 `sync_sidebar` 的指纹，
  否则侧栏不会跟着变。侧栏里会动的东西（运行中会话的转圈）不画在侧栏里，见 `sidebar.rs` 文件头。
- **输入框上方的内容区也是缓存 view**（`ContentView`，见 `content.rs`）：`ChatApp` 每次 `cx.notify()` 都让它重画，
  打字时只有输入区重画。所以**只影响输入区的刷新**（补全、发送键、聚焦边框）用 `ChatApp::refresh_composer`，
  不用 `cx.notify()`；画在 `ChatApp` 里的循环动画要包成单独的小 view（如侧栏转圈的 `SidebarSpinners`），
  否则节拍器每拍通知 `ChatApp`，内容区跟着重画。
- **性能压测**：`CLAUDESKTOP_STRESS=<行数>` 逐帧滚动出帧间隔和每帧工作量（`work`，render 到画完）；
  再加 `CLAUDESKTOP_STRESS_BUSY=1` 改成置忙 5 秒（末尾挂着思考中和跑着的工具），看转圈常驻时每秒重画几次、每帧工作量（CPU 在外面量）；
  加 `CLAUDESKTOP_STRESS_TYPE=1` 改成逐帧往输入框打字；加 `CLAUDESKTOP_STRESS_VIEW=settings:N` / `home` /
  `requests`（`requests-all` 全部列出）改成逐帧滚设置页 / 首页 / 「请求」页签（`<行数>` 是假请求的轮数）。
  动画 / 重画范围、设置页和首页的改动前后各跑一次。

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
- **输入框的 Tab 补全**（`app/suggest.rs` + `crates/builtin/src/suggest.rs`）：停顿一下就让模型
  猜一句下文，灰字画在光标后面，Tab 接上（`complete_view.rs` 里 `IndentInline` 的第二个分支，
  排在 `/` `@` 列表和首页 ghost prompt 之后）。灰字不能自己算位置——光标在第几行、横向滚了多远
  只有输入框知道：叠在输入框上包一层 `deferred(canvas(..))`，prepaint 读
  `state.prepaint_caret_geometry()`（当前帧的光标框 + 行高 + `input_bounds`），paint 里
  `with_content_mask(input_bounds)` 裁边再 `shape_line().paint()`。字号用 `TEXT_BODY`
  （gpui-kit 输入框 Medium 档就是 `text_sm`），行高取输入框排出来的那份。
  触发条件收得很紧（光标在末尾、没有选区、一轮没在跑、`/` `@` 列表没开），接上之后不连猜。
  每次停顿就是一次请求：默认关，开关和补全模型在「内核与模型」（`suggestions` / `suggest_model`），
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

## 其他

- 会话 / 内核逻辑放 `crates/*`（可单测），UI 层保持薄。
- 调试钩子统一走环境变量（`CLAUDEDESKTOP_*`），入口集中在 `main.rs`；
  涉及 Popover 内部状态的钩子做不了（open 状态不在我们手里），别为此改成受控模式。
- 提交信息用英文 conventional commits（见 git log），代码注释用中文。
- 本文件会整份进自研内核的系统提示词（`crates/builtin/src/instructions.rs`，每次请求都带），新增内容保持精炼。
- 新增要进日志的 `KernelEvent` 时，同时想清楚 `Record::worth_keeping`、`history::rebuild`（续接时进不进上下文）
  和 `Session::apply_event`（重放时怎么显示）三处。读日志一律先过 `claudesktop_core::effective`（折算回退）。
- MCP 客户端在 `crates/builtin/src/mcp.rs`（配置是数据目录的 `mcp.json`，格式同 Claude Code 的 `.mcp.json`；
  本地命令走 stdio，远程的 Streamable HTTP / 旧版 SSE 在 `mcp_http.rs`；连上过又断了的自动退避重连）；工具以 `mcp__服务器__工具` 交给模型、走 `Action::Mcp` 和正常的权限流程，
  状态经 `KernelEvent::McpStatus` 到设置页「MCP」（`app/mcp_view.rs`）。改 MCP 用 `tools/mock-mcp-server.mjs` 测（`--http` / `--sse` 起远程模式，测试在 `tests/mcp.rs`）。
- 设置页「MCP」「插件」分「已安装 / 推荐」两栏带搜索（`app/catalog_view.rs`），推荐的内容在 `crates/core/src/catalog.rs`：
  MCP 推荐项点「添加」写进 mcp.json（`mcp::add_server`，还有 `remove_server` / `set_server_disabled`）；插件推荐项从官方市场
  稀疏克隆单个插件（`plugin_git::install_subdir`）并直接启用。新增推荐插件后跑一次 `cargo test -p claudesktop-core catalog -- --ignored`（联网真装一遍）。
- 插件读取在 `crates/core/src/plugin.rs`（数据目录 `plugins/`，格式兼容 Claude Code，说明见「设计说明-插件.md」；
  从 git 装 / 更新 / 卸载在 `plugin_git.rs`），
  启用状态在设置的 `plugins_enabled`，经 `Kernel::set_plugins` 交给内核。斜杠命令在界面 `submit_to` 展开
  （日志 `Record::User.sent` 存展开后的全文，重建上下文用它）；技能、子 agent、钩子只有自研内核接（`run_hooks`）；
  MCP 服务并进 `McpHub`，ACP 内核在开会话时经 `mcpServers` 交给 agent。
- 读用户手写的 JSON（mcp.json、插件清单）出错时用 `claudesktop_core::json_error_text` 转成「第几行第几列：什么问题」的中文；
  git / 进程 / 网络的常见报错同样先译成能照着做的话（见 `plugin_git::explain_git_error`、`mcp::spawn_error`、`mcp_http::send_error`）。
- 用量网关在 `crates/builtin/src/gateway.rs`：provider 经 `gateway::route` 发请求，开着（`ProviderConfig::gateway`）就转给本地网关、
  每个请求记一行 `gateway/<日期>.jsonl`（按内核会话 + 第几轮归组，首页「请求」页签读它，见 `requests.rs`；费用按设置里的单价表估算，见 `pricing.rs`，分峰谷价的模型在单价行尾写 `x2`，峰时段用 `@peak` 行按本地时间或指定时区判定）。新的请求出口（新 provider、新接口）也要走 `route`，内核里新的请求用途用 `gateway::tag` 标上。
  ACP agent 可选经网关：起进程时设 base URL（`gateway::agent_env`，端点编码在 `/via/` 路径里），轮次靠 `gateway::note_turn`。
  请求体和响应原文另存（`request_bodies.rs`，日志行的 `body` 指过去），「请求」页签每次请求行尾的「原文」打开查看（`app/raw_request.rs`）。
- 自研内核默认不打扰前台：系统提示词里的 `BACKGROUND` 要求测试 / 浏览器 / 截图无头跑，`tools::QUIET_ENV` 给命令带上
  不弹窗口的环境变量（用户自己设了的不覆盖）；推荐的浏览器类 MCP 带 `--headless`。
- 用量：首页概览（`app/overview.rs`）的统计卡、热力图格子、模型行和右上角箭头都跳到用量页（`app/usage_view.rs`，
  一整页不分页签：总数、趋势、模型 / 会话排行、时段、请求明细，时间范围和首页共用；画在设置页的位置上但不带分类导航）。
  点一天 / 一个模型 / 一个会话是给整页加筛选（存在 `RequestsView` 的 `day` / `model` / `session`），汇总在 `usage.rs` 的 `board`；
  费用、缓存命中从网关日志算（`request_costs`）。跑着的轮也要算进去：读用量统计用 `ChatApp::usage_stats`，不直接读 `usage`；
  网关日志在报用量、一轮结束时自动重读（`refresh_requests_soon`）。新的用量入口一律走 `ChatApp::open_usage`。
- 子 agent 在 `crates/builtin/src/subagent.rs`：`task` 工具开子会话（ID `主会话/调用 ID`），和主会话共用 `run_steps`；
  子会话的事件经 `subagent::forward` 转到主会话（工具行带 `parent`，回复 / 思考换成 `SubagentChunk`，
  开头发 `SubagentStarted` 带任务说明；`history::rebuild` 都跳过）。同一步的几个 `task` 并行跑（`run_tools`）。
  界面上子 agent 的事件按 task 调用 ID 收进 `Session::subagents`，不进 transcript；对话里只有一张子任务卡片，
  点开是详情页（`app/subagent_view.rs`，`Session::viewing_task`）。
- 待办清单（`todo_write`）：内核在 `SessionState::todos` 记着当前清单（续接 / 回退从日志的 `PlanReplaced` 恢复）；
  还有没做完的项、连续 `TODO_REMIND_STEPS` 步没更新时，在那次请求的最后一条消息上临时挂提醒（不进历史，`todo_nudge`）；
  压缩时内核把当前清单附在摘要后面（`tools::summary_with_todos`），不靠总结的模型抄。
- 命令运行中的输出：`run_shell` 每 0.5 秒把最新几行经 `KernelEvent::ToolCallProgress` 报给界面（不进日志），跑完由最终的 `ToolCallUpdated` 覆盖；
  命令行（`ToolCard::shows_command`）随时可点开，展开后先列完整命令，所以行上不挂悬停提示。长文本的悬停提示用 `chat.rs` 的 `text_tip`（限宽、截断），别直接 `Tooltip::new`。
- 逐块审查（`crates/core/src/review.rs` + `app/review.rs`）：只审最新一轮、跑完了才行；撤回 / 恢复写盘后追加带 `reverted` 的 `Record::FileChanged`（回退照常算），
  下一条消息的 `sent` 前面附 `<user-review>` 说明（`Session::review_pending` 只是提示，说明从日志现算）。磁盘现状对不上就只读，不合并。
  入口在回复末尾的改动卡片（`render_changes_card`）：整轮「撤回」/「恢复」也走这套（`undo_turn`），撤回状态记在 `Session::undo`（进行指纹）。
- 记忆（`crates/core/src/memory.rs` + `crates/builtin/src/remember.rs` + `app/memory_view.rs`）：两份 JSON（数据目录 / 项目根 `.claudesktop/`），
  文件是唯一真相、每轮重读。**系统提示词那一节只放预算内的**（`pick_missed` 按 `pinned` → 最近用过 / 改过排），这一轮相关的另附 `<memory-context>` 在用户消息尾
  （`Inner::attach_memory`，system 变了会废掉提示缓存）；超预算的条数写在提示词里让模型用 `memory_search` 找。
  `used_ms` 不在每轮写（进 git 的项目记忆会一直变），只在本来就要写盘时由 `mark_used` 刷。`memory_forget` 只删本会话记下的（别的会话的让用户去设置页删）。
  第 3 期的候选只进设置页顶部「待确认」（`KernelEvent::MemorySuggested`，不进日志、不进对话流），用户收下才落盘；开关 `memory_suggest` 默认关。
- 内核新增会写文件的工具时，执行前后各 `checkpoint::read_snapshot` 一次并发 `KernelEvent::FileChanged`，
  否则检查点回退还原不了它改的文件（见「设计说明-检查点与回退.md」）。
- 思考强度（`crates/builtin/src/provider.rs` 的 `ThinkingLevel` / `thinking_spec` / `apply_thinking`）：
  对外的唯一选项是**档位**（默认 = 不发字段 / 关 / 极低 / 低 / 中 / 高 / 极高），预算和 effort 只是翻译出来的值。
  能力表按「协议 + 模型名」给能选哪些档和字段怎么写；**认不出的模型不发任何思考字段、设置页也不显示那一行**
  （发错了就是 400，宁可没这个选项）。`ThinkingSpec::accepts_temperature == false` 的模型（Opus 4.7 以后）
  不管开不开思考都不发 `temperature`，这条和「开思考不发 temperature」一起在 `apply_thinking` 里管，
  所以新增 provider 只要组完体调一次 `apply_thinking`、`extra` 仍最后合。
  旁路小请求（补全 / 记忆提炼 / 分类器）一律 `suggest::short_thinking(model)`：能关就关、关不掉取最低一档。
  加新模型 = 能力表加一条，同时想清楚 `can_disable`（关不掉的那代别给「关」）和 `accepts_temperature`。
