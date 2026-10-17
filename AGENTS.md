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
  点击收尾用同文件导出的 `menu_action!`。消息下方的小操作按钮（复制 / 编辑 / 重跑）用
  `widgets.rs` 的 `message_action()`。
- **视图模式**：新增对话行类型时，在 `ChatItem::visible_in` 里写清它在摘要模式下显示与否，
  在 `ChatItem::is_step` 里写清它算不算过程步骤（算的话会和相邻步骤收进一个可折叠的组，
  分组规则在 `Session::compute_layout`）；
  行高受别的状态影响时（如"是不是回合末尾"），把那个状态并进 `Session::row_key`（或 `RowLayout`），否则虚拟列表不会重测。
- **Markdown（`TextView`）**：字号、颜色、行高要直接设在 `TextView` 上（它实现了 `Styled`），
  设在外层 div 上不生效。
- **条目点击回调**：Popover 的 `content` 闭包里拿到的是 `Context<PopoverState>`，
  动作经 `Entity<ChatApp>.update(cx, |this, cx| ...)` 回根视图；参照
  `composer.rs` 里 `mode_menu_items` 等菜单条目函数的写法（`menu_action!` 宏在 `menu.rs`）。
- **按钮**：新按钮优先 `gpui-component` 的 `Button`（变体齐全）；确需特殊样式再
  手写 div，但成组出现时应抽成 `app/widgets.rs` 的 helper。
  带边框的容器（卡片、权限卡、输入框）里的按钮不再描边（单层边框）：次级用 `SAND` 实底
  （悬停 `SELECTED`），拒绝 / 危险项只留 `RED` 字、悬停出底色（红字在 `SAND` 上只有 4.0:1）。
- **颜色**：只用 `theme.rs` 的常量（纸底 / 赤陶橙那套），不要内联新的十六进制色值。
  - 每个常量是浅色 / 深色一对（`Color`），界面里写 `rgb(INK)`；这个 `rgb` 是 `theme::rgb`（按当前外观取值），
    文件里要 `use crate::theme::rgb;`，否则和 `gpui_kit::*` 里的同名函数冲突。新颜色两种外观都要给值并校对对比度。
  - 前景放在会随外观反转的实底上（如 `INK` 底的停止按钮）时用 `PAPER` 当前景，不用固定的 `WHITE`；
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
  新快捷键加在那里；处理函数没事可做时 `cx.propagate()`，让给外层（gpui-kit Root 的 Tab 焦点切换等）。
  快捷键依赖焦点在界面里：**别在根视图上 `track_focus`**（按下鼠标时会把焦点从输入框抢走），
  焦点丢失由 `restore_focus` 兜底（输入框，设置页开着时是设置页的焦点锚）。
- **提示消息**：不属于某个对话的（导出结果、设置 / 文件读写失败、输入限制）用 `ChatApp::toast`
  （gpui-kit `Notification`，应用内）；会话自己的事（内核错误、停止、续接）才用 `Session::note` 进对话流。
  系统通知走 `notify.rs` 的 `notify_attention`：它注册的点击回调顶掉了 gpui-kit 的，所以别用 `Notification::system()`。
- **快捷键提示**：用 `ChatApp::kbd(KEY)`（gpui-kit `Kbd`），按键串取 `keys.rs` 里和绑定共用的常量，不写死文案。
- **加载 / 进行中动效**：先用 `Spinner`、`ShimmerText`（组件自带动画并尊重系统减少动效），
  别急着手写 `with_animation`。

### gpui-kit 踩坑备忘

- **`use gpui_kit::*` 的文件里写单元测试必须用 `#[core::prelude::v1::test]`**，
  裸 `#[test]` 会被 glob 导入的 gpui `test` 宏劫持，展开时无限递归
  （报 "recursion limit reached while expanding #[test]"，提高 recursion_limit 没用）。
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
  和 `Session::apply_event`（重放时怎么显示）三处。
