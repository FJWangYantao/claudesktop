//! claudesktop 桌面端入口。
//!
//! 目标界面：Claude 桌面风（暖纸底 + 赤陶橙），含
//! - 自绘标题栏 + 侧栏（工作区分组 / 任务列表 / 内核卡）
//! - 会话视图：左缘头像轨道、思考块、工具卡、计划、流式 Markdown
//! - 空会话首页：问候语 + 用量概览（统计卡 + 热力图）
//! - 底部输入区：工作区 / 模式 chips、圆角输入条、权限模式栏

mod app;
mod demo;
mod model;
mod session;
mod theme;

use app::ChatApp;
use gpui_kit::component::*;
use gpui_kit::*;
use std::time::Duration;

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);

            let mut options = TitleBar::window_options();
            options.window_bounds = Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1280.), px(820.)),
                cx,
            )));
            if let Some(tb) = options.titlebar.as_mut() {
                tb.title = Some("claudesktop · M1 UI 预览".into());
            }

            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| {
                    let app = ChatApp::new(window, cx);
                    // 延迟 1.5s 触发自动演示，避开启动阶段的窗口初始化。
                    cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(Duration::from_millis(1500))
                            .await;
                        let _ = this.update(cx, |app, cx| app.autodemo_hook(cx));
                    })
                    .detach();
                    app
                })
            })
            .expect("Failed to open window");
            cx.activate(true);
        });
}
