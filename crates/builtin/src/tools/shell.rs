//! shell 执行：命令包装、超时、输出裁剪，以及后台任务入口。

use super::files::*;
use super::*;

/// 规则是否覆盖这条 shell 命令（按词前缀匹配，复合命令不匹配）。
pub fn shell_rule_matches(rule: &str, command: &str) -> bool {
    if is_compound(command) {
        return false;
    }
    let want: Vec<&str> = rule.split_whitespace().collect();
    let got: Vec<&str> = command.split_whitespace().collect();
    !want.is_empty()
        && got.len() >= want.len()
        && want.iter().zip(&got).all(|(w, g)| w.eq_ignore_ascii_case(g))
}

pub(super) fn is_compound(command: &str) -> bool {
    command.contains([';', '&', '|', '>', '<', '`', '\n', '\r']) || command.contains("$(")
}

/// `git status -s` → `git status`；`cargo test -p x` → `cargo test`；`ls -la` → `ls`
pub(super) fn shell_rule(command: &str) -> Option<String> {
    if is_compound(command) {
        return None;
    }
    let mut words = command.split_whitespace();
    let program = words.next()?;
    let sub = words.next().filter(|w| {
        w.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') && !w.starts_with('-')
    });
    Some(match sub {
        Some(sub) => format!("{program} {sub}"),
        None => program.to_string(),
    })
}

/// `shell_background`：起一条后台命令，立刻回任务 ID。
pub(super) fn start_background(command: &str, cwd: &Path) -> ToolOutput {
    match crate::bgtasks::start(command, cwd) {
        Ok(id) => ToolOutput::ok(
            format!(
                "Started {id} in the background. Read its output with `shell_output` \
                 {{\"id\":\"{id}\"}} (it waits for new output); stop it with `shell_stop`."
            ),
            format!("后台 {id}"),
        ),
        Err(e) => ToolOutput::fail(e, "无法启动后台命令"),
    }
}

/// shell 工具说明的补充：Windows 上应用退出时会连带结束它拉起的整棵进程树
/// （`claudesktop_core::process_guard`），`start` 出去的也逃不掉。要比应用活得久的常驻服务
/// （隧道、本地网关）得用 WMI 拉起，不然用户关掉应用重编译时服务跟着断。
pub(super) const DETACH_HINT: &str = if cfg!(windows) {
    " Every process started here, including ones launched with `start` or `Start-Process`, is \
     ended when claudesktop exits. To start a long-running service that must outlive the app \
     (an SSH tunnel, a local server the user keeps using), launch it outside the app's process \
     tree with WMI: `Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments \
     @{ CommandLine = '<command>' }`."
} else {
    ""
};

/// 给模型看的 shell 名称
pub(super) fn shell_name() -> &'static str {
    if cfg!(windows) {
        if has_pwsh() { "PowerShell 7 (pwsh)" } else { "Windows PowerShell 5.1 (no `&&`; use `;`)" }
    } else {
        "sh"
    }
}

#[cfg(windows)]
pub(super) fn has_pwsh() -> bool {
    use std::sync::OnceLock;
    static FOUND: OnceLock<bool> = OnceLock::new();
    *FOUND.get_or_init(|| {
        std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("pwsh.exe").is_file()))
    })
}

#[cfg(not(windows))]
pub(super) fn has_pwsh() -> bool {
    false
}

/// 跑命令时默认带上的环境变量：常见的会弹窗口 / 开浏览器的工具改成不弹（用户自己设了的不覆盖）。
pub(super) const QUIET_ENV: [(&str, &str); 4] = [
    // Playwright 测试失败后默认会打开 HTML 报告
    ("PLAYWRIGHT_HTML_OPEN", "never"),
    ("PW_TEST_HTML_REPORT_OPEN", "never"),
    // create-react-app 等开发服务器启动时开浏览器
    ("BROWSER", "none"),
    // matplotlib 画图不弹窗口（savefig 照常）
    ("MPLBACKEND", "Agg"),
];

pub(super) fn shell_command(command: &str) -> Command {
    let mut cmd = shell_command_raw(command);
    for (key, value) in QUIET_ENV {
        if std::env::var_os(key).is_none() {
            cmd.env(key, value);
        }
    }
    cmd
}

/// 后台命令用同一套 shell / 编码 / 不弹窗设置（`bgtasks` 模块调）。
pub(crate) fn background_command(command: &str) -> Command {
    shell_command(command)
}

pub(super) fn shell_command_raw(command: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let exe = if has_pwsh() { "pwsh.exe" } else { "powershell.exe" };
        let mut cmd = Command::new(exe);
        // 管道输出默认走 OEM 代码页（中文系统是 GBK），强制 UTF-8
        cmd.args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(format!(
                "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; $OutputEncoding=[System.Text.Encoding]::UTF8; {command}"
            ))
            .creation_flags(CREATE_NO_WINDOW);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    }
}

/// 连同子孙进程一起结束（Windows 上 `kill` 只结束 shell 本身）
pub(crate) fn kill_tree(child: &mut std::process::Child) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000)
            .status();
    }
    let _ = child.kill();
}

/// 界面里不经模型直接跑的命令（worktree 的初始化）：和 `run_shell` 同一套 shell、不弹窗、
/// 边读边攒输出；在 unix 上自成一个进程组，超时连同 `npm` / `node` 这些子孙一起结束。
pub fn run_command(command: &str, cwd: &Path, timeout: Duration) -> ToolOutput {
    let cmd = shell_command(command);
    #[cfg(unix)]
    let cmd = {
        let mut cmd = cmd;
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        cmd
    };
    run_shell_in(cmd, cwd, timeout, &|| false, &|_| {}, cfg!(unix))
}

/// 结束整个进程组（`own_group` = 子进程是用 `process_group(0)` 起的组长）。
pub(super) fn kill_group(child: &mut std::process::Child, own_group: bool) {
    #[cfg(unix)]
    if own_group {
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = own_group;
    kill_tree(child);
}

pub(super) fn run_shell(
    command: &str,
    cwd: &Path,
    timeout: Duration,
    should_stop: &dyn Fn() -> bool,
    progress: &dyn Fn(String),
) -> ToolOutput {
    run_shell_in(shell_command(command), cwd, timeout, should_stop, progress, false)
}

pub(super) fn run_shell_in(
    mut cmd: Command,
    cwd: &Path,
    timeout: Duration,
    should_stop: &dyn Fn() -> bool,
    progress: &dyn Fn(String),
    own_group: bool,
) -> ToolOutput {
    let mut child = match cmd
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return ToolOutput::fail(format!("Failed to start shell: {e}"), "无法启动 shell"),
    };
    // 边读边攒：后台进程继承了管道时读不到 EOF，退出后最多再等一会儿就取已读到的部分
    let drain = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        let buf: Arc<Mutex<Vec<u8>>> = Arc::default();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let sink = buf.clone();
        std::thread::spawn(move || {
            if let Some(mut pipe) = pipe {
                let mut chunk = [0u8; 8192];
                while let Ok(n) = pipe.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    let mut b = sink.lock().unwrap();
                    let room = PIPE_MAX_BYTES.saturating_sub(b.len());
                    b.extend_from_slice(&chunk[..n.min(room)]);
                }
            }
            let _ = done_tx.send(());
        });
        (buf, done_rx)
    };
    let stdout = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>));
    let stderr = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>));

    // 运行中的输出：每 [`LIVE_EVERY`] 看一次，有新内容就报（只取两路缓冲的末尾一截来解码）
    let live = |seen: &mut (usize, usize)| {
        let (out, err) = (stdout.0.lock().unwrap(), stderr.0.lock().unwrap());
        if (out.len(), err.len()) == *seen {
            return;
        }
        *seen = (out.len(), err.len());
        let tail = |b: &[u8]| {
            String::from_utf8_lossy(&b[b.len().saturating_sub(LIVE_TAIL_BYTES)..]).into_owned()
        };
        let mut text = tail(&out);
        if !err.is_empty() {
            let _ = write!(text, "\n[stderr]\n{}", tail(&err));
        }
        drop((out, err));
        progress(claudesktop_core::output_tail_preview(&text));
    };
    let mut seen = (0, 0);
    let mut last_live = Instant::now();

    let started = Instant::now();
    let mut interrupted = None;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }
        if last_live.elapsed() >= LIVE_EVERY {
            last_live = Instant::now();
            live(&mut seen);
        }
        if should_stop() {
            interrupted = Some("cancelled");
        } else if started.elapsed() >= timeout {
            interrupted = Some("timed out");
        }
        if interrupted.is_some() {
            kill_group(&mut child, own_group);
            break child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(30));
    };
    let collect = |(buf, done): (Arc<Mutex<Vec<u8>>>, mpsc::Receiver<()>)| {
        let _ = done.recv_timeout(Duration::from_millis(1500));
        let bytes = std::mem::take(&mut *buf.lock().unwrap());
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let stdout = collect(stdout);
    let stderr = collect(stderr);

    let mut out = stdout.trim_end().to_string();
    if !stderr.trim().is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        let _ = write!(out, "[stderr]\n{}", stderr.trim_end());
    }
    let code = status.and_then(|s| s.code());
    if let Some(why) = interrupted {
        let _ = write!(out, "\n[command {why} after {}s]", started.elapsed().as_secs());
        return ToolOutput::fail(
            clip_output(out),
            if why == "cancelled" { "已中断" } else { "超时" },
        );
    }
    let first_line =
        |s: &str| s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
    match code {
        Some(0) => {
            let summary = first_line(&stdout);
            if out.is_empty() {
                out.push_str("(no output)");
            }
            ToolOutput::ok(
                clip_output(out),
                if summary.is_empty() { "完成".into() } else { summary },
            )
        }
        code => {
            let code = code.map_or("unknown".into(), |c| c.to_string());
            let _ = write!(out, "\n[exit code: {code}]");
            let detail = first_line(&stderr);
            ToolOutput::fail(
                clip_output(out),
                if detail.is_empty() {
                    format!("exit {code}")
                } else {
                    format!("exit {code} · {detail}")
                },
            )
        }
    }
}
