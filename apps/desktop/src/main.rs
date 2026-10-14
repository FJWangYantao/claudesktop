//! claudesktop 桌面端入口。
//!
//! 目标界面：Claude 桌面风（暖纸底 + 赤陶橙），含
//! - 自绘标题栏 + 侧栏（工作区分组 / 任务列表 / 内核卡）
//! - 会话视图：左缘头像轨道、思考块、工具卡、计划、流式 Markdown
//! - 空会话首页：问候语 + 用量概览（统计卡 + 热力图）
//! - 底部输入区：工作区 / 模式 chips、圆角输入条、权限模式栏

mod app;
mod demo;
mod fonts;
mod model;
mod session;
mod settings;
mod theme;
mod usage;

use app::ChatApp;
use settings::KernelChoice;
use gpui_kit::component::*;
use gpui_kit::*;
use std::time::Duration;

fn main() {
    // 先于任何子进程：应用退出（含被强杀）时连带结束 agent 进程树
    if let Err(err) = claudesktop_core::kill_children_on_exit() {
        eprintln!("[claudesktop] 防孤儿进程保护未启用: {err}");
    }

    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            fonts::register(cx);

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
                    cx.spawn_in(window, async move |this, cx| {
                        cx.background_executor()
                            .timer(Duration::from_millis(1500))
                            .await;
                        let _ = this.update_in(cx, |app, window, cx| {
                            // 调试钩子：CLAUDESKTOP_OPEN = settings（打开设置页）/ fetch（并获取模型列表）
                            // / models、top-models（输入框旁 / 顶栏的模型下拉）/ kernels（打开内核弹层）/ restart（重启内核）
                            // / rename、ask-delete、delete（对侧栏第 2 个会话）
                            match std::env::var("CLAUDESKTOP_OPEN").as_deref() {
                                Ok("settings") => app.open_settings(window, cx),
                                Ok("fetch") => {
                                    app.open_settings(window, cx);
                                    app.fetch_models(true, cx);
                                }
                                Ok("models") => app.toggle_model_menu(false, cx),
                                Ok("top-models") => app.toggle_model_menu(true, cx),
                                // 侧栏第 2 行（第一个历史会话）：重命名 / 删除确认 / 直接删除
                                Ok("rename") => app.start_rename(1, window, cx),
                                Ok("ask-delete") => app.ask_delete(1, cx),
                                Ok("delete") => app.delete_session(1, cx),
                                Ok("kernels") => app.open_kernel_menu(cx),
                                Ok("restart") => {
                                    let kind = app.session().kernel;
                                    app.restart_kernel(kind, cx)
                                }
                                _ => {}
                            }
                            app.autodemo_hook(cx);
                        });
                        // 压测钩子：CLAUDESKTOP_STRESS=<行数>，灌数据后先滚到顶再滚回底（各 150 帧，每帧 16ms），
                        // 把帧间隔统计写到 CLAUDESKTOP_STRESS_OUT（默认 stderr）
                        if let Some(n) = std::env::var("CLAUDESKTOP_STRESS")
                            .ok()
                            .and_then(|v| v.parse::<usize>().ok())
                        {
                            let _ = this.update(cx, |app, cx| app.debug_stress(n, cx));
                            cx.background_executor()
                                .timer(Duration::from_millis(1500))
                                .await;
                            let _ = this.update(cx, |app, _| app.frame_log = Some(Vec::new()));
                            for step in 0..300 {
                                let dy = if step < 150 { -400. } else { 400. };
                                let _ = this.update(cx, |app, cx| app.debug_scroll(dy, cx));
                                cx.background_executor()
                                    .timer(Duration::from_millis(16))
                                    .await;
                            }
                            let _ = this.update(cx, |app, _| {
                                let log = app.frame_log.take().unwrap_or_default();
                                let mut gaps: Vec<f64> = log
                                    .windows(2)
                                    .map(|w| (w[1] - w[0]).as_secs_f64() * 1000.)
                                    .collect();
                                gaps.sort_by(|a, b| a.total_cmp(b));
                                let pct = |p: f64| {
                                    gaps.get(((gaps.len() as f64 - 1.) * p) as usize).copied().unwrap_or(0.)
                                };
                                let report = format!(
                                    "rows={n} frames={} p50={:.1}ms p95={:.1}ms max={:.1}ms\n",
                                    log.len(),
                                    pct(0.5),
                                    pct(0.95),
                                    pct(1.0)
                                );
                                match std::env::var("CLAUDESKTOP_STRESS_OUT") {
                                    Ok(path) => {
                                        let _ = std::fs::write(path, report);
                                    }
                                    Err(_) => eprint!("{report}"),
                                }
                            });
                        }
                        // 调试钩子：CLAUDESKTOP_RERUN = builtin / acp / demo，
                        // 等 CLAUDESKTOP_RERUN_DELAY_MS（默认 10s）后把首条提示词放到该内核上重跑
                        let rerun = std::env::var("CLAUDESKTOP_RERUN").ok().and_then(|v| {
                            match v.trim() {
                                "builtin" => Some(KernelChoice::Builtin),
                                "acp" => Some(KernelChoice::Acp),
                                "demo" => Some(KernelChoice::Demo),
                                _ => None,
                            }
                        });
                        if let Some(kind) = rerun {
                            let delay = std::env::var("CLAUDESKTOP_RERUN_DELAY_MS")
                                .ok()
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(10_000);
                            cx.background_executor()
                                .timer(Duration::from_millis(delay))
                                .await;
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
