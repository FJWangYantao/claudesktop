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
- **条目点击回调**：Popover 的 `content` 闭包里拿到的是 `Context<PopoverState>`，
  动作经 `Entity<ChatApp>.update(cx, |this, cx| ...)` 回根视图；参照
  `composer.rs` 里 `mode_menu_items` 等三个菜单条目函数的写法（含 `menu_action!` 宏）。
- **按钮**：新按钮优先 `gpui-component` 的 `Button`（变体齐全）；确需特殊样式再
  手写 div，但成组出现时应抽成 `app/widgets.rs` 的 helper。
- **颜色**：只用 `theme.rs` 的常量（纸底 / 赤陶橙那套），不要内联新的十六进制色值。
- **字号 / 行高**：只用 `theme.rs` 的 `TEXT_*` 阶梯（12/13/14/15/17/20/28）和
  `LEADING`，不要写 `text_size(px(..))`。正文一律无衬线（对照官方 Code 桌面端；
  衬线拉丁字母会和回退的雅黑混排）。中文回退字体在根视图统一设置。
- **界面文案**：一律中文（含占位符、模式名、问候语）；agent 自己上报的名称除外。
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
- 文件对话框与系统联动：`cx.prompt_for_paths(PathPromptOptions)`（注意有 `prompt`
  字段）、`cx.prompt_for_new_path(dir, name)`，均返回 oneshot，配 `cx.spawn_in` +
  `this.update_in` 回 UI；`cx.reveal_path(&path)` 在资源管理器中打开。

## 其他

- 会话 / 内核逻辑放 `crates/*`（可单测），UI 层保持薄。
- 调试钩子统一走环境变量（`CLAUDEDESKTOP_*`），入口集中在 `main.rs`；
  涉及 Popover 内部状态的钩子做不了（open 状态不在我们手里），别为此改成受控模式。
- 提交信息用英文 conventional commits（见 git log），代码注释用中文。
