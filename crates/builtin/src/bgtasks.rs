//! 后台命令：`shell_background` 起一条长命令立刻返回任务 ID，`shell_output` 按 ID
//! 增量取输出（可以等一下新输出），`shell_stop` 结束它。
//!
//! 表是**进程级**的（和 `websearch` 的数据目录一样用 `OnceLock` 存放）：`Action::run`
//! 拿不到内核的 `Inner`，工具执行需要一个能从任何线程访问的注册表。每个内核启动时
//! 经 [`register`] 登记一个出口，任务状态变化就整份发一次 `BackgroundTasks`
//! （同时有几个内核时各自收到各自会话的任务，见 [`views_for`]）。
//! 应用的进程树在退出时被 `process_guard` 一并结束，所以这里不用自己管退出清理；
//! 输出有上限（只留最近的 [`OUTPUT_MAX_BYTES`]），完成的旧任务在表满时先丢。

use claudesktop_core::BackgroundTaskView;
use std::collections::BTreeMap;
use std::io::Read;
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 每个任务留多少输出（字节，超出丢最前面的）
const OUTPUT_MAX_BYTES: usize = 256 * 1024;
/// 表里最多留几个任务（运行中的不丢，已完成的先丢最老的）
const MAX_TASKS: usize = 16;
/// `shell_output` 最多等新输出多久
pub const WAIT_MAX_SECS: u64 = 30;

/// 一个后台任务。
struct Task {
    id: String,
    command: String,
    /// 会话 ID：界面上按会话分组，会话换了就不显示别的会话的任务
    session: String,
    /// 起它的内核（同时有几个内核时事件只发给自己的）
    kernel: u64,
    started: Instant,
    /// 回收线程只在 `try_wait` 的一瞬间拿锁，`stop` 随时能拿到它去结束进程
    child: Arc<Mutex<Child>>,
    output: Arc<Mutex<Output>>,
    state: Arc<Mutex<State>>,
    /// `shell_output` 上次读到哪（从开头算的绝对字节数，含已丢掉的部分）
    cursor: Mutex<usize>,
}

/// 输出缓冲：只留最近的 [`OUTPUT_MAX_BYTES`]，`dropped` 记前面丢了多少，
/// 绝对位置 = `dropped + buf` 里的下标（增量读取按绝对位置算，缓冲满了也能接着读）。
#[derive(Default)]
struct Output {
    buf: Vec<u8>,
    dropped: usize,
}

impl Output {
    /// 一共写进来多少字节（含已丢的）
    fn end(&self) -> usize {
        self.dropped + self.buf.len()
    }

    fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
        if self.buf.len() > OUTPUT_MAX_BYTES {
            let cut = self.buf.len() - OUTPUT_MAX_BYTES;
            self.buf.drain(..cut);
            self.dropped += cut;
        }
    }

    /// 从绝对位置 `from` 起的输出；`from` 落在已丢的区间里时从留着的开头给，
    /// 第二个值是中间丢了多少字节。
    fn since(&self, from: usize) -> (&[u8], usize) {
        let skipped = self.dropped.saturating_sub(from);
        let start = from.saturating_sub(self.dropped).min(self.buf.len());
        (&self.buf[start..], skipped)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Running,
    /// 退出码；None = 被信号 / 未知
    Exited(Option<i32>),
}

/// 一个内核的事件出口：任务列表变了就调它。
type Sink = Box<dyn Fn(Vec<BackgroundTaskView>) + Send + Sync>;

struct Table {
    tasks: Mutex<BTreeMap<String, Arc<Task>>>,
    next: Mutex<u64>,
    /// 事件出口（每个内核一个，带内核 ID：同时跑几个内核时各自收自己的）
    sinks: Mutex<Vec<(u64, Sink)>>,
}

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| Table {
        tasks: Mutex::default(),
        next: Mutex::new(1),
        sinks: Mutex::default(),
    })
}

/// 内核启动时登记一个出口：`kernel` 是内核自己的编号（同一进程里几个内核各不同），
/// 收到的只有它拉起的那些任务。
pub fn register(kernel: u64, sink: Box<dyn Fn(Vec<BackgroundTaskView>) + Send + Sync>) {
    table().sinks.lock().unwrap().push((kernel, sink));
}

/// 内核关闭时：停掉它起的任务、摘掉它的出口（出口里攥着内核，不摘内核就一直释放不掉）。
pub fn unregister(kernel: u64) {
    table().sinks.lock().unwrap().retain(|(k, _)| *k != kernel);
    let ids: Vec<String> = table()
        .tasks
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, t)| t.kernel == kernel)
        .map(|(id, _)| id.clone())
        .collect();
    for id in ids {
        let _ = stop(&id);
    }
    table().tasks.lock().unwrap().retain(|_, t| t.kernel != kernel);
}

/// 只通知某个内核（它的任务变了）；`None` = 全部（启动 / 停止时用）。
fn notify_kernel(only: Option<u64>) {
    let sinks = table().sinks.lock().unwrap();
    for (kernel, sink) in sinks.iter() {
        if only.is_none_or(|k| k == *kernel) {
            sink(views_for(*kernel));
        }
    }
}

thread_local! {
    /// 正在跑工具的内核与会话（`Action::run` 拿不到，内核执行前设一下）
    static CURRENT: std::cell::RefCell<(u64, String)> =
        const { std::cell::RefCell::new((0, String::new())) };
}

/// 内核执行工具前设置：之后起的后台任务记在这个内核的会话名下。
pub fn set_current(kernel: u64, session: &str) {
    CURRENT.with(|c| *c.borrow_mut() = (kernel, session.to_string()));
}

/// 某个内核拉起的任务（事件里只带自己的）。
fn views_for(kernel: u64) -> Vec<BackgroundTaskView> {
    views_filtered(Some(kernel))
}

fn views_filtered(kernel: Option<u64>) -> Vec<BackgroundTaskView> {
    let tasks = table().tasks.lock().unwrap();
    let mut out: Vec<BackgroundTaskView> = tasks
        .values()
        .filter(|task| kernel.is_none_or(|k| task.kernel == k))
        .map(|task| {
            let state = *task.state.lock().unwrap();
            let output = task.output.lock().unwrap();
            BackgroundTaskView {
                id: task.id.clone(),
                session: task.session.clone(),
                command: task.command.clone(),
                running: state == State::Running,
                exit_code: match state {
                    State::Exited(code) => code,
                    State::Running => None,
                },
                duration_secs: task.started.elapsed().as_secs(),
                output_tail: tail(&output.buf),
            }
        })
        .collect();
    out.sort_by(|a, b| b.id.cmp(&a.id));
    out
}

/// 输出末尾的几行（界面上的小卡片用）。
fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(3);
    lines[start..].join("\n")
}

/// 起一条后台命令，返回任务 ID。
pub fn start(command: &str, cwd: &std::path::Path) -> Result<String, String> {
    let mut child = crate::tools::background_command(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start shell: {e}"))?;
    let output: Arc<Mutex<Output>> = Arc::default();
    let state = Arc::new(Mutex::new(State::Running));
    // stdout 和 stderr 都往一个缓冲里写：模型看输出不关心是哪一个
    for pipe in [
        child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
        child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let sink = output.clone();
        std::thread::spawn(move || {
            let mut pipe = pipe;
            let mut chunk = [0u8; 8192];
            while let Ok(n) = pipe.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().push(&chunk[..n]);
            }
        });
    }
    let child = Arc::new(Mutex::new(child));
    let id = {
        let mut next = table().next.lock().unwrap();
        let id = format!("bg-{}", *next);
        *next += 1;
        id
    };
    let (kernel, session) = CURRENT.with(|c| c.borrow().clone());
    let task = Arc::new(Task {
        id: id.clone(),
        command: command.to_string(),
        session,
        kernel,
        started: Instant::now(),
        child: child.clone(),
        output,
        state: state.clone(),
        cursor: Mutex::new(0),
    });
    {
        let mut tasks = table().tasks.lock().unwrap();
        // 表满：先丢最早的已完成任务
        if tasks.len() >= MAX_TASKS {
            let done = tasks
                .iter()
                .find(|(_, t)| *t.state.lock().unwrap() != State::Running)
                .map(|(id, _)| id.clone());
            if let Some(id) = done {
                tasks.remove(&id);
            }
        }
        tasks.insert(id.clone(), task);
    }
    // 回收线程：等进程退出，记下退出码。轮询 `try_wait` 而不是阻塞 `wait`：
    // `wait` 要一直攥着 child 的锁，`stop` 就拿不到锁去结束进程了
    let owner = kernel;
    std::thread::spawn(move || {
        loop {
            let status = child.lock().unwrap().try_wait();
            match status {
                Ok(None) => std::thread::sleep(Duration::from_millis(200)),
                Ok(Some(status)) => {
                    let mut state = state.lock().unwrap();
                    // 被 `stop` 结束的已经记成 Exited(None)，不再改
                    if *state == State::Running {
                        *state = State::Exited(status.code());
                    }
                    break;
                }
                Err(_) => {
                    *state.lock().unwrap() = State::Exited(None);
                    break;
                }
            }
        }
        notify_kernel(Some(owner));
    });
    notify_kernel(Some(kernel));
    Ok(id)
}

/// 按 ID 取增量输出；`wait` 内没新输出也返回（附一句状态），`should_stop` 为真（用户停了这一轮）时立刻返回。
pub fn output(id: &str, wait: Duration, should_stop: &dyn Fn() -> bool) -> Result<String, String> {
    let task = get(id).ok_or_else(|| format!("没有这个后台任务：{id}"))?;
    let deadline = Instant::now() + wait;
    let cursor = *task.cursor.lock().unwrap();
    loop {
        let running = *task.state.lock().unwrap() == State::Running;
        let available = task.output.lock().unwrap().end();
        if available > cursor || !running || Instant::now() >= deadline || should_stop() {
            let (text, skipped, new_cursor) = {
                let out = task.output.lock().unwrap();
                let (bytes, skipped) = out.since(cursor);
                (String::from_utf8_lossy(bytes).into_owned(), skipped, out.end())
            };
            *task.cursor.lock().unwrap() = new_cursor;
            let exit = *task.state.lock().unwrap();
            let mut out = if text.trim().is_empty() {
                "(no new output)".to_string()
            } else {
                text.trim_end().to_string()
            };
            if skipped > 0 {
                out = format!("[{skipped} bytes of earlier output dropped]\n{out}");
            }
            match exit {
                State::Running => {
                    out.push_str(&format!(
                        "\n[still running, {}s]",
                        task.started.elapsed().as_secs()
                    ));
                }
                State::Exited(Some(code)) => {
                    out.push_str(&format!("\n[exit code: {code}]"));
                }
                State::Exited(None) => out.push_str("\n[exited]"),
            }
            return Ok(out);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 结束一个任务（连同子孙进程）。
pub fn stop(id: &str) -> Result<String, String> {
    let task = get(id).ok_or_else(|| format!("没有这个后台任务：{id}"))?;
    if *task.state.lock().unwrap() != State::Running {
        return Ok(format!("{} 早就结束了", task.id));
    }
    let mut child = task.child.lock().unwrap();
    crate::tools::kill_tree(&mut child);
    drop(child);
    *task.state.lock().unwrap() = State::Exited(None);
    let owner = task.kernel;
    notify_kernel(Some(owner));
    Ok(format!("已结束 {}（{}）", task.id, task.command))
}

/// 会话结束时把它的任务全停掉（会话没了，模型再也读不到输出）。
pub fn stop_session(session: &str) {
    let ids: Vec<String> = table()
        .tasks
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, t)| t.session == session)
        .map(|(id, _)| id.clone())
        .collect();
    for id in ids {
        let _ = stop(&id);
    }
}

fn get(id: &str) -> Option<Arc<Task>> {
    table().tasks.lock().unwrap().get(id).cloned()
}

/// 截到 `max` 个字符（给工具摘要用）。
pub(crate) fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn tail_keeps_the_last_lines() {
        let text = "a\n\nb\nc\nd\n";
        assert_eq!(tail(text.as_bytes()), "b\nc\nd");
    }

    /// 缓冲满了以后增量读取照样接得上：游标是绝对位置，不随丢掉前面的而失效
    #[test]
    fn output_cursor_survives_the_cap() {
        let mut out = Output::default();
        out.push(&vec![b'a'; OUTPUT_MAX_BYTES]);
        let cursor = out.end();
        out.push(b"new");
        assert_eq!(out.since(cursor), (&b"new"[..], 0));
        // 游标落在已丢的区间里：从留着的开头给，报告丢了多少
        let (rest, skipped) = out.since(0);
        assert_eq!(rest.len(), OUTPUT_MAX_BYTES);
        assert_eq!(skipped, 3);
    }

    /// 回收线程等进程退出时不能攥着锁：`stop` 得马上结束一个还在跑的任务
    #[cfg(unix)]
    #[test]
    fn stop_kills_a_running_task_promptly() {
        let id = start("sleep 30", &std::env::temp_dir()).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let begun = Instant::now();
        stop(&id).unwrap();
        assert!(begun.elapsed() < Duration::from_secs(5));
        let out = output(&id, Duration::from_secs(1), &|| false).unwrap();
        assert!(out.ends_with("[exited]"), "{out}");
    }

    #[test]
    fn clip_adds_an_ellipsis() {
        assert_eq!(clip("abcdef", 3), "abc…");
        assert_eq!(clip("ab", 3), "ab");
    }
}
