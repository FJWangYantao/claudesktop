//! claudesktop 桌面端入口。
//!
//! 目标界面：对照新版 Claude Code 桌面端（暖纸底 + 赤陶橙），含
//! - 自绘窗口框架：浮起的圆角侧栏顶到窗口顶，会话区顶栏兼做拖拽区与窗口按钮
//! - 会话视图：右对齐用户气泡、思考块、工具行、计划、流式 Markdown、末尾运行状态行
//! - 空会话首页：问候语 + 用量概览（统计卡 + 热力图）
//! - 底部输入区：上方环境 / 项目 chips，输入框，下方权限模式 / 内核 / 模型

mod app;
mod complete;
mod demo;
mod fonts;
mod model;
mod session;
mod settings;
mod theme;
mod usage;

use app::ChatApp;
use gpui_kit::component::*;
use gpui_kit::*;
use settings::KernelChoice;
use std::time::Duration;

pub(crate) static STARTUP_T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

fn main() {
    let t0 = std::time::Instant::now();
    let bench = std::env::var("CLAUDESKTOP_BENCH_STARTUP").is_ok();
    let _ = STARTUP_T0.set(t0);

    // 先于任何子进程：应用退出（含被强杀）时连带结束 agent 进程树
    if let Err(err) = claudesktop_core::kill_children_on_exit() {
        eprintln!("[claudesktop] 防孤儿进程保护未启用: {err}");
    }
    let t_job = t0.elapsed();

    gpui_kit::application().with_assets(gpui_kit::assets::AllAssets).run(move |cx| {
        let t_gpui_run = t0.elapsed();
        gpui_kit::init(cx);
        // 系统通知要有应用身份（Windows AppUserModelID，通知上显示的应用名）
        cx.set_app_identity("claudesktop.desktop", "claudesktop");
        app::keys::bind(cx);
        fonts::register(cx);
        let t_fonts = t0.elapsed();

        let mut options = TitleBar::window_options();
        options.window_bounds =
            Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1280.), px(820.)), cx)));
        if let Some(tb) = options.titlebar.as_mut() {
            // 任务栏 / Alt+Tab 里的名字（窗口内不显示标题栏）
            tb.title = Some("claudesktop".into());
        }

        gpui_kit::open_window(options, cx, |window, cx| {
            let t_window_open = t0.elapsed();
            cx.new(|cx| {
                let app = ChatApp::new(window, cx);
                let t_app_new = t0.elapsed();
                if bench {
                    eprintln!(
                        "[startup_bench] main -> job_object:       {:>6.2} ms",
                        t_job.as_secs_f64() * 1000.0
                    );
                    eprintln!(
                        "[startup_bench] main -> gpui_runtime:     {:>6.2} ms",
                        t_gpui_run.as_secs_f64() * 1000.0
                    );
                    eprintln!(
                        "[startup_bench] main -> fonts_registered: {:>6.2} ms",
                        t_fonts.as_secs_f64() * 1000.0
                    );
                    eprintln!(
                        "[startup_bench] main -> window_opened:    {:>6.2} ms",
                        t_window_open.as_secs_f64() * 1000.0
                    );
                    eprintln!(
                        "[startup_bench] main -> app_initialized:  {:>6.2} ms",
                        t_app_new.as_secs_f64() * 1000.0
                    );
                }
                // 延迟 1.5s 触发自动演示，避开启动阶段的窗口初始化。
                cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(Duration::from_millis(1500)).await;
                    let _ = this.update_in(cx, |app, window, cx| {
                        // 调试钩子：CLAUDESKTOP_OPEN = settings（打开设置页）/ fetch（并获取模型列表）
                        // / restart（重启内核）
                        // / rename、ask-delete、delete（对侧栏第 2 个会话）
                        match std::env::var("CLAUDESKTOP_OPEN").as_deref() {
                            Ok("settings") => app.open_settings(window, cx),
                            Ok("fetch") => {
                                app.open_settings(window, cx);
                                app.fetch_models(true, cx);
                            }
                            // 侧栏第 2 行（第一个历史会话）：重命名 / 删除确认 / 直接删除
                            Ok("rename") => app.start_rename(1, window, cx),
                            Ok("ask-delete") => app.ask_delete(1, cx),
                            Ok("delete") => app.delete_session(1, cx),
                            Ok("restart") => {
                                let kind = app.session().kernel;
                                app.restart_kernel(kind, cx)
                            }
                            _ => {}
                        }
                        app.autodemo_hook(cx);
                    });
                    // 压测钩子：CLAUDESKTOP_STRESS=<行数>，灌数据后逐帧滚动（先往上 150 帧再往下 150 帧，
                    // 每帧画完才滚下一段，量的是真实帧间隔；不用计时器驱动，Windows 计时器按 15.6ms 一档）。
                    // 加 CLAUDESKTOP_STRESS_BUSY=1 则不滚，置忙 5 秒看转圈常驻时每秒重绘几次；
                    // 加 CLAUDESKTOP_STRESS_TYPE=1 则改成逐帧往输入框里打字。
                    // 统计写到 CLAUDESKTOP_STRESS_OUT（默认 stderr）
                    if let Some(n) = std::env::var("CLAUDESKTOP_STRESS")
                        .ok()
                        .and_then(|v| v.parse::<usize>().ok())
                    {
                        let busy = std::env::var_os("CLAUDESKTOP_STRESS_BUSY").is_some();
                        let typing = std::env::var_os("CLAUDESKTOP_STRESS_TYPE").is_some();
                        let _ = this.update(cx, |app, cx| app.debug_stress(n, busy, cx));
                        cx.background_executor().timer(Duration::from_millis(1500)).await;
                        let _ = this.update_in(cx, |app, window, cx| {
                            app.frame_log = Some(Vec::new());
                            if typing {
                                app.debug_type_frames(0, window, cx);
                            } else if !busy {
                                app.debug_scroll_frames(0, window, cx);
                            }
                        });
                        if busy {
                            cx.background_executor().timer(Duration::from_secs(5)).await;
                            let _ = this.update(cx, |app, _| app.stress_report());
                        }
                    }
                    // 调试钩子：CLAUDESKTOP_RERUN = builtin / acp / demo，
                    // 等 CLAUDESKTOP_RERUN_DELAY_MS（默认 10s）后把首条提示词放到该内核上重跑
                    let rerun =
                        std::env::var("CLAUDESKTOP_RERUN").ok().and_then(|v| match v.trim() {
                            "builtin" => Some(KernelChoice::Builtin),
                            "acp" => Some(KernelChoice::Acp),
                            "demo" => Some(KernelChoice::Demo),
                            _ => None,
                        });
                    if let Some(kind) = rerun {
                        let delay = std::env::var("CLAUDESKTOP_RERUN_DELAY_MS")
                            .ok()
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(10_000);
                        cx.background_executor().timer(Duration::from_millis(delay)).await;
                        let _ = this.update(cx, |app, cx| app.rerun_first_prompt(kind, cx));
                    }
                })
                .detach();
                app
            })
        })
        .expect("Failed to open window");
        cx.activate(true);
    });
}
