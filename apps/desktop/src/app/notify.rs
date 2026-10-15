//! 系统通知：窗口不在前台时，回复完成 / 等你授权 / 出错弹系统通知（Windows toast），
//! 点通知回到那个会话；回到窗口时撤掉发过的通知。要不要提醒由 `Session::attention` 判断。

use super::ChatApp;
use gpui_kit::*;

/// 通知标签：每个会话一个，同一会话的新通知替换旧的。
fn tag_of(token: u64) -> SharedString {
    format!("session-{token}").into()
}

impl ChatApp {
    /// 启动时调用：跟踪窗口前后台，注册通知的点击回调。
    pub(super) fn init_notifications(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 前台 = 窗口激活且没最小化。从别的程序把窗口最小化时 Windows 可能不发「失活」，
        // 所以还要看可见性（最小化 = Hidden）
        fn in_front(window: &Window) -> bool {
            window.is_window_active() && window.visibility() == WindowVisibility::Visible
        }
        fn update(this: &mut ChatApp, window: &Window, cx: &mut Context<ChatApp>) {
            this.window_active = in_front(window);
            if this.window_active {
                for tag in this.notified.drain(..) {
                    cx.dismiss_system_notification(&tag);
                }
            }
        }
        self.window_active = in_front(window);
        cx.observe_window_activation(window, |this, window, cx| update(this, window, cx)).detach();
        cx.observe_window_visibility(window, |this, _, window, cx| update(this, window, cx))
            .detach();

        let app = cx.entity().downgrade();
        let handle = window.window_handle();
        cx.on_system_notification_response(move |response, cx| {
            let Some(token) =
                response.tag.strip_prefix("session-").and_then(|t| t.parse::<u64>().ok())
            else {
                return;
            };
            let _ = handle.update(cx, |_, window, cx| {
                window.activate_window();
                let _ = app.update(cx, |this, cx| {
                    if let Some(i) = this.sessions.iter().position(|s| s.token == token) {
                        this.activate_session(i, cx);
                    }
                });
            });
        });
    }

    /// 第 `idx` 个会话有事要提醒：窗口在前台或设置里关了通知就不发。
    pub(super) fn notify_attention(&mut self, idx: usize, body: String, cx: &mut Context<Self>) {
        if self.window_active || !self.settings.notifications {
            return;
        }
        let s = &self.sessions[idx];
        let tag = tag_of(s.token);
        cx.show_system_notification(SystemNotification {
            tag: tag.clone(),
            title: s.title.clone(),
            body: body.into(),
            actions: Vec::new(),
        });
        if !self.notified.contains(&tag) {
            self.notified.push(tag);
        }
    }
}
