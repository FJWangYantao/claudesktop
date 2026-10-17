//! 全局快捷键：GPUI `actions!` + `KeyBinding`，作用域是根视图的 `ChatApp` 键上下文。
//!
//! - 输入框（gpui-kit `Input`）自己也绑了 Esc / Shift+Tab，但没事可做时会 `propagate`，
//!   GPUI 接着试外层的绑定，于是落到这里；菜单（Popover）打开时 Esc 先由它关菜单。
//! - 没有焦点时 GPUI 从窗口根分发，键上下文里没有 `ChatApp`，快捷键全部失效。所以启动时
//!   把焦点给输入框，焦点丢失（聚焦的元素被移出界面：关设置页、重命名提交等）时再还给它。
//!   不在根视图上 `track_focus`：那样点任何按钮都会把焦点从输入框抢走。
//! - 打开某个 Popover 的快捷键做不了（open 状态在组件里），权限模式改用 Shift+Tab 循环。

use super::ChatApp;
use gpui_kit::component::WindowExt as _;
use gpui_kit::*;

/// 根视图的键上下文名。
pub(super) const CONTEXT: &str = "ChatApp";

actions!(
    claudesktop,
    [
        /// 停止当前一轮；设置页开着时先关设置页
        StopTurn,
        /// 循环切换权限模式（列表由 agent 上报）
        CycleMode,
        /// 新会话
        NewSession,
        /// 显示 / 隐藏侧栏
        ToggleSidebar,
        /// 按侧栏顺序切到下一个会话
        NextSession,
        /// 按侧栏顺序切到上一个会话
        PrevSession,
        /// 循环切换视图模式（摘要 / 标准 / 详细）
        CycleView,
        /// 界面放大一档
        ZoomIn,
        /// 界面缩小一档
        ZoomOut,
        /// 界面字号回到标准档
        ZoomReset,
        /// 打开命令面板
        OpenPalette,
        /// 打开快捷键速查
        ShowShortcuts,
    ]
);

// 界面上用 `Kbd` 提示的键：绑定和提示读同一个常量，改键只改这里。
/// 新会话 / 显示隐藏侧栏 / 命令面板
pub(super) const NEW_SESSION_KEY: &str = "secondary-n";
pub(super) const TOGGLE_SIDEBAR_KEY: &str = "secondary-b";
pub(super) const PALETTE_KEY: &str = "secondary-k";
pub(super) const SHORTCUTS_KEY: &str = "secondary-/";
/// 停止 / 循环权限模式 / 前后切会话
pub(super) const STOP_KEY: &str = "escape";
pub(super) const CYCLE_MODE_KEY: &str = "shift-tab";
pub(super) const NEXT_SESSION_KEY: &str = "ctrl-tab";
pub(super) const PREV_SESSION_KEY: &str = "ctrl-shift-tab";
/// 循环切换视图模式（与官方一致，macOS 上也是 Ctrl+O）
pub(super) const CYCLE_VIEW_KEY: &str = "ctrl-o";
/// 界面放大 / 缩小一档（和浏览器一致）
pub(super) const ZOOM_IN_KEY: &str = "secondary-=";
pub(super) const ZOOM_OUT_KEY: &str = "secondary--";
pub(super) const ZOOM_RESET_KEY: &str = "secondary-0";

/// 注册按键（应用启动时调用一次）。`secondary` 在 macOS 上是 Cmd，其余平台是 Ctrl。
pub(crate) fn bind(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(STOP_KEY, StopTurn, Some(CONTEXT)),
        KeyBinding::new(CYCLE_MODE_KEY, CycleMode, Some(CONTEXT)),
        KeyBinding::new(NEW_SESSION_KEY, NewSession, Some(CONTEXT)),
        KeyBinding::new(TOGGLE_SIDEBAR_KEY, ToggleSidebar, Some(CONTEXT)),
        KeyBinding::new(PALETTE_KEY, OpenPalette, Some(CONTEXT)),
        KeyBinding::new(SHORTCUTS_KEY, ShowShortcuts, Some(CONTEXT)),
        KeyBinding::new(NEXT_SESSION_KEY, NextSession, Some(CONTEXT)),
        KeyBinding::new(PREV_SESSION_KEY, PrevSession, Some(CONTEXT)),
        KeyBinding::new(CYCLE_VIEW_KEY, CycleView, Some(CONTEXT)),
        // 界面缩放：和浏览器一致的 Ctrl + 加号 / 减号 / 0
        KeyBinding::new(ZOOM_IN_KEY, ZoomIn, Some(CONTEXT)),
        KeyBinding::new("secondary-+", ZoomIn, Some(CONTEXT)),
        KeyBinding::new(ZOOM_OUT_KEY, ZoomOut, Some(CONTEXT)),
        KeyBinding::new(ZOOM_RESET_KEY, ZoomReset, Some(CONTEXT)),
    ]);
}

impl ChatApp {
    /// 把键盘焦点放回输入框。
    pub(crate) fn focus_input(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |state, cx| state.focus(window, cx));
    }

    /// 焦点丢失时的兜底：设置页开着就给设置页的焦点锚（输入框这时不在界面里），否则给输入框。
    pub(super) fn restore_focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        // 对话框（看大图）开着时焦点归它，Esc 才能关掉它
        if window.has_active_dialog(cx) {
            return;
        }
        if self.settings_open {
            self.settings_focus.focus(window, cx);
        } else {
            self.focus_input(window, cx);
        }
    }

    /// 根视图挂上键上下文和各动作的处理。
    pub(super) fn with_key_actions(&self, root: Div, cx: &mut Context<Self>) -> Div {
        root.key_context(CONTEXT)
            .on_action(cx.listener(Self::on_stop_turn))
            .on_action(cx.listener(Self::on_cycle_mode))
            .on_action(cx.listener(|this, _: &NewSession, window, cx| {
                this.settings_open = false;
                this.add_session(cx);
                this.focus_input(window, cx);
            }))
            .on_action(
                cx.listener(|this, _: &OpenPalette, window, cx| this.open_palette(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ShowShortcuts, window, cx| this.open_shortcuts(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.sidebar_open = !this.sidebar_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &NextSession, _, cx| this.step_session(1, cx)))
            .on_action(cx.listener(|this, _: &PrevSession, _, cx| this.step_session(-1, cx)))
            .on_action(cx.listener(|this, _: &CycleView, _, cx| {
                this.set_view_mode(this.settings.view_mode.next(), cx)
            }))
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| this.zoom_by(1, cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| this.zoom_by(-1, cx)))
            .on_action(cx.listener(|this, _: &ZoomReset, _, cx| {
                this.set_font_scale(crate::theme::FONT_SCALE_DEFAULT, cx)
            }))
    }

    /// 界面缩放按档走（步进 5%）：`step` 为 +1 放大、-1 缩小；到边界就不动。
    fn zoom_by(&mut self, step: isize, cx: &mut Context<Self>) {
        let current = self.settings.font_scale;
        let delta = step as f32 * crate::theme::FONT_SCALE_STEP / 100.;
        let next =
            (current + delta).clamp(crate::theme::FONT_SCALE_MIN, crate::theme::FONT_SCALE_MAX);
        if (next - current).abs() > 1e-4 {
            self.set_font_scale(next, cx);
        } else {
            cx.propagate();
        }
    }

    fn on_stop_turn(&mut self, _: &StopTurn, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_open {
            self.settings_open = false;
            self.focus_input(window, cx);
            cx.notify();
        } else if self.session().busy {
            self.stop(cx);
        } else {
            cx.propagate();
        }
    }

    fn on_cycle_mode(&mut self, _: &CycleMode, _: &mut Window, cx: &mut Context<Self>) {
        let n = self.session().modes.len();
        // 设置页里 Shift+Tab 留给 gpui-kit Root 做焦点倒序切换
        if self.settings_open || n == 0 {
            cx.propagate();
            return;
        }
        self.choose_mode((self.session().permission_mode + 1) % n, cx);
    }

    /// 按侧栏的显示顺序前后切换会话（首尾相接）。
    fn step_session(&mut self, step: isize, cx: &mut Context<Self>) {
        let order = session_order(self.session_groups().into_iter().map(|(_, members)| members));
        if let Some(next) = neighbor(&order, self.active, step) {
            self.activate_session(next, cx);
        }
    }
}

/// 把侧栏各组的会话下标拼成一条显示顺序。
fn session_order(groups: impl IntoIterator<Item = Vec<usize>>) -> Vec<usize> {
    groups.into_iter().flatten().collect()
}

/// `current` 在 `order` 里前后走 `step` 步（首尾相接）；不在列表里时取第一个。
fn neighbor(order: &[usize], current: usize, step: isize) -> Option<usize> {
    let len = order.len() as isize;
    if len == 0 {
        return None;
    }
    let pos = order.iter().position(|&i| i == current).map_or(0, |p| p as isize + step);
    Some(order[pos.rem_euclid(len) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn neighbor_wraps_both_ways() {
        let order = session_order([vec![2, 0], vec![1]]);
        assert_eq!(order, vec![2, 0, 1]);
        assert_eq!(neighbor(&order, 0, 1), Some(1));
        assert_eq!(neighbor(&order, 1, 1), Some(2));
        assert_eq!(neighbor(&order, 2, -1), Some(1));
        assert_eq!(neighbor(&order, 0, -1), Some(2));
    }

    #[core::prelude::v1::test]
    fn neighbor_handles_missing_and_empty() {
        assert_eq!(neighbor(&[], 0, 1), None);
        // 当前会话不在列表里：取第一个
        assert_eq!(neighbor(&[3, 4], 9, 1), Some(3));
    }

    /// 界面提示用的按键串都能解析（`ChatApp::kbd` 解析失败会 panic）：速查表里的全部按键。
    #[core::prelude::v1::test]
    fn hint_keys_parse() {
        for (_, rows) in super::super::shortcuts::SHORTCUTS {
            for (_, keys) in *rows {
                for key in *keys {
                    assert!(Keystroke::parse(key).is_ok(), "{key}");
                }
            }
        }
    }
}
