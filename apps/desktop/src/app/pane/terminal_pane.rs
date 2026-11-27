//! 终端 Pane：`portable-pty`（Windows 上就是 ConPTY）+ `alacritty_terminal` 的终端状态机。
//!
//! 结构分两层：
//!
//! - [`PtySession`]：进程 + PTY + 解析线程 + 终端网格。读线程只做 `read → vte 解析 → 置脏标`，
//!   不碰 UI；UI 侧按 ~16ms 合并重画（见 [`TerminalPane::render`] 里的节拍），**绝不每包数据都 notify**。
//! - [`TerminalPane`]：把网格画成一行行文本，并把键盘输入写回 PTY。
//!
//! 几条硬约束（见 `docs/dev/kernel-and-mcp.md` 与 HANDOFF）：
//!
//! - 不引 crates.io 的 `gpui-terminal`：它依赖上游 `gpui`，和 vendor 的 `gpui-pre` 冲突；
//! - 子进程由 [`claudesktop_core::process_guard`] 的 Job Object 兜底（主进程退出即整棵子树结束），
//!   关掉 Pane 时再显式 `kill` 一次；
//! - 回滚缓冲上限 [`SCROLLBACK`] 行，单个终端内存控制在 6–12 MiB；
//! - 尺寸变化（拖分割条）按 [`RESIZE_DEBOUNCE`] 去抖，别每帧都 `ioctl` 一次。

use super::{ClaudesktopPane, PaneKind};
use crate::app::ChatApp;
use crate::theme::rgb;
use crate::theme::*;
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte;
use gpui_base::dock::{Panel, PanelEvent, PanelState};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::*;
use gpui_kit::*;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// 回滚缓冲上限（行）。5000 行 @ 120 列大约 6–12 MiB，和 HANDOFF 的内存预算一致
const SCROLLBACK: usize = 5_000;
/// 拖分割条时的 resize 去抖
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(50);
/// 脏标合并的最小间隔：一次 stdout 刷出几百行也只重画一帧
const FRAME_INTERVAL: Duration = Duration::from_millis(16);
/// 单元高度（rem 的倍数），宽度按 0.6 个字号估（等宽字体）
const CELL_H_EM: f32 = 1.35;
const CELL_W_EM: f32 = 0.6;

/// 终端尺寸：`Term` 只关心行列数
#[derive(Clone, Copy)]
struct TermSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// 一屏的内容：每行若干段（同色的连续单元合并成一段），外加光标位置
pub(crate) struct Screen {
    pub(crate) rows: Vec<Vec<Run>>,
    pub(crate) cursor: (usize, usize),
}

/// 一段同色的文字
pub(crate) struct Run {
    pub(crate) text: String,
    pub(crate) color: alacritty_terminal::vte::ansi::Color,
}

/// 一个 PTY 会话：进程 + 解析线程 + 网格。关掉 Pane 时 `Drop` 会杀子进程。
pub(crate) struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn std::io::Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    term: Arc<std::sync::Mutex<Term<VoidListener>>>,
    dirty: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
    cols: u16,
    rows: u16,
}

impl PtySession {
    /// 在 `cwd` 里起一个 shell。`cols` / `rows` 是初始网格大小。
    pub(crate) fn spawn(cwd: &Path, cols: u16, rows: u16) -> std::io::Result<Self> {
        let pair = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|err| std::io::Error::other(err.to_string()))?;
        let mut command = CommandBuilder::new(shell_program());
        command.cwd(cwd);
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|err| std::io::Error::other(err.to_string()))?;
        // slave 这一端交给子系统就够了，我们自己不留（留着窗口关不掉）
        drop(pair.slave);
        let writer =
            pair.master.take_writer().map_err(|err| std::io::Error::other(err.to_string()))?;
        let mut reader =
            pair.master.try_clone_reader().map_err(|err| std::io::Error::other(err.to_string()))?;

        let config = Config { scrolling_history: SCROLLBACK, ..Config::default() };
        let term = Arc::new(std::sync::Mutex::new(Term::new(
            config,
            &TermSize { cols: cols as usize, rows: rows as usize },
            VoidListener,
        )));
        let dirty = Arc::new(AtomicBool::new(true));
        let exited = Arc::new(AtomicBool::new(false));

        // 解析线程：只读 + 置脏标，不碰 UI
        let reader_term = term.clone();
        let reader_dirty = dirty.clone();
        let reader_exited = exited.clone();
        std::thread::spawn(move || {
            let mut processor = vte::ansi::Processor::<vte::ansi::StdSyncHandler>::new();
            let mut buf = [0u8; 8 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        reader_exited.store(true, Ordering::Relaxed);
                        reader_dirty.store(true, Ordering::Relaxed);
                        return;
                    }
                    Ok(n) => {
                        if let Ok(mut term) = reader_term.lock() {
                            processor.advance(&mut *term, &buf[..n]);
                        }
                        reader_dirty.store(true, Ordering::Relaxed);
                    }
                }
            }
        });

        Ok(Self { master: pair.master, writer, child, term, dirty, exited, cols, rows })
    }

    /// 有变化才重画：UI 侧每帧问一次，没变就不通知
    pub(crate) fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::Relaxed)
    }

    pub(crate) fn exited(&self) -> bool {
        self.exited.load(Ordering::Relaxed)
    }

    /// 当前一屏（含回滚位置）。锁的是解析线程那份 `Term`，只在这几毫秒里持有
    pub(crate) fn screen(&self) -> Option<Screen> {
        let term = self.term.lock().ok()?;
        let mut rows: Vec<Vec<Run>> = Vec::with_capacity(self.rows as usize);
        let mut current: Vec<Run> = Vec::new();
        let mut line = 0usize;
        for indexed in term.grid().display_iter() {
            let point = indexed.point;
            let cell = indexed.cell;
            if point.line.0 + 1 != line as i32 {
                // 新的一行（display_iter 是逐行逐列的）
                if !current.is_empty() || !rows.is_empty() {
                    rows.push(std::mem::take(&mut current));
                }
                line = (point.line.0 + 1) as usize;
            }
            match current.last_mut() {
                Some(run) if run.color == cell.fg => run.text.push(cell.c),
                _ => current.push(Run { text: cell.c.to_string(), color: cell.fg }),
            }
        }
        if !current.is_empty() || rows.is_empty() {
            rows.push(current);
        }
        let cursor = term.grid().cursor.point;
        Some(Screen { rows, cursor: (cursor.line.0.max(0) as usize, cursor.column.0) })
    }

    /// 把按键写进 PTY
    pub(crate) fn write(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    /// 网格尺寸变化：`master.resize` + `Term::resize` 一起做（少了任何一边都会错位）
    pub(crate) fn resize(&mut self, cols: u16, rows: u16) {
        if (cols, rows) == (self.cols, self.rows) || cols == 0 || rows == 0 {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        if let Ok(mut term) = self.term.lock() {
            term.resize(TermSize { cols: cols as usize, rows: rows as usize });
        }
        self.dirty.store(true, Ordering::Relaxed);
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // 关 Pane / 退出都要收掉子进程（Job Object 是主进程级的兜底，这里管单块 Pane）
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 用哪个 shell：优先 `pwsh`，退到 `powershell`，最后 `cmd`
fn shell_program() -> String {
    std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
}

pub(crate) struct TerminalPane {
    app: WeakEntity<ChatApp>,
    focus_handle: FocusHandle,
    session: Option<PtySession>,
    /// 上次重画的时刻（16ms 合并）
    last_frame: Instant,
    /// 上一帧量到的 Pane 尺寸（像素）
    size: (Pixels, Pixels),
    /// 上次 resize 的时刻（50ms 去抖）
    last_resize: Option<Instant>,
    /// 一屏单元尺寸（像素），按字号算
    cell: (Pixels, Pixels),
    /// 排着的那次「到点重画」（避免脏标期间反复 spawn）
    pending_frame: Option<Task<()>>,
}

impl TerminalPane {
    pub(crate) fn new(app: WeakEntity<ChatApp>, cx: &mut Context<Self>) -> Self {
        Self {
            app,
            focus_handle: cx.focus_handle(),
            session: None,
            last_frame: Instant::now() - FRAME_INTERVAL,
            size: (px(0.), px(0.)),
            last_resize: None,
            cell: (px(8.), px(18.)),
            pending_frame: None,
        }
    }

    /// 懒起会话：知道窗口字号与 Pane 尺寸之后才建，才能算对行列数
    fn ensure_session(&mut self, cols: u16, rows: u16, cwd: std::path::PathBuf) {
        if self.session.is_some() {
            return;
        }
        match PtySession::spawn(&cwd, cols.max(20), rows.max(4)) {
            Ok(session) => self.session = Some(session),
            Err(err) => eprintln!("[terminal] 起不来：{err}"),
        }
    }

    /// Pane 尺寸变了：按「单元尺寸」折算行列数，**50ms 去抖**后再写给 ConPTY
    /// （拖分割条时每帧都会来这里一次，不去抖等于每帧一次 ioctl）
    fn on_bounds(&mut self, width: Pixels, height: Pixels, cx: &mut Context<Self>) {
        if width <= px(0.) || height <= px(0.) {
            return;
        }
        self.size = (width, height);
        let due = self.last_resize.is_none_or(|at| at.elapsed() >= RESIZE_DEBOUNCE);
        if !due {
            return;
        }
        let cols = (f32::from(width) / f32::from(self.cell.0)).floor().max(20.) as u16;
        let rows = (f32::from(height) / f32::from(self.cell.1)).floor().max(4.) as u16;
        self.last_resize = Some(Instant::now());
        if let Some(session) = &mut self.session {
            session.resize(cols, rows);
        }
        cx.notify();
    }
}

impl ClaudesktopPane for TerminalPane {
    fn pane_kind(&self) -> PaneKind {
        PaneKind::Terminal
    }

    fn icon(&self, _cx: &App) -> IconAll {
        IconAll::Terminal
    }

    fn title(&self, _cx: &App) -> SharedString {
        PaneKind::Terminal.title()
    }

    fn on_window_changed(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        // 换窗口后字号 / DPI 可能变了：下一帧按新尺寸重算行列数（resize 自带去抖）
        self.last_resize = None;
    }
}

impl Panel for TerminalPane {
    fn panel_name(&self) -> &'static str {
        PaneKind::Terminal.panel_name()
    }

    fn dump(&self, _cx: &App) -> PanelState {
        super::pane_state(PaneKind::Terminal, serde_json::json!({ "kind": "terminal" }))
    }

    fn on_removed(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.session = None;
        self.pending_frame = None;
        cx.notify();
    }
}

impl EventEmitter<PanelEvent> for TerminalPane {}

impl Focusable for TerminalPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rem = window.rem_size();
        let font = TEXT_SMALL.to_pixels(rem);
        let cw = font * CELL_W_EM;
        let ch = font * CELL_H_EM;
        self.cell = (cw, ch);

        // 会话按「Pane 有多大」来建：先按字体估一个尺寸，之后由 resize 去抖修正
        let cwd =
            self.app.upgrade().map(|app| app.read(cx).session().cwd.clone()).unwrap_or_default();
        self.ensure_session(120, 30, cwd);

        // 脏标 → 每 16ms 最多重画一帧（一次 stdout 刷几百行也只画一帧）
        if let Some(session) = &self.session
            && session.take_dirty()
        {
            let since = self.last_frame.elapsed();
            if since >= FRAME_INTERVAL {
                self.last_frame = Instant::now();
                cx.notify();
            } else if self.pending_frame.is_none() {
                // 把这段时间里剩下的输出合并成一帧，到点再通知一次
                self.pending_frame = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(FRAME_INTERVAL - since).await;
                    let _ = this.update(cx, |pane, cx| {
                        pane.pending_frame = None;
                        pane.last_frame = Instant::now();
                        cx.notify();
                    });
                }));
            }
        }
        let exited = self.session.as_ref().is_some_and(|s| s.exited());
        let screen = self.session.as_ref().and_then(|s| s.screen());
        let mut body = v_flex().size_full().bg(rgb(CODE_BG)).px_3().py_2().font_family(MONO);
        if let Some(screen) = screen {
            for (row_ix, row) in screen.rows.iter().enumerate() {
                let mut line = div().h(ch).w_full().flex().items_center().whitespace_nowrap();
                if row.is_empty() {
                    line = line.child(" ".to_string());
                }
                for run in row {
                    line = line.child(
                        div().text_color(rgb(ansi_color(run.color))).child(run.text.clone()),
                    );
                }
                // 光标：用一条下划线示意（块状光标在等宽单元格里不好画，先这样）
                if screen.cursor.0 == row_ix {
                    line = line.child(div().w(px(2.)).h(ch * 0.8).bg(rgb(CORAL)));
                }
                body = body.child(line);
            }
        } else {
            body = body.child(
                div()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .child("还没有输出：点一下这里，然后敲命令"),
            );
        }
        if exited {
            body = body.child(
                div().pt_2().text_size(TEXT_META).text_color(rgb(INK_FAINT)).child("进程已退出"),
            );
        }
        div()
            .id("terminal-pane")
            .track_focus(&self.focus_handle)
            .size_full()
            .overflow_hidden()
            .cursor_text()
            .on_prepaint({
                let handle = cx.entity();
                move |bounds, _, cx| {
                    let (w, h) = (bounds.size.width, bounds.size.height);
                    handle.update(cx, |pane, cx| pane.on_bounds(w, h, cx));
                }
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.focus_handle.focus(window, cx);
                    cx.notify();
                }),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                let bytes = key_bytes(&event.keystroke);
                if bytes.is_empty() {
                    return;
                }
                if let Some(session) = &mut this.session {
                    session.write(&bytes);
                }
                cx.notify();
            }))
            .child(body)
    }
}

impl ChatApp {
    /// `Ctrl+\``：起一块（或聚焦已有的）终端 Pane，落在当前会话的工作目录。
    pub(crate) fn open_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.focus_pane_of_kind(PaneKind::Terminal, window, cx) {
            let dock = self.dock.clone();
            dock.update(cx, |dock, cx| {
                if dock.has_dock(gpui_base::dock::DockPlacement::Bottom)
                    && !dock.is_dock_open(gpui_base::dock::DockPlacement::Bottom)
                {
                    dock.toggle_dock(gpui_base::dock::DockPlacement::Bottom, window, cx);
                }
            });
            return;
        }
        let app = cx.entity().downgrade();
        let pane = cx.new(|cx| TerminalPane::new(app, cx));
        let dock = self.dock.clone();
        let handle = std::sync::Arc::new(super::PaneHandle::new(pane))
            as std::sync::Arc<dyn gpui_base::dock::PanelView>;
        dock.update(cx, |dock, cx| {
            dock.add_panel_view(handle, gpui_base::dock::DockPlacement::Bottom, None, window, cx);
        });
        cx.notify();
    }
}

/// ANSI 颜色 → 主题色。前 16 色映射到 `theme.rs` 的语义色，其余退到墨水色
fn ansi_color(color: alacritty_terminal::vte::ansi::Color) -> crate::theme::Color {
    use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor};
    match color {
        AnsiColor::Named(named) => match named {
            NamedColor::Black | NamedColor::BrightBlack => INK_FAINT,
            NamedColor::Red | NamedColor::BrightRed => DANGER,
            NamedColor::Green | NamedColor::BrightGreen => GREEN,
            NamedColor::Yellow | NamedColor::BrightYellow => CORAL_DEEP,
            NamedColor::Blue | NamedColor::BrightBlue => INK_SOFT,
            NamedColor::Magenta | NamedColor::BrightMagenta => CORAL,
            NamedColor::Cyan | NamedColor::BrightCyan => GREEN,
            NamedColor::White | NamedColor::BrightWhite => INK,
            _ => INK,
        },
        _ => INK,
    }
}

/// GPUI 按键 → 终端字节。只覆盖常用键；IME / 组合键留给后续（见 HANDOFF）
fn key_bytes(keystroke: &Keystroke) -> Vec<u8> {
    let ctrl = keystroke.modifiers.control;
    let key = keystroke.key.as_str();
    let mut out = Vec::new();
    match key {
        "enter" => out.push(b'\r'),
        "tab" => out.push(b'\t'),
        "backspace" => out.push(0x7f),
        "escape" => out.push(0x1b),
        "up" => out.extend_from_slice(b"\x1b[A"),
        "down" => out.extend_from_slice(b"\x1b[B"),
        "right" => out.extend_from_slice(b"\x1b[C"),
        "left" => out.extend_from_slice(b"\x1b[D"),
        "home" => out.extend_from_slice(b"\x1b[H"),
        "end" => out.extend_from_slice(b"\x1b[F"),
        "delete" => out.extend_from_slice(b"\x1b[3~"),
        "space" => out.push(if ctrl { 0 } else { b' ' }),
        _ => {
            if ctrl {
                // Ctrl+字母 → 1..=26
                if let Some(ch) = key.chars().next()
                    && ch.is_ascii_alphabetic()
                {
                    out.push(ch.to_ascii_lowercase() as u8 - b'a' + 1);
                }
            } else if let Some(ch) = keystroke.key_char.as_ref().and_then(|s| s.chars().next()) {
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out
}
