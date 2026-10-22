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
mod complete;
mod demo;
mod fonts;
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
    let t0 = std::time::Instant::now();
    let bench = std::env::var("CLAUDESKTOP_BENCH_STARTUP").is_ok();
    let _ = STARTUP_T0.set(t0);
    log_panics();

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

        // 静默模式（CLAUDESKTOP_QUIET=1，或跑压测时自动开启）：窗口照常显示但不抢前台、不激活，
        // 不打断用户手头正在做的事；截图脚本靠 PrintWindow 抓内容，不需要窗口在前台。
        let quiet = std::env::var_os("CLAUDESKTOP_QUIET").is_some()
            || std::env::var_os("CLAUDESKTOP_STRESS").is_some();

        let mut options = TitleBar::window_options();
        options.focus = !quiet;
        if quiet {
            // 非活动窗口默认限到 30 帧，压测量到的就不是真实帧间隔了
            options.inactive_frame_interval = None;
        }
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
                        // 调试钩子：CLAUDESKTOP_OPEN = settings（打开设置页；settings:N 打开第 N 个分类）
                        // / fetch（并获取模型列表）/ restart（重启内核）/ review（审查页）
                        // / rename、ask-delete、delete（对侧栏第 2 个会话）
                        let open = std::env::var("CLAUDESKTOP_OPEN").unwrap_or_default();
                        match open.as_str() {
                            "settings" => app.open_settings(window, cx),
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
                            // 侧栏第 2 个会话最新一轮的审查页；review:N 再把第 N 个文件的第一块撤回
                            o if o.starts_with("review") => {
                                let file = o.strip_prefix("review:").and_then(|n| n.parse().ok());
                                app.debug_review(file, cx)
                            }
                            "restart" => {
                                let kind = app.session().kernel.clone();
                                app.restart_kernel(&kind, cx)
                            }
                            // 在一个临时 git 仓库里建 worktree（截图 / 自检用）
                            "worktree" => app.debug_worktree(cx),
                            _ => {}
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
                    // 压测钩子：CLAUDESKTOP_STRESS=<行数>，灌数据后逐帧滚动（先往上 150 帧再往下 150 帧，
                    // 每帧画完才滚下一段，量的是真实帧间隔；不用计时器驱动，Windows 计时器按 15.6ms 一档）。
                    // 加 CLAUDESKTOP_STRESS_BUSY=1 则不滚，置忙 5 秒看转圈常驻时每秒重绘几次；
                    // 加 CLAUDESKTOP_STRESS_TYPE=1 则改成逐帧往输入框里打字。
                    // 加 CLAUDESKTOP_STRESS_VIEW=settings:N / home / requests / requests-all 则不灌对话，
                    // 改成逐帧滚设置页第 N 个分类 / 首页概览 / 首页「请求」页签
                    // （灌 <行数> 轮假请求，只列第一页 / 全部列出来）。
                    // 统计写到 CLAUDESKTOP_STRESS_OUT（默认 stderr）
                    if let Some(n) = std::env::var("CLAUDESKTOP_STRESS")
                        .ok()
                        .and_then(|v| v.parse::<usize>().ok())
                    {
                        let busy = std::env::var_os("CLAUDESKTOP_STRESS_BUSY").is_some();
                        let typing = std::env::var_os("CLAUDESKTOP_STRESS_TYPE").is_some();
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
                            if typing {
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
                    // 调试钩子：CLAUDESKTOP_RERUN = builtin / acp / demo，
                    // 等 CLAUDESKTOP_RERUN_DELAY_MS（默认 10s）后把首条提示词放到该内核上重跑
                    let rerun =
                        std::env::var("CLAUDESKTOP_RERUN").ok().and_then(|v| match v.trim() {
                            "builtin" => Some(KernelChoice::Builtin),
                            "acp" => Some(KernelChoice::Acp("custom-1".into())),
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
        if !quiet {
            cx.activate(true);
        }
    });
}
