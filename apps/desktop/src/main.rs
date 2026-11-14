//! claudesktop 桌面端入口。
//!
//! 目标界面：对照新版 Claude Code 桌面端（暖纸底 + 赤陶橙），含
//! - 自绘窗口框架：浮起的圆角侧栏顶到窗口顶，会话区顶栏兼做拖拽区与窗口按钮
//! - 会话视图：右对齐用户气泡、思考块、工具行、计划、流式 Markdown、末尾运行状态行
//! - 空会话首页：问候语 + 用量概览（统计卡 + 热力图）
//! - 底部输入区：上方环境 / 项目 chips，输入框，下方权限模式 / 内核 / 模型

// release 版双击打开不带控制台黑窗（子进程都带 CREATE_NO_WINDOW）；debug 版留着看 stderr
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod assets;
mod complete;
mod demo;
mod fonts;
mod http;
mod model;
mod pricing;
mod requests;
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

pub(crate) fn perf_elapsed(name: &str, started: std::time::Instant) {
    if std::env::var("CLAUDESKTOP_BENCH_STARTUP").is_ok() {
        eprintln!("[perf] {name}: {:.2} ms", started.elapsed().as_secs_f64() * 1000.0);
    }
}

/// 崩溃时把原因和调用栈追加到数据目录的 `crash.log`：release 版没有控制台，
/// 不记下来的话用户只看到窗口闪一下就没了，没有任何线索。
fn log_panics() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(dir) = claudesktop_core::default_data_dir() {
            let _ = std::fs::create_dir_all(&dir);
            let text = format!(
                "[unix {} · v{}] {info}\n{}\n\n",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs()),
                env!("CARGO_PKG_VERSION"),
                std::backtrace::Backtrace::force_capture()
            );
            if let Ok(mut file) =
                std::fs::OpenOptions::new().create(true).append(true).open(dir.join("crash.log"))
            {
                use std::io::Write as _;
                let _ = file.write_all(text.as_bytes());
            }
        }
        default(info);
    }));
}

fn main() {
    #[cfg(not(feature = "headless-screenshot"))]
    if std::env::var_os("CLAUDESKTOP_SCREENSHOT").is_some()
        || std::env::var_os("CLAUDESKTOP_HIBERNATE_BENCH").is_some()
    {
        eprintln!("无头验收需要用 --features headless-screenshot 编译");
        std::process::exit(1);
    }
    let t0 = std::time::Instant::now();
    let bench = std::env::var("CLAUDESKTOP_BENCH_STARTUP").is_ok();
    let _ = STARTUP_T0.set(t0);
    log_panics();

    // 先于任何子进程：应用退出（含被强杀）时连带结束 agent 进程树
    if let Err(err) = claudesktop_core::kill_children_on_exit() {
        eprintln!("[claudesktop] 防孤儿进程保护未启用: {err}");
    }
    let t_job = t0.elapsed();

    gpui_kit::application()
        .with_assets(assets::Assets)
        // 不配 `with_http_client`：回复里的远程图片要先问用户，自己走 `http::get_bytes`（见 http.rs）
        .run(move |cx| {
            let t_gpui_run = t0.elapsed();
            gpui_kit::init(cx);
            // 系统通知要有应用身份（Windows AppUserModelID，通知上显示的应用名）
            cx.set_app_identity("claudesktop.desktop", "claudesktop");
            app::keys::bind(cx);
            fonts::register(cx);
            let t_fonts = t0.elapsed();

            // 静默模式不抢前台；无头模式隐藏窗口，直接离屏渲染。
            let headless = std::env::var("CLAUDESKTOP_HEADLESS").as_deref() == Ok("1");
            let screenshot = std::env::var_os("CLAUDESKTOP_SCREENSHOT");
            let hibernate_bench = std::env::var_os("CLAUDESKTOP_HIBERNATE_BENCH");
            let quiet = headless || screenshot.is_some() || hibernate_bench.is_some()
                || std::env::var_os("CLAUDESKTOP_QUIET").is_some()
                || std::env::var_os("CLAUDESKTOP_STRESS").is_some();

            let mut options = TitleBar::window_options();
            options.focus = !quiet;
            options.show = !headless && screenshot.is_none() && hibernate_bench.is_none();
            if quiet {
                // 非活动窗口默认限到 30 帧，压测量到的就不是真实帧间隔了
                options.inactive_frame_interval = None;
            }
            options.window_bounds =
                Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1280.), px(820.)), cx)));
            #[cfg(feature = "headless-screenshot")]
            if screenshot.is_some() {
                if let Some((width, height)) = std::env::var("CLAUDESKTOP_SCREENSHOT_SIZE").ok()
                    .and_then(|size| size.split_once('x').and_then(|(w, h)| Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?))))
                    .filter(|(w, h)| w.is_finite() && h.is_finite() && *w >= 400. && *h >= 300.)
                {
                    options.window_bounds = Some(WindowBounds::Windowed(Bounds::centered(None, size(px(width), px(height)), cx)));
                }
            }
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
                    #[cfg(feature = "headless-screenshot")]
                    if let Some(dir) = hibernate_bench {
                        let dir = std::path::PathBuf::from(dir);
                        cx.spawn_in(window, async move |this, cx| {
                            let mut last_id = 0;
                            let mut last_draw = std::time::Instant::now();
                            let mut last_command = std::time::Instant::now();
                            loop {
                                cx.background_executor().timer(Duration::from_millis(50)).await;
                                let command = std::fs::read(dir.join("command.json")).ok()
                                    .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
                                if let Some(command) = command
                                    && let Some(id) = command["id"].as_u64().filter(|id| *id > last_id)
                                {
                                    last_id = id;
                                    last_command = std::time::Instant::now();
                                    let result = this.update_in(cx, |app, window, cx| {
                                        app.hibernate_bench_command(&command, window, cx)
                                    }).expect("无头验收窗口已关闭");
                                    let response = match result {
                                        Ok(value) => serde_json::json!({"id": id, "ok": true, "value": value}),
                                        Err(error) => serde_json::json!({"id": id, "ok": false, "error": error}),
                                    };
                                    let tmp = dir.join("response.tmp");
                                    std::fs::write(&tmp, response.to_string()).expect("写验收结果失败");
                                    std::fs::rename(tmp, dir.join("response.json")).expect("发布验收结果失败");
                                    if command["action"] == "quit" {
                                        cx.background_executor().timer(Duration::from_secs(1)).await;
                                        let _ = cx.update(|_, cx| cx.quit());
                                        return;
                                    }
                                }
                                if last_command.elapsed() > Duration::from_secs(90) {
                                    panic!("验收控制进程失联");
                                }
                                if last_draw.elapsed() >= Duration::from_millis(250) {
                                    cx.update(|window, cx| {
                                        window.refresh();
                                        window.draw(cx).clear(cx);
                                    }).expect("无头验收绘制窗口已关闭");
                                    last_draw = std::time::Instant::now();
                                }
                            }
                        }).detach();
                        return app;
                    }
                    if headless && screenshot.is_none() {
                        cx.spawn_in(window, async move |_, cx| {
                            loop {
                                cx.background_executor().timer(Duration::from_millis(250)).await;
                                if cx.update(|window, cx| {
                                    window.refresh();
                                    window.draw(cx).clear(cx);
                                }).is_err() {
                                    break;
                                }
                            }
                        }).detach();
                    }
                    // 延迟 1.5s 触发自动演示，避开启动阶段的窗口初始化。
                    cx.spawn_in(window, async move |this, cx| {
                        cx.background_executor().timer(Duration::from_millis(1500)).await;
                        let _ = this.update_in(cx, |app, window, cx| {
                            // 调试钩子：CLAUDESKTOP_OPEN = settings（打开设置页；settings:N 打开第 N 个分类）
                            // / fetch（并获取模型列表）/ restart（重启内核）/ review（审查页）
                            // / rename、ask-delete、delete（对侧栏第 2 个会话）
                            let open = std::env::var("CLAUDESKTOP_OPEN").unwrap_or_default();
                            match open.as_str() {
                                "settings" => app.open_settings(window, cx),
                                "model-menu" => app.open_settings_at(4, window, cx),
                                #[cfg(feature = "headless-screenshot")]
                                "history-mode-menu" => app.debug_history_mode_menu(cx),
                                #[cfg(feature = "headless-screenshot")]
                                "project-menu" | "project-flow" => {
                                    app.debug_sidebar_projects(open == "project-flow", cx)
                                }
                                o if o.starts_with("settings:") => {
                                    let tab = o["settings:".len()..].parse().unwrap_or(0);
                                    app.open_settings_at(tab, window, cx)
                                }
                                "fetch" => {
                                    app.open_settings(window, cx);
                                    app.fetch_models(true, cx);
                                }
                                // 侧栏第 2 行（第一个历史会话）：重命名 / 删除确认 / 直接删除
                                "rename" => app.start_rename(1, window, cx),
                                "ask-delete" => app.ask_delete(1, cx),
                                "delete" => app.delete_session(1, cx),
                                // 服务商删除确认：只打开弹窗，不执行删除
                                o if o.starts_with("ask-delete-provider:") => {
                                    let i = o["ask-delete-provider:".len()..].parse().unwrap_or(0);
                                    app.open_settings_at(3, window, cx);
                                    app.ask_remove_form_profile(i, window, cx);
                                },
                                // 全局默认模型预览：只改表单，不保存设置
                                o if o.starts_with("default-model:") => {
                                    if let Some((profile, model)) =
                                        o["default-model:".len()..].split_once(':')
                                        && let Ok(profile) = profile.parse()
                                    {
                                        app.open_settings_at(4, window, cx);
                                        app.select_form_model(profile, model.into(), window, cx);
                                    }
                                }
                                // 侧栏第 2 个会话最新一轮的审查页；review:N 再把第 N 个文件的第一块撤回
                                o if o.starts_with("review") => {
                                    let file =
                                        o.strip_prefix("review:").and_then(|n| n.parse().ok());
                                    app.debug_review(file, cx)
                                }
                                "restart" => {
                                    let kind = app.session().kernel.clone();
                                    app.restart_kernel(&kind, cx)
                                }
                                // 在一个临时 git 仓库里建 worktree（截图 / 自检用）
                                "worktree" => app.debug_worktree(cx),
                                // 把排版样本摆成一轮回复，供对比验证。
                                "sample-reply" => app.debug_sample_reply(cx),
                                _ => {}
                            }
                            #[cfg(feature = "headless-screenshot")]
                            if std::env::var_os("CLAUDESKTOP_SCREENSHOT").is_some() {
                                let preview = std::env::var("CLAUDESKTOP_SETTINGS_PREVIEW").unwrap_or_default();
                                let scale = std::env::var("CLAUDESKTOP_SCREENSHOT_SCALE").ok().and_then(|s| s.parse().ok());
                                app.debug_settings_preview(&preview, scale, window, cx);
                            }
                            // 调试钩子：CLAUDESKTOP_DROP = 以 ; 分隔的路径，当作从资源管理器拖进来松开
                            if let Ok(paths) = std::env::var("CLAUDESKTOP_DROP") {
                                let paths: Vec<std::path::PathBuf> = paths
                                    .split(';')
                                    .filter(|p| !p.is_empty())
                                    .map(Into::into)
                                    .collect();
                                app.drop_paths(&paths, window, cx);
                            }
                            // 调试钩子：CLAUDESKTOP_SUGGEST=<灰字>，不走模型直接摆出输入框补全的灰字
                            if let Ok(text) = std::env::var("CLAUDESKTOP_SUGGEST")
                                && !text.is_empty()
                            {
                                app.debug_suggestion(text, window, cx);
                            }
                            // 调试钩子：CLAUDESKTOP_NEXT=<淡色字>，摆出「轮结束时猜的下一句」
                            if let Ok(text) = std::env::var("CLAUDESKTOP_NEXT")
                                && !text.is_empty()
                            {
                                app.debug_next_suggestion(text, window, cx);
                            }
                            app.autodemo_hook(cx);
                        });
                        #[cfg(feature = "headless-screenshot")]
                        if let Some(path) = std::env::var_os("CLAUDESKTOP_SCREENSHOT") {
                            for _ in 0..3 {
                                let _ = cx.update(|window, cx| {
                                    window.refresh();
                                    window.draw(cx).clear(cx);
                                });
                                cx.background_executor().timer(Duration::from_millis(100)).await;
                            }
                            if std::env::var("CLAUDESKTOP_SETTINGS_PREVIEW").as_deref() == Ok("refresh") {
                                let mut ready = false;
                                for _ in 0..100 {
                                    ready = this.update(cx, |app, _| app.debug_refresh_finished(false)).unwrap_or(false);
                                    if ready { break; }
                                    cx.background_executor().timer(Duration::from_millis(50)).await;
                                }
                                assert!(ready, "刷新验证超时");
                                let _ = this.update(cx, |app, _| app.debug_refresh_finished(true));
                            }
                            if std::env::var("CLAUDESKTOP_SETTINGS_PREVIEW").as_deref() == Ok("kernel-status") {
                                for refreshed in [false, true] {
                                    let mut ready = false;
                                    for _ in 0..600 {
                                        ready = this.update(cx, |app, _| app.debug_kernel_status_ready()).unwrap_or(false);
                                        if ready { break; }
                                        cx.background_executor().timer(Duration::from_millis(50)).await;
                                    }
                                    assert!(ready, "内核检测验证超时");
                                    this.update_in(cx, |app, window, cx| app.debug_check_kernel_status(refreshed, window, cx)).unwrap();
                                }
                            }
                            if let Some(delta) = std::env::var("CLAUDESKTOP_SCREENSHOT_SCROLL").ok().and_then(|s| s.parse::<f32>().ok()) {
                                let _ = cx.update(|window, cx| {
                                    let viewport = window.viewport_size();
                                    window.dispatch_event(PlatformInput::ScrollWheel(ScrollWheelEvent {
                                        position: point(viewport.width * 0.65, viewport.height * 0.45),
                                        delta: ScrollDelta::Pixels(point(px(0.), px(-delta))),
                                        modifiers: Modifiers::default(), touch_phase: TouchPhase::Moved,
                                    }), cx);
                                });
                            }
                            if std::env::var_os("CLAUDESKTOP_STRESS").is_some() {
                                let mut work = Vec::new();
                                for step in 0..300 {
                                    let _ = cx.update(|window, cx| {
                                        let viewport = window.viewport_size();
                                        window.dispatch_event(
                                            PlatformInput::ScrollWheel(ScrollWheelEvent {
                                                position: point(viewport.width * 0.65, viewport.height * 0.45),
                                                delta: ScrollDelta::Lines(point(0., if step < 150 { -3. } else { 3. })),
                                                modifiers: Modifiers::default(),
                                                touch_phase: TouchPhase::Moved,
                                            }),
                                            cx,
                                        );
                                        let start = std::time::Instant::now();
                                        window.refresh();
                                        window.draw(cx).clear(cx);
                                        work.push(start.elapsed().as_secs_f64() * 1000.);
                                    });
                                }
                                work.sort_by(f64::total_cmp);
                                if !work.is_empty() {
                                    let percentile = |p: f64| work[((work.len() - 1) as f64 * p) as usize];
                                    let report = format!(
                                        "headless frames={} draw p50={:.2}ms p95={:.2}ms max={:.2}ms\n",
                                        work.len(), percentile(0.5), percentile(0.95), percentile(1.),
                                    );
                                    if let Ok(output) = std::env::var("CLAUDESKTOP_STRESS_OUT") {
                                        let _ = std::fs::write(output, &report);
                                    }
                                    eprint!("{report}");
                                }
                            }
                            // 内存测量：隐藏窗口多保持一段时间，覆盖后台索引与空闲状态。
                            if let Some(ms) = std::env::var("CLAUDESKTOP_SCREENSHOT_HOLD_MS")
                                .ok()
                                .and_then(|v| v.parse::<u64>().ok())
                            {
                                cx.background_executor().timer(Duration::from_millis(ms)).await;
                            }
                            let result = cx.update(|window, cx| {
                                window.refresh();
                                window.draw(cx).clear(cx);
                                window
                                    .render_to_image()
                                    .and_then(|image| image.save(&path).map_err(Into::into))
                            });
                            if let Err(error) = result.and_then(|result| result) {
                                eprintln!("截图失败：{error:#}");
                            }
                            let _ = cx.update(|_, cx| cx.quit());
                            return;
                        }
                        // 压测钩子：CLAUDESKTOP_STRESS=<行数>，灌数据后逐帧滚动（先往上 150 帧再往下 150 帧，
                        // 每帧画完才滚下一段，量的是真实帧间隔；不用计时器驱动，Windows 计时器按 15.6ms 一档）。
                        // 加 CLAUDESKTOP_STRESS_BUSY=1 则不滚，置忙 5 秒看转圈常驻时每秒重绘几次；
                        // 加 CLAUDESKTOP_STRESS_TYPE=1 则改成逐帧往输入框里打字。
                        // 加 CLAUDESKTOP_STRESS_STREAM=1 则改成逐帧推一个 AgentMessageChunk（流式输出）。
                        // 加 CLAUDESKTOP_STRESS_VIEW=settings:N / home / requests / requests-all 则不灌对话，
                        // 改成逐帧滚设置页第 N 个分类 / 首页概览 / 首页「请求」页签
                        // （灌 <行数> 轮假请求，只列第一页 / 全部列出来）。
                        // 统计写到 CLAUDESKTOP_STRESS_OUT（默认 stderr）
                        // 压测钩子：CLAUDESKTOP_STRESS_SESSIONS=<会话数>，生成 N 个历史会话并逐轮激活，
                        // 测冷会话回收的内存差异；CLAUDESKTOP_STRESS_ROUNDS 默认 3。
                        if let Some(n) = std::env::var("CLAUDESKTOP_STRESS_SESSIONS")
                            .ok()
                            .and_then(|v| v.parse::<usize>().ok())
                        {
                            let rounds = std::env::var("CLAUDESKTOP_STRESS_ROUNDS")
                                .ok()
                                .and_then(|v| v.parse::<usize>().ok())
                                .unwrap_or(3);
                            let _ = this.update(cx, |app, cx| app.debug_stress_sessions(n, rounds, cx));
                            return;
                        }
                        if let Some(n) = std::env::var("CLAUDESKTOP_STRESS")
                            .ok()
                            .and_then(|v| v.parse::<usize>().ok())
                        {
                            let busy = std::env::var_os("CLAUDESKTOP_STRESS_BUSY").is_some();
                            let typing = std::env::var_os("CLAUDESKTOP_STRESS_TYPE").is_some();
                            let streaming =
                                std::env::var_os("CLAUDESKTOP_STRESS_STREAM").is_some();
                            let view = std::env::var("CLAUDESKTOP_STRESS_VIEW").ok();
                            let _ = this.update_in(cx, |app, window, cx| match &view {
                                Some(view) => app.debug_stress_view(view, n, window, cx),
                                None => app.debug_stress(n, busy, cx),
                            });
                            cx.background_executor().timer(Duration::from_millis(1500)).await;
                            let _ = this.update_in(cx, |app, window, cx| {
                                app.frame_log = Some(Vec::new());
                                if view.is_some() {
                                    return;
                                }
                                if streaming {
                                    app.debug_stream_frames(0, window, cx);
                                } else if typing {
                                    app.debug_type_frames(0, window, cx);
                                } else if !busy {
                                    app.debug_scroll_frames(0, window, cx);
                                }
                            });
                            if view.is_some() {
                                let weak = this.clone();
                                let _ = cx.update(|window, cx| {
                                    ChatApp::debug_wheel_frames(weak, 0, window, cx)
                                });
                            }
                            if busy {
                                cx.background_executor().timer(Duration::from_secs(5)).await;
                                let _ = this.update(cx, |app, _| app.stress_report());
                            }
                        }
                        // 调试钩子：CLAUDESKTOP_RERUN = builtin / acp，
                        // 等 CLAUDESKTOP_RERUN_DELAY_MS（默认 10s）后把首条提示词放到该内核上重跑
                        let rerun =
                            std::env::var("CLAUDESKTOP_RERUN").ok().and_then(|v| match v.trim() {
                                "builtin" => Some(KernelChoice::Builtin),
                                "acp" => Some(KernelChoice::Acp("custom-1".into())),
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
            if !quiet {
                cx.activate(true);
            }
        });
}
