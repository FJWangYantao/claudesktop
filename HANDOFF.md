# HANDOFF：踩过的坑

接新任务前先读这一页；踩到新的坑就往对应小节加一行（是什么现象、为什么、怎么绕开）。
按主题分节，写完超过一屏就往下归档。

## 构建 / 测试

- **本机 cargo 在 `D:\dev\cargo\bin`，不在 PATH 里**（2026-11-25）：`cargo`/`rustfmt` 直接敲会报「无法将 cargo 项识别为 cmdlet」。先 `$env:Path="D:\dev\cargo\bin;$env:Path"` 再构建；目标目录固定 `$env:CARGO_TARGET_DIR="D:/t3"`，否则 `ring` 在项目内 `target` 下报 D8050。
- **lld 链接器要用 `rust-lld.exe`，不是 `gcc-ld\lld-link.exe`**（2026-11-25）：本机 `D:\dev\cargo\config.toml` 里配 `linker` 时，`lib/rustlib\x86_64-pc-windows-msvc\bin\gcc-ld\lld-link.exe` 是固定 link flavor 的构建，会忽略并拒绝 rustc 传来的 `-flavor link`（报 `could not open 'link'`）；同目录树的 `...\bin\rust-lld.exe` 是多路复用器，接受该参数。链接器配好后 debug PDB 从 660 MB 降到 120 MB。`[profile.dev]` 与 `[profile.dev.package."*"]` 都设 `debug = false` / `line-tables-only`；`D:/t3/debug` 清理旧增量后从 20.8 GB 降到约 3.8 GB。裁剪 tree-sitter feature 后 debug exe 117.5 → 107.6 MB，`syntax.rs` 的别名表对缺失语言自动跳过，不用改代码。
- **桌面端没有 lib target**：`cargo test -p claudesktop --lib` 报 "no library targets found"，单测入口是 `cargo test -p claudesktop --bin claudesktop`。
- **`Get-Content -Raw | Set-Content` 往返会永久损坏 UTF-8 源码**（2026-11-25）：PowerShell 5.1 的 `Get-Content` 默认按 ANSI / GBK（CP936）解码，中文变乱码；GBK 解不开的字节被替换成 `?` 并吞掉后一个字节（`— ` 变 `鈥?`），再写回 UTF-8 就丢了原文。本轮对 `vendor/gpui-base/src/text/node.rs` 做 `-replace` 时中招，编译报 `unknown start of token: \u{e15f}`。改源码一律用 Edit / Write 工具；非要用脚本就先 `[IO.File]::ReadAllText($p,[Text.UTF8Encoding]::new($false,$true))`。抢救办法：按 CP936 把乱码还原回字节，再把每处 `?` 用 `git show HEAD:file` 的上下文补齐（脚本留在 `D:/t3/repair_node.py` 可参考）。
- **PowerShell 的 `>` 重定向会写坏 UTF-8 源码**（2026-11-25）：`git show HEAD:file > out.rs` 之后的文件是 UTF-16 且带 BOM，粘回仓库会让整个文件变成二进制 diff。要还原被自己改过的文件用 `git restore --source=HEAD --worktree -- <path>`；读带中文的文件用 `[IO.File]::ReadAllText($p,[Text.Encoding]::UTF8)`，`Get-Content` 默认按 GBK 解码会出乱码。
- **`**加粗链接**` 星号外露在 2026-11-14 的 vendor/markdown 补丁里已经修好**（2026-11-25 复核）：用探针把 12 组 `**http://…**` 写法在有 / 无补丁下对比，输出完全一致；真正还漏网的是 `close_dangling_markup` 按单个反引号数奇偶配对行内代码（双反引号里夹单反引号时把代码里的 `**` 当成粗体标记），改用同长反引号串配对后修好。
- **D 盘满时可复用 Release 缓存换盘续编**（2026-11-25）：`gpui-component` 写 `.rlib` 报 Windows 错误 112 后，用 `robocopy /E /COPY:DAT /DCOPY:DAT` 把 `target/release` 复制到可写临时目录的 `release` 子目录（跳过锁文件、`*.rcgu.o` 和旧的桌面 exe / PDB），再用 `cargo build --release -p claudesktop --locked --offline --target-dir <临时目录>`。本次复用了已编好的依赖，普通 Release 构建通过，旧的 `target/release/claudesktop.exe` 仍是上一次的产物。
- **`CLAUDESKTOP_STRESS` 报完统计不会自己退出**（2026-11-24）：busy 模式 5 秒后 `stress_report()` 打完就留在前台（只有 `CLAUDESKTOP_STRESS_SESSIONS` 那条会定时退出）。脚本里跑要设 `CLAUDESKTOP_STRESS_OUT=<文件>`（默认写 stderr），`Start-Process -PassThru` 起进程、等够时间再 `Stop-Process`，否则管道一直挂着看不到结果。同一台机器上还开着正式实例时数字会被抢 CPU，最好先关。
- **Windows SDK 的 Lib / Include 曾被掏空，`setup.exe modify --add` 不修**（2026-11-24，已修复）：现象是编译卡在 `LNK1181 无法打开输入文件 kernel32.lib`，`C:\Program Files (x86)\Windows Kits\10\` 只剩 `bin` / `Redist` / `DesignTime`。此时 `vswhere -requires Microsoft.VisualStudio.Component.Windows11SDK.26100` 仍返回安装路径，`modify --add` 6 秒退出 0（组件登记着，MSI 的 `Lib`/`Include` 文件没了）。正确做法是跑 `setup.exe repair --installPath ...\2022\BuildTools --quiet --norestart`：它会先自更新 installer，再在后台进程里重装缺的 MSI；父进程立刻返回，要以 `Get-Process setup` 是否退出为准。修复后 `Windows Kits\10\Lib\10.0.26100.0\um\x64\kernel32.lib` 回来，`cargo check` 通过。
- **Windows 沙箱里的 exe 可带低完整性标签，离开沙箱双击也不一定恢复**（2026-11-24）：滚轮候选从 Explorer 启动后，进程令牌仍为 `S-1-16-4096`。用该进程的令牌只请求目录的 `FILE_ADD_FILE`，用户目录、桌面、文档和默认会话目录均返回 Windows 错误 5，低完整性的工作区可写。排查导出弹窗和历史不落盘，先核对实际运行的 exe、令牌完整性和目标目录；版本号相同不代表是同一构建。给用户验收时，从发布 ZIP 用资源管理器重新解压到普通用户目录，不复用沙箱解压的 exe，也不改目录权限。
- **磁盘余量不足时截图构建可能报 `LNK1318: LIMIT (12)`**：本轮临时 PDB 已 503 MiB，删除后普通链接仍失败；
  曾只为截图 exe 加 `cargo rustc … -- -C link-arg=/DEBUG:NONE` 临时通过。清理旧临时 target 释放约 16 GiB 后，
  标准 `cargo build -p claudesktop --features headless-screenshot` 重新通过；最终验收用标准构建，不能用早期绕过结果代替。
- **LLVM 输出失败也要查磁盘空间**（2026-11-16）：内核对照构建报 `IO failure on output stream: no space on device`，D 盘只剩 44 KiB。
  本轮仅移除自己创建的失败源码副本 / LLVM 残片，以及逐 SHA256 核对过的重复构建 exe；有效普通候选仍在 `D:/t3/gateway-memory/receive-fixed/claudesktop.exe`。
  有效 ETW 原始记录改存 `.claudesktop/bench/gateway-memory/latency-etw-02/system.etl.gz`，599785472 → 112014444 字节；完整解压流的字节数与 SHA256 校验通过后才删除未压缩副本。
  原文 SHA256 为 `9eed1844faf58f502714e80c63c2387c127785d5b726c799748fb0a5c71ccb81`，清理后可用 662061056 字节；重做跟踪分析先解压，不能把压缩文件直接交给 TraceEvent。
- **Git Bash 调 `vcvars64.bat` 的路径会被多层转义吃掉**（2026-11-16）：直接拼 `cmd /c` 的引号或反斜杠，
  曾分别变成带字面引号的命令和没有分隔符的路径；Node `spawn` + `/s /c` 也可能报「文件名、目录名或卷标语法不正确」。
  已验证的稳法是把临时 `.cmd` 放在项目内的无空格相对路径，在脚本里 `call` vcvars 后运行 cargo，再从 Git Bash 执行
  `cmd.exe //d //c ".claudesktop\bench\_check-desktop.cmd"`（`//` 防止 MSYS 把开关当路径）；目标目录仍用 `D:/t3`。
- **调试环境变量是 `CLAUDESKTOP_*`，不是 `CLAUDEDESKTOP_*`**：多拼一个 `DE` 会让截图、样本、启动计时等钩子全部无效，表现为窗口照常启动但没有截图或日志。先核对 `main.rs` 实际读取的名字，不要据此推断启动卡死。
- **markdown crate 在 vendor 里打过补丁（2026-11-14）**：GFM 自动链接只把 ASCII 空白当 URL 终点，
  全角标点（`，。（）`）被连字吞进 URL（`http://a.b/，好的` 整句成链接；`**http://a.b/**（注）` 星号露出）。
  改动在 `vendor/markdown/src/construct/gfm_autolink_literal.rs`（标 `claudesktop patch`，详见
  `vendor/README.md`），回归测试 `chat.rs` 的 `autolink_ends_at_fullwidth_punctuation`。
  坑：路径里的 ASCII 标点（`/` `?` `=` `&`）必须继续吞、只有非 ASCII 标点才收尾——第一版补丁按
  「是标点就收」写，URL 的路径被砍掉，被这个测试当场抓回来。
- 本机内存小，本地 `cargo test` / `cargo check` 会因页面文件太小（os error 1455）失败：改动直接提交，验证交给 CI。
- **v0.15.0 分发验收**：本地普通 release 已通过 604 项 workspace 测试、fmt 和 Clippy；打包核对 exe 版本 / x64、ZIP 内三份文件及两份 SHA256。
  隐藏运行实际分发 exe，走真实窗口消息新建 / 切换 / 发送，4 个会话、7 次主会话回环请求已验证历史无损恢复、运行中回合不休眠、流取消、正常退出清快照和重启续接。
  GitHub 构建的下载包也通过同一组快查，exe 与 ZIP 内的 exe 一致，版本 / README / LICENSE / 两份 SHA256 全部核对；`dist/` 已同步为下载产物。
  快查仅把 idle 阈值缩到 2 秒，仍走生产的 30 秒回收定时器。下载包不设休眠开关和 idle 阈值的完整验收也已通过：601.028 秒后自动休眠，随后恢复 / 运行中保护 / 停止 / 正常退出清快照 / 重启续接全部成功。
  下载包 SHA256 为 `8be90fc5cf8c7a15e38082a30444a34f1f45d5a77ef7baedf984475b6a99e948`；空数据首帧 485.62ms，4 会话重启首帧 303.73ms（本机快查，非冷缓存 A/B）。
  原始记录在 `.claudesktop/bench/release-local-07/`、`release-github-fast-02/` 和 `release-github-default-01/`。
- **v0.16.0 发布候选**：`48026aa` 保留设置未保存草稿，`13ab5d1` 统一「自己说」提示；真实无头检查覆盖
  修改 → 关闭 → 重进保留 → 手动重开丢弃，截图 `.claudesktop/bench/settings-drafts-03.png`。注意重进走
  `open_settings`，不是同分类的 `select_settings_tab`。
- **工具图片资源表测试要共隔离锁**：`assets::tests::isolated` 会清空进程级资源表，并行显示快照测试若不持同一把锁，
  会偶发 `工具图片资源读取失败`。`7b4c7ee` 已统一隔离，随后 workspace 623 项测试、fmt、Clippy、普通构建和
  `release-clean-fast-01` 全部通过。干净 worktree 内 `target/` 仍会踩已知 `ring/cc-rs` 问题，按旧约定用 `D:/t3`；
  D 盘曾只剩 9 MiB，只清理了 24 GiB 可重建 debug 产物，未动原始验收数据。
- **v0.16.0 包**：`384a887` 更新 workspace 与 lock 版本。包内 exe / README / LICENSE 三文件正确，构建 exe 与包内 exe
  SHA256 同为 `1f3925b142f8d9e3299f7e1fa699c629988573ec1e30293fb6c272f0393bc6a7`，ZIP 为
  `9ec9f9dc90a28b3a8dc0b16e6ef13c06eff3139f5da142d65c45b1ec28edbb03`。包内快查 `v016-package-fast-01`
  通过（4 会话、14 次请求、双层休眠、停止、清理与重启续接）；`v016-package-default-01` 也通过：
  604.174 秒自动双层休眠，随后恢复 / 运行中保护 / 停止 / 正常退出清快照 / 重启续接全部成功。
- **v0.16.0 GitHub 构建**：master CI `43759000764` 通过（含 `-D warnings` Clippy 和 workspace 测试），tag 构建
  `43759466703` 通过，Release 保持草稿。下载 exe 的 SHA256 为
  `59ba57e093ad60a1dcb2fd9e39fae3a030ea465bb7e60536190b0efdc130cee9`，ZIP 为
  `affa841d6db4dec627211c3aec1522fd01d3f2467be22d45c7a0c0b4a7877295`；与本地构建不同是工具链差异，
  但 ZIP 内外和 `SHA256SUMS.txt` 一致。`v016-github-fast-01` 通过（4 会话、14 次请求、双层休眠、
  停止、清理与重启续接）；`v016-github-default-01` 也通过：603.027 秒自动双层休眠，
  随后恢复 / 运行中保护 / 停止 / 正常退出清快照 / 重启续接全部成功，`dist/` 已同步为下载产物。

- **隐藏窗口验收退出只关主窗口**（2026-11-16）：同 PID 还拥有平台内部消息窗口，给枚举出的所有窗口都发 `WM_CLOSE`
  会造成退出超时；按 PID + 标题 `claudesktop` 筛选主窗口后通过。复用 `.claudesktop/bench/release-window-driver.ps1` 的筛法，别批量关同进程的 HWND。
- **正式包也要用同一份 exe 验收**：`CLAUDESKTOP_HEADLESS=1` 隐藏窗口、每 250ms 离屏绘制；不用截图 feature 或文件控制入口。
  本机把 GUI 放到 `CreateDesktop` 新建桌面时，进程在 `main` 前以 `0xc0000142` 退出，不能拿它判断业务启动失败；
  验收走隐藏窗口、隔离数据目录和回环模拟端点，按自己创建的 PID 发送窗口消息，不切前台、不移动用户鼠标。
  窗口消息验收不要模拟 `WM_ACTIVATE`（实测触发了前台保护）；先向隐藏输入区派发鼠标按下 / 抬起以取得界面焦点，快捷键要补齐抬键。
  每步检查窗口仍隐藏且未到前台；`WM_GPUI_KEYDOWN` 的返回值不能代表业务成功（回车返回 1 仍会正常发送），应核对实际日志 / 请求和会话归属。
  退出后立刻重命名 stderr 日志可能遇到 Windows `EBUSY`；验收中每次启动直接写独立日志，避免用日志归档动作阻断重启续接检查。
- **隐藏窗口的 Ctrl 快捷键不能靠 `PostMessage(WM_KEYDOWN)` 伪造**（2026-11-15 正式包验收）：目标线程
  `current_modifiers` 读 `GetKeyState`，只投修饰键消息不会置位，Ctrl+N / Ctrl+Tab 返回 1 且未执行。
  不要为验收用全局 `keybd_event` 干扰用户；直接向隐藏窗口发侧栏新建 / 会话行的鼠标消息。
  重启后应用落在空白新会话，续接验收要先点最新历史行，否则只是另建了一个会话。
- **含中文的 `.ps1` 必须带 UTF-8 BOM**（2026-11-14 实锤）：PowerShell 5.1 对无 BOM 文件按
  ANSI（GBK）解码，中文注释 / 字符串里的 UTF-8 字节会跟后面的 ASCII 配对吞字符，
  报一堆莫须有的语法错（「,」后面缺少表达式、字符串缺少终止符）。Edit/Write 工具重写整个
  脚本会丢 BOM——改完用 `printf '\xef\xbb\xbf'` 补回头三个字节，再用
  `[System.Management.Automation.Language.Parser]::ParseFile` 验一遍。
- **`target/` 旧产物会坏**（2026-11-12 实锤）：`target/debug/build/ring-*/` 的 build script
  残留一旦被杀毒 / 360 动过，再编就在 `cc-rs` 报 `exit code: 2`（cl.exe 的 `D8050: 未能将命令行
  放入调试记录中`，真实原因被 cc 吞了）。同样的源码换个 `CARGO_TARGET_DIR` 就能编。
  **修法：`CARGO_TARGET_DIR=D:/t3 cargo build`（干净目录，一次性编完约 2 分钟）**，
  项目内新建 `target/compact-verify` 仍会在同一步失败（2026-11-13），仅换子目录不够。
  或删 `target/debug/build/ring-*` 后换新目录。别再顺着 cc-rs / vcvars / CFLAGS 排查——
  那条路本机已全部证伪（INCLUDE/LIB/并发内存都不是原因）。
- **拆模块时的三个必踩点**（2026-11-25，session / settings_view / builtin tools 大拆分）：
  ① 父模块 `pub(crate) use child::*;` 再接子模块 `use super::*;` 是循环 glob，rustc 不保证链式可见——
  兄弟模块之间直接写 `use super::files::*;`，被共享的项改 `pub(super)`；凡是原来 `pub(super)`
  的方法搬进子模块后含义会缩成「仅 settings_view 内可见」，外层 `app::keys` 调用的要升成 `pub(crate)`。
  ② 用「方法名 + 大括号计数」切函数会提前收尾（字符串 / 注释里有 `}`），按顶层 items 或方法边界切；
  ③ 切分点落在 `///` 注释和下一项之间会留下「empty line after doc comment」的孤儿注释，clippy 必报——
  文档注释要跟着函数一起搬。新增自由函数若在子模块里被 `impl` 块后插入，记得先闭合 `impl`。
- **`core.autocrlf=true` + `.gitattributes eol=lf` 下批量改 EOL 后 `git status` 会残留假脏**（2026-11-25）：
  工作区的 CRLF 一次性转成 LF 后，200 多个文件显示 ` M`，但 `git diff` / `git diff-files` 的 blob 哈希完全一致，
  `git update-index --refresh` 也清不掉（只对确认无改动的单个文件 `git checkout -- <path>` 才恢复）。
  **判断改动一律以 `git diff` 为准，不要被 status 的 M 数吓到**；正常编辑源码不需要手动转 EOL，
  git 提交时会按 `eol=lf` 归一，脚本批量改写时才要自己写 LF（见上一条 —— 全程用 Edit/Write）。

## UI / gpui

- **`Reveal` 必须一直挂着，自动收起不能播弹簧**（2026-11-24）：只在 `reveal < 1.0` 时包 `Reveal`，展开期间它的高度从没量过（`height == None`），一收起第一帧按 0 排，整页塌一下再弹回。列表里 `layout.member` 的行一律挂 `Reveal::new(..)`，进度 1.0 也挂。反过来，自动收起（新行涌入、正文到达、`apply_event`）得走 `reveal_progress_when(.., animate=false)`（内部 `Spring::with_travel(false)`，直接落位、不请求帧），否则每出一行列表高度都一涨一缩。只有用户点过的组（`Session::animated_group`）才播。
- **折叠态工具的 `ToolCallProgress` 会每 500ms 重排整页**（2026-11-24）：`run_shell` 跑着时每 0.5 秒报一次最新几行，原来每次进 `apply_event` 都改 `ToolCard.output` 并 `cx.notify()`——那条工具收在折叠组里看不见，却让虚拟列表重测行高、内容区整页重排，还把滚动的转圈画了一帧就重置。现在两条一起：`ChatApp` 收到进度先按 `is_tool_output_visible` 判断这条展开着没有，不可见就不通知；`ChatItem::fingerprint` 不再把 `output.len()` 编进去（改在 `Session::row_key` 里只对**展开中**的工具行算长度），折叠行的指纹不随输出变。
- **内层滚动区不能在 capture 阶段「放行」，要自己滚掉**（2026-11-24）：展开的工具输出靠 `overflow_y_scroll` 自己滚，`div` 的内置处理**不调用** `stop_propagation`，同一条 `ScrollWheelEvent` 会继续走 bubble 到外层对话 `List`，两边各滚一次（双滚动、整页跟着硬跳）。正确做法是在 capture 阶段按指针命中的句柄直接 `set_offset`，并**照常** `cx.stop_propagation()`；登记命中区时要和 `window.content_mask().bounds` 求交，卡片被视口裁掉的那半不能再命中。
- **60 FPS 的循环动画要锚在「上一帧真正画出的时刻」**（2026-11-24）：：`LoopOverlay` 从 30 帧提到 60 帧（`LOOP_INTERVAL = 16ms`）后，相位若按「计划时刻」推，计时器晚到会累积漂移、转圈一顿一顿。现在 `paint_loop_spots` 每次画完记 `LAST_LOOP_PAINT`，`wait_for_tick` 取它和本次唤醒时刻的较大者当锚。圆弧扫过固定 `SPIN_SWEEP`（0.30），不做脉冲——相位和形状都只跟时钟走。

- **12px 的小图形直接 `paint_quad` 比 `PathBuilder` 干净**（2026-11-26）：转圈换成弹性双块后就画两支圆角方块——
  `fill(bounds, rgba)` 得到 `PaintQuad`，改 `corner_radii = Corners::all(px)` 再 `window.paint_quad(quad)`，
  不建路径也没有描边圆头的补点。两个坑：`Pixels` 同时有派生的 `Div<f32, Output = Pixels>`（`gauge.rs` 的
  `stroke / 2.` 靠它）和手写的 `Div<Pixels, Output = f32>`，除数是**整数字面量**时两条都不匹配（`d / 2` 报错，
  写 `d / 2.` 或 `d * 0.5`）；圆角按 `w.min(h)` 算，伸长的条才不会被夹成胶囊。

- **对话滚动的卡感要分别量位移和绘制耗时**（2026-11-24）：当前 60px 一格先走 30px，按 16ms 帧间隔计算，显示步长约为 38、6、4、3px。隐藏窗口用 100 轮短对话实测，应用内 300 帧整窗重绘的 p95 为 2.99–4.41ms（三次）。在末条回复加一张 256 行四列表后，p95 为 26.44–27.16ms，两次都超过 60Hz 的 16.7ms 预算；直接滚动也有 23.59ms。同样的表格数据拆成同一条回复里的 8 块仍有 25.91ms，分到 8 条回复后降为 8.09–8.29ms。外层按消息虚拟化，可见消息里的表格由 `gpui-base::text` 整表构造。查普通文本的顿挫先看单格位移分布；查复杂回复再看一帧构造的块和表格行数。复现脚本、截图与逐帧数据在 `.claudesktop/bench/wheel-stutter-20261124/`。
- **对话滚轮的绝对像素目标会在补量行高后失效**（2026-11-24）：`wheel_by` 保存滚动条的绝对目标，`step_wheel` 用下一帧的滚动条读数计算剩余距离。GPUI 给未测行记 0 高，补量视口上方的行会抬高该读数，顶端仍可停在同一条消息。每帧限速只能压低单步位移。从底部向上连滚四格，100 轮历史的隐藏窗口中直接滚动走 240 个逻辑像素，首次平滑走约 525，预先滚过再回底仅走 150。后者还暴露了连滚时即时位移消耗上一格余量的问题。修复时应按输入累计带方向的剩余位移，换向清余量，并对照同一消息的实际屏幕位置。复现脚本和离屏帧位于 `.claudesktop/bench/wheel-root-20261124/`。
  已按此修复（2026-11-24，三轮）：对话滚轮状态换成 `session::WheelInput`——每次离散输入累加 `84px` 相对剩余位移（不再记滚动条绝对目标），每帧按 `τ=68ms` 指数阻尼缓出（`1 - exp(-dt/0.068)`，剩不到 `0.5px` 时当帧带完不留零头）。同向累加、反向清空，单帧最多 48px，到边界把朝外的余量作废。`step_wheel` 排在 `sync_list` 之后，定位 / 恢复先完成再推进当帧滚轮。
  四个坑：①**不能把未走完的亚像素增量清零**。每帧剩余不到 `0.5px` 时必须顺手带完而不是丢弃，否则高刷或小步长会连续吃掉余量；现在仅靠 `(remaining - raw).abs() < 0.5` 时一次收完保证位移严格守恒。②`ListState` 正跟随末尾时逻辑位置记在**末尾之后**（读数比可滚范围多一个视口，只在 `set_follow_mode(Tail)` / `scroll_to_end()` 之后到下次 `prepaint` 之前），先 `scroll_by(-viewport)` 会先拿这段溢出抵掉，表现为一开始滚要空滑几帧——往上先归位到 `max_scroll` 再用读数算；往下跟随时直接判到底。③`scroll_by` 只在内部夹顶部，底部靠布局夹。单步一律裁到**已量出的底部 `max`**（未测行记零高，但列表留了 600px 尾部 overdraw，正常 48px 和触控板输入都碰不到），这样触控板甩滑也不会一步跨过整片零高的未测行瞬移到底；「到没到底」要按**滚动之后**的读数问 `is_scrolled_to_end()`。④`layout_items` 里「离底不到 1px 就重新跟随」会把微秒级紧邻重绘或极小步长下的上滚拽回底部，所以还在往上走时 `step_wheel` 每帧再 `pause_following_tail()` 一次。
  另外：两帧间隔超过 500ms（去了设置页、最小化）整串作废，`push` 也按同样的间隔判断，回来不补几秒前的滚轮、也不会把刚推入的那一格一起清掉；进设置 / 子任务 / 审查页与 `sync_list` 的重置也清掉余量。`WHEEL_HOLD`（400ms 一步都走不出去就丢）只在 `applied == 0` 时计时，`push` 会重置它。
- **滚轮手感第二版：换 $C^1$ 三次贝塞尔 + VSync 节拍 + 物理像素对齐**（2026-11-24）：指数阻尼在真机上仍能看出两个毛病——首帧按墙钟一步冲 18px、尾部靠亚像素爬 130ms（1px 行距微颤），连滚时每格一次 4 倍锯齿。现在 `WheelInput` 沿一段三次贝塞尔 `(0,0)-(x1,y1)-(0.55,1)-(1,1)` 推进，段长 = 欠量、段时长 180ms 起按欠量收到 135ms；单格 60Hz 走 `6, 9, 11, 12, 12, 10, 9, 7, 5, 2, 1`（11 帧、总计严格 84）。四个新坑：
  ①**帧间隔要按刷新周期量化**（`window.refresh_interval(cx)`，查不到用 `DEFAULT_FRAME_DT`）。直接用墙钟差值会把 `WM_PAINT` 的消息队列抖动算成速度变化；现在首帧固定按一个刷新周期起步（到达时刻不再影响首步），后续帧把间隔舍入到 1..=4 个刷新周期，同一周期内的杂重绘（< min(0.25T, 2ms)）直接跳过、不动 `last`。
  ②**`applied == 0` 时必须重锚曲线**：计划了却走不动（等未测行 / 撞边界）不能推进 `seg_elapsed`，否则解堵那一帧 `u >= 1` 会把剩余距离一次性吐出来。但高刷尾段 `advance` 自己返回 0（不到半个物理像素）时 `step_wheel` 不会调 `consume`，那是正常记账——`consume` 里靠 `pending` 有没有来区分这两种 0。
  ③**量化 + 误差扩散**：每帧步长取 `round(want * scale) / scale`，小数余量沿当前方向累加到下一帧，总位移严格守恒；高刷尾段攒不够一个物理像素就返回 0 等下一帧。配合 `vendor/gpui-pre` 里 `prepaint_items` 把绘制起点也钉到物理像素网格，整屏文字在所有滚动帧里相位恒定。
  ④**连滚接速度而不是接距离**：同向追加输入时把当前 `vel` 归一化成新段的起手斜率（`x1 = min(0.85 / slope, 0.42)`，保证严格单调无过冲），所以连滚没有「一格格一冲」的节奏；被限速 / 部分截断时按实际速度重锚剩余距离，平滑脱离 48px 上限。
- **距底 80px 内松手不再瞬移吸底**（2026-11-24）：旧 `sync_follow` 在「距底 ≤ 80px + 正在往下滚 + 尾部已实测」时直接 `set_follow_mode(Tail)`，用户滚到距底 40–60px 一松手视口就跳到底。现在 `ScrollFollow` 多两个字段：`armed`（已进吸附带待命）+ `armed_seq`（进入待命那一刻的 `content_seq`）。停在 1..=80px 之间只置 `armed`、视口原地不动；真来了新内容（`content_seq > armed_seq`）才 `sync_follow` 返回 `true`，由 `content.rs` 起一个 `motion::JumpAnim` 平滑滑回底部（减少动态效果时直接 `follow_tail()`）。`sync_list` 同步把 `armed` 也算进「都看到了」，所以待命期间不出未读红点。用户自己往上翻 / 拖滚动条 / 重定位 / `follow_tail` 都会清掉 `armed`。判定本身抽成纯函数 `follow_step`，单测直接覆盖六种几何组合。

- **正文滚动快照要跟着帧状态接力**（2026-11-24）：`cached_scroll` 从未被外层视口裁切的正文快照按物理像素平移，
  每帧再应用当前视口。图元和 hitbox 的基准坐标不累加。普通 `ContentView::cached` 下的临时 `refreshing` 与
  `Window::refresh()` 的整窗刷新分开处理，否则每个滚动帧都会错过正文缓存。滚停按真实位置 prepaint，鼠标按下或横滚前
  同步重建输入回调捕获的绝对坐标。后台 Markdown 解析由 `TextViewState` 观察器使缓存失效。会话休眠、回收正文或重建行划分时清理正文实体缓存。
- **第三版滚轮**（2026-11-24）：默认 3 行走 96px，Impulse 单格 210ms，积到 480px 时逐步增加到 320ms。
  静止控制点为 `(0.22, 0.24)` 和 `(0.25, 1)`，连续输入继承起手速度。Windows 绘制锁内在绘制前校验失效区，给绘制期间新到的 VSync 保留失效标记。
- **当前滚轮曲线与落定帧**（2026-11-24）：Chromium 在 2024 年移除了 Impulse，156 的鼠标滚轮参考曲线为
  `EASE_IN_OUT + INVERSE_DELTA`。默认一格 96px、200ms，欠量从 120px 增至 480px 时默认段长线性缩至 100ms。
  旧控制点横坐标 `x1 > x2` 会在中慢速连滚时放大速度起伏。固定横坐标 0.42/0.58，继承速度过大时缩短段长以压住 `y1`，
  单周期限速 6000px/s、单帧至多 48px。[Chromium 移除 Impulse 的提交](https://chromium.googlesource.com/chromium/src/+/890cc838a079823dac40fd37b9e4edc1c73cc9f4)
  - 最后一步会清空平滑状态，但这一帧依然移动过。回底和滚轮推进报告当帧是否移动，`transcript_scrolling` 继续复用快照。
    指针点击、释放或纯横滚前同步重建布局。悬停落点继续记录，停稳 120ms 后再淡入，触控板和拖动滚动条也进同一段静默期。
  - `WM_PAINT` 投递差会在 180Hz 下把单周期误算成两个。每窗独立保存最后一个 `DwmFlush` 时间戳，仅在绘制回调内读取。
    隐藏窗口、计时器回退及其他平台用墙钟，量化的二帧阈值为 1.75 个周期。GPUI profiler 仍单独保留首个信号。
  - 180Hz / 150% DPI 的步进模拟中，单格用 35 帧，峰值后无零位移帧。120ms / 160ms 连滚的稳态步长峰谷比为
    1.60× / 2.33×。10 格每 35ms 输入一次，最后一格后 157ms 停稳。
  - `paced_180.py` 本轮 A/B 的 `rest` 最大耗时（short / many / code）为 6.58 / 6.38 / 4.26 → 4.15 / 6.48 / 2.60ms。
    原脚本在停输入后等待 800ms 并截图才采 `rest`，不能定位最后一个移动帧。在 `up` 与截图间补 60 帧无输入采样后，
    该段 p95 为 6.01 / 6.34 / 3.92 → 2.68 / 4.08 / 3.51ms。`rest` 峰值仍未稳定在约 2ms。
    原始数据在 `.claudesktop/bench/wheel-stutter-20261124/vsync-hover-*/frames.json`。180Hz 前台呈现和实际点选仍需实机核对。
  - 普通及无头 release 构建通过，桌面 253 项测试通过。Windows VSync 三项测试用 `claudesktop/headless-screenshot` 提供
    GPUI 的 test-support。GPUI 隔离套件的 profiler 和缓存测试通过，弹簧重定向用例在本次构建和改动前二进制中同样失败。
- **滚动压测要走正常缓存路径**：旧 `draw_bench.py` 强制 `Window::refresh()` 并清理继承的 `CLAUDESKTOP_*` 环境变量。
  通过进程启动选项传 `CLAUDESKTOP_STRESS_RETAINED=1` 才能测平移缓存。`draw_retained_ab.py` 都用这一路径跑 Step 2 和候选，
  `paced_180.py` 用无头窗口的 180Hz 消息节拍辅助测交互。手动离屏 draw 数字不包含真实显示器的 Present/VSync 交付。
  vendored GPUI 的原始测试需要未随 crate 带来的上游夹具。滚动测试在同源码隔离副本里取出那几组不相干的测试并关闭离线不可解析的
  可选上游基准依赖，见 `.claudesktop/bench/wheel-stutter-20261124/translate-gpui-tests.log`。

- **行级快照与第三版滚轮的实测**：144 DPI、1920×1230 隐藏窗口，release 每场景三对，每轮 300 帧。
  下列 p50 / p95 是三轮各自百分位的中位数，单位 ms。Step 2 → 最终版，代码 1.41 / 3.09 → 0.73 / 0.97，
  多消息表格 2.40 / 4.31 → 1.25 / 1.77，离底表格 2.44 / 4.64 → 1.24 / 1.82，短消息 1.68 / 2.42 → 1.67 / 2.47，
  大量历史 1.72 / 2.55 → 1.52 / 2.15。短消息和大量历史的 p50 仍超过 1.20ms。
  180Hz 节拍下，代码/表格消息处理 p95 最高 3.08 / 3.75ms，最大 9.87 / 14.27ms。路径见
  `.claudesktop/bench/wheel-stutter-20261124/translate-final-ab.json` 和 `translate-final-paced180-*/frames.json`。
  桌面 249 项和 GPUI 隔离滚动测试 21 项通过。完整工作区套件受回环 socket 错误 10013 阻挡，
  单独执行 core 已编译的测试另有 5 个插件 hook / 本地仓库用例失败，移到无 `#` 的可写临时目录重试结果相同。
  当前 gpui-base 代码块默认折行，普通长代码没有水平滚动条，水平裁切使用 Scene 测试验证。
  180Hz 前台呈现、与 Kimi Code 并排体感、正文选择/链接和实际切 DPI 仍待实机核对。

- **顶行切换和触控板斜向输入**（2026-11-24）：列表保留连续的逻辑 Y 坐标，逐项把绘制坐标对齐物理像素，
  自动滚动用绘制坐标反推偏移。半像素统一往正方向取整，跨零点时也能按整数物理像素平移。
  触控板输入只在有横移且纵移为零时同步重建布局，因为对话区会截住含纵移的滚轮事件。
  新测试在旧版复现 150% DPI 半像素位置偏差和斜向输入强制重画。日志在
  `.claudesktop/bench/wheel-stutter-20261124/phase-fix-regressions-before.log`。
  修复后 GPUI 滚动 22 项和桌面端 249 项通过。同机 144 DPI、每场景三对各 300 帧，三轮中位 p50/p95（ms）
  短消息 1.54/2.04 → 1.46/1.91，大量历史 1.38/1.80 → 1.36/1.87。两组 p50 仍超过 1.20ms。
  GPUI 的 Taffy 测量边界本身也有物理像素取整，半像素跳位的复现不能推断整屏正文都在重排。
  原始数据见 `phase-fix-ab.json`。同目录 `phase-fix-paced180-*/frames.json` 记录 180Hz 节拍离屏测试。
  复制 vendor 代码到测试副本后要更新目标文件的修改时间。保留较旧时间戳会让 Cargo 判定 Fresh，误跑上一轮的测试可执行文件。

- **隐藏窗口的鼠标消息坐标要用物理像素**（2026-11-16）：PowerShell 默认 DPI 不感知时 `GetClientRect` 可能报 1280×820，
  而同帧离屏截图是 1920×1230，按这个假比例换算会点空。窗口驱动先 `SetProcessDPIAware()` 再取客户区 / DPI，
  截图坐标直接作为 `WM_LBUTTONDOWN` / `WM_LBUTTONUP` 的客户区坐标；验收脚本还要持续更新控制命令，90 秒不更新会触发看门狗。
- **右侧面板首次打开不要先选文件再初始化会话**（2026-11-16）：`RightPanelState::sync_session` 首次绑定会清空文件树和预览；
  截图 / 调试入口先调用 `sync_session`，再 `open_file` / `open_right_diff`，否则画面只剩未选择状态。
  正常用户点击发生在绑定之后，不受影响。
- **右侧文件树基线**：`.claudesktop/bench/file-tree-baseline/baseline-01/aggregate.json` 记录 100 文件、单层 1 万文件和 10×1000 文件各 3 次隐藏 release 运行。视口约 27 行；后两种每次滚动分别构造 10000 / 10010 行，滚动 p95 三轮中位 412.90 / 292.05ms，单层根目录读盘加排序约 8ms。先测虚拟列表，再量缓存回收收益。
  单层 / 分层的进程私有字节峰值三轮中位为 495.94 / 486.13 MiB，关闭 3 秒后为 403.71 / 393.97 MiB；仍持有的目录数据估算只有 2.98 / 2.78 MiB。峰值含整进程，不能按差值给具体分配器归因。
  筛选探针用 `InputState::replace_all` 走 `InputEvent::Change`，`set_value` 不发事件。原 60 秒看门狗截断第三轮分层组；保留失败目录，只补未完成组，改为 150 秒，样本数和测量逻辑相同。临时探针补丁已撤下，普通 exe 已恢复并核对哈希。
- **文件树虚拟化验收**：`.claudesktop/bench/file-tree-baseline/paired-01/comparison.json` 的 9 对全部达标。单层 / 全展开分层每滚动帧 10000 / 10010 → 29 行，三轮滚动 p95 中位 455.87 / 431.21 → 1.97 / 2.15ms，桌面进程私有内存峰值 495.20 / 485.51 → 159.11 / 159.24 MiB。索引容量 384 KiB。百文件同版本单对 p95 差 2.53ms，未通过预设 2ms 校准；保留 `aa-small-01`，正式对照三对小目录的实测均无倒退，精确小差值受本机抖动影响。
  普通 release 的隐藏窗口经鼠标和字符消息操作，UI Automation 验证输入框状态。`window-capture-02` 离屏图补验目录名、滚动、预览与切换会话后的文件归属；UI Automation 不返回这些普通文件行的文本。格式、Clippy、660 项工作区测试与普通 release 构建通过。
- **源码预览基线**：`.claudesktop/bench/source-preview-baseline/baseline-01/aggregate.json` 有 5 组各 3 轮、每轮 60 次滚动。256 KiB / 8192 短行的读取、打开和滚动 p95 三轮中位分别为 0.44 / 606.15 / 485.45ms；每帧构造全部 8192 行。私有字节峰值较打开前多 271.68 MiB，关闭 3 秒后仍多 242.67 MiB。这是进程级测量，预览保留的文本只有 256 KiB。
  320 个长行在 345 / 453 / 779 逻辑像素宽下，每行实排 22 / 17 / 9 排，优先试按实际高度虚拟化。普通版隐藏窗口通过 13 个交互步骤；`smoke-capture-01` 离屏图覆盖换行、回到首行、截断、截断处 UTF-8、CRLF 空行、二进制和文件消失。
  独立计时区间转成浮点毫秒后相加有舍入误差；允许 1µs 容差，首次并排组原始样本保留，补跑后完成三轮。临时探针补丁已撤下并核对普通 exe 哈希。
- **预览变高列表的末尾要按实际行高排**：`List` 重排会丢掉旧高度；估算高度让未测行仍参与滚动，只有视口附近实测。
  滚动跨过估算的底部时，改走 `scroll_to_end` 从末项反向排，才能一次到截断提示。26 万空行的坐标计算从视口顶部累加，避免大坐标相减令末项位置断言失真。
- **源码预览虚拟化验收**：`.claudesktop/bench/source-preview-baseline/ab-01/comparison.json`，5 组各 3 对、1800 次滚动、24 项检查通过。
  8192 行每滚动帧构造 8192 → 37 行，首帧 610.32 → 7.98ms，滚动 p95 433.49 → 2.12ms，进程私有内存峰值增量 271.48 → 6.26 MiB。
  `verify-02` 验证第 161 行在拖宽、全宽和字号变化时保留 41.36% 行内锚点。262144 空行与单行 256 KiB 均可到末尾；空行极端样本单次峰值增量 52.65 MiB。
  `smoke-virtual-normal-02` 普通版 13 步交互通过，`smoke-virtual-capture-02` 核对截断、回首行、CRLF 和错误提示。fmt、Clippy、661 项 workspace 测试及普通 release 构建通过。
- **右侧改动面板基线**：`.claudesktop/bench/diff-panel-baseline/baseline-01/aggregate.json`，5 组各 3 轮，720 次确认位移的滚动。千文件窄面板首帧 138.17ms、滚动 p95 114.76ms、每帧构造千行，进程私有内存峰值增量 68.82 MiB。滚动汇总中位 59.04ms；点选后又为查预览重算一遍。单文件 400 行短 / 长 Diff 的滚动 p95 为 34.71 / 35.86ms，点选首帧 43.72 / 156.64ms。
  `window-capture-02` 复现同一工具改两文件时，点清单第二行无响应，点对话改动卡第二行则显示第一份 Diff；全宽会同时高亮两行。`window-large-03` 的同一滚轮操作在窄面板移动到第 17 号文件，在并排和全宽时首行仍是 0 号，两个布局的滚动时延待功能修复后补测。
  GPUI 会在分发抬键前补画脏帧。测快捷键打开时先采按键到首帧，再发抬键；第一次快查把两次场景构建算成一帧的记录留在 `smoke-01`。探针补丁已撤下，普通 exe 哈希未变。
- **改动清单的布局与回退夹具**：并排和全宽的左列用纵向 flex 限住清单视口。`uniform_list` 的行要显式 `.w_full()`，否则点击和高亮只覆盖文字，增删数贴着文件名。
  走对话回退的历史夹具把元信息 `agent` 写成 `builtin:diff-fixture`。`builtin` 会按旧格式识别为 ACP，只允许还原文件。看回退日志的 `conversation` 字段可区分两种路径。
  GPUI 滚轮先累加偏移，布局时才夹到上下限。验证大步滚到表尾时要先绘制一帧，或用 `candidate-large-02` 的窗口消息和离屏图核对末行及其 Diff。
- **改动清单虚拟化验收**：`.claudesktop/bench/diff-panel-baseline/candidate-ab-02/comparison.json` 记录三轮交替对照、42 个独立进程和 2340 次有位移校验的滚轮事件。
  窄栏千文件每滚动帧 1000 → 27 行，首帧 135.55 → 9.78ms，滚动 p95 229.61 → 5.20ms，私有内存峰值增量 68.43 → 3.63 MiB。并排／全宽带预览时的最慢一轮滚动 p95 为 7.74／2.81ms。
  三种布局都能点开第 1000 个文件。`candidate-normal-02` 验证第二行右端可点，`candidate-verify-02/1-selection-candidate` 确认同工具多文件和回退，`candidate-history-02` 核对同路径两轮预览与切会话。fmt、Clippy、662 项测试及普通 release 构建通过。
  该批 400 行短／长 Diff 正文的滚动 p95 中位 64.82／65.50ms。清单已退出窄栏预览的渲染路径，当时正文仍构造整份 Diff。
  工作区根目录运行 `node .claudesktop/bench/diff-panel-baseline/compare-candidates.mjs <新目录> ab` 复跑，脚本核对两份冻结探针和该批候选源码哈希。
- **改动正文虚拟化**复用源码预览的变高列表。共享 Diff 行与卡片表头，正文行内样式直接设在行上，列表单独量行时不会继承卡片的字体样式。
  正文剩余高度由 flex 确定，表头固定，截断提示作为末行。关闭或切页释放已测行高，保留定位和滚动位置；实时替换同一份 Diff 时按修订号失效高度。
  `.claudesktop/bench/diff-body/ab-01/comparison.json` 冻结清单优化版为基线，三轮交替有 30 个独立进程、2520 次有位移校验的滚轮事件。用的是同一份历史夹具，1280×820 逻辑像素、字号 100%、动画关闭。
  400 短／长行的正文首帧三轮中位 41.52／135.62 → 9.26／9.11ms，滚动 p95 中位 55.93／59.73 → 8.26／7.75ms，每帧最多构造 400 → 34／5 行。候选三轮最慢的滚动 p95 分别为 11.40／7.85ms，长行首帧最慢 10.61ms。
  桌面进程私有内存峰值增量 19.38／24.22 → 5.25／4.03 MiB，关面板 3 秒后 14.55／19.43 → 2.37／1.37 MiB。千文件三布局继续每帧至多 27 文件行，最慢清单滚动 p95 7.01ms；窄栏该场景峰值增量中位 3.67 → 4.36 MiB。
  `verify-01` 验证第 161 行在拖宽、全宽与 125% 字号下保留约 20.16% 的行内位置；实时替换重测行高，回退清掉预览。`capture-01` 用窗口消息滚到三种布局的末行和截断提示，`normal-01` 用普通 release 核对打开、返回和切文件。fmt、Clippy、663 项测试及普通 release 构建通过，探针插桩已移除。
  工作区根目录运行 `node .claudesktop/bench/diff-body/run.mjs <新目录> probe-01 ab` 复跑。用 `build-probe.mjs <新构建名>` 另建探针，原基线与 `probe-01` 保持冻结。

- **请求原文弹窗基线（`6580825`）**：从侧栏「用量」滚到「请求明细」，展开一轮再点「原文」。
  `.claudesktop/bench/raw-request/baseline-01/summary.json` 有五组各三轮，15 个隐藏进程共 30 次打开、75 次绘制。1 / 12 / 32 MiB 合成请求配 128 KiB SSE，另测 12 MiB 配空响应和 2 MiB 截断响应。
  1 / 12 / 32 MiB 请求点击到首次内容绘制的中位分别为 0.99 / 10.37 / 26.85 秒，打开峰值私有内存比打开前多 40.28 / 412.91 / 1069.90 MiB。关 3 秒后的增量为 3.36 / 7.26 / 14.09 MiB。
  32 MiB 样本的后台解压和排版中位合计 102.49ms，请求编辑器初始化 120.04ms，从原文视图 `render` 到内容绘制 26.53 秒。首次切到 2 MiB 截断响应的帧工作为 264.46ms，切回已显示的「请求」约 8ms。
  `capture-01` 有空与截断响应的离屏图，`normal-04` 的普通 release 验过打开 / 切页 / 关闭 / 重开。普通 exe 的 UI Automation 要在打开用量页之前激活；探针 stderr 用共享读写句柄读，避免等待已经写出的标记。截图构建走控制入口的 `quit`，直接发 `WM_CLOSE` 会碰到绘制定时器的 `window not found`。
  `wrap-ablation-01` 用同一个诊断 exe 交替三轮，只切换请求页软换行。12 MiB 样本首帧中位数 9551.56 → 205.67ms，首次帧工作 9344.88 → 47.51ms，私有内存峰值增量 413.67 → 93.21 MiB。gpui-base 首次获得换行宽度时会测量整个文档；关闭换行也会量最长行，后续须加入超长单行夹具。
  探针在忽略目录的独立源码副本构建。工作区根目录运行 `node .claudesktop/bench/raw-request/run.mjs <新目录> probe-01 baseline` 复跑；软换行对照用 `probe-wrap ablation 12m`。同目录 `verify.mjs` 核对原文 gzip、帧序列与汇总。
- **请求原文按页读取与视口排版验收**。`.claudesktop/bench/raw-request/ab-02/comparison.json` 有六组各三对，36 个隐藏进程和 180 次绘制校验。
  12 / 32 MiB 点击到首帧中位 8787.87 / 22693.31 → 244.74 / 244.16ms，首次绘制工作 8510.00 / 22411.90 → 26.45 / 35.11ms。两个请求样本的首次绘制各构造 7 个正文块，峰值私有内存增量 413.85 / 1070.81 → 62.82 / 158.20 MiB，关后 3 秒增量 7.23 / 14.14 → 3.33 / 3.76 MiB。
  2 MiB 截断响应首切绘制 217.18 → 16.60ms。16 KiB 小请求保留编辑器，首次绘制中位 96.71 → 75.12ms，点击到画面 331.60 → 359.57ms（含隐藏窗口等帧）。仅打开请求时未开始读响应，第二次切到已查看页复用正文。
  `edge-02` 搜索到 12 MiB Base64 单行末尾，切页恢复位置，复制数据和导出文件的 SHA256 均与全文一致。用诊断副本接管剪贴板和系统打开动作。`race-02` 验证加载时关闭并快速切页，`missing-01` / `empty-01` 核对缺失和空响应，普通版在 `final-normal-01` 的小请求与 `final-normal-02` 的 32 MiB 请求完成主窗口操作。
  保留 `ab-01` 的早期窗口尺寸失败。启动稳定后重新读客户区和 DPI，`--resume=ab-01` 复用完成的配对，补跑最后三组完整配对。隐藏窗口需要立即刷新关闭按钮的命中区时发 `WM_GPUI_FORCE_UPDATE_WINDOW`。空响应的禁用观感用离屏图核对，普通版 UI Automation 的 `IsEnabled` 断言在该路径不一致。
  fmt、all-targets release Clippy 和普通 release 构建通过。工作区测试首次在既有的资源缓存并行断言失败，完整重跑 667 通过 / 13 忽略。普通 exe 位于本节 `release-02/claudesktop.exe`，探针与源码哈希见 `candidate-01/build.json`。
  复跑 `node .claudesktop/bench/raw-request/run.mjs <新目录> candidate-01 ab tiny 1m 12m 32m 12m-empty 12m-cut`，再用同目录 `verify-candidate.mjs <新目录> candidate-01` 复核。
- **请求原文峰值分段（`75f9af1`）**：`raw-request/stages-02/stages.json` 记录 32 MiB 多行、Base64 单行各三次真实窗口打开。
  两组的解压缓冲容量都到 64 MiB，解压阶段峰值增量中位为 97.26 / 97.28 MiB，JSON 解析后为 101.54 / 97.28 MiB，排版后为 158.30 / 193.49 MiB；行索引容量均为 1.5 MiB。峰值来自 Windows `PeakPagefileUsage`。
  分段打点同时标记请求和返回的索引分配。128 KiB SSE 有 4163 行，也走大正文列表；`stages-01` 首次校验漏算它的两个标记，修正序列后重测。
- **请求原文复用解压缓冲**：同一解析器成功后清空解压缓冲，由 `to_writer_pretty` 写回，释放 `Value` 后收紧缓冲容量。无效 JSON 仍退回原始文本。
  `.claudesktop/bench/raw-request/peak-ab-01/peak-verification.json` 有五组各三对，30 个隐藏进程和 150 次绘制校验。12 MiB、32 MiB 多行、32 MiB 单行的打开峰值增量中位为 62.84 → 33.76、158.28 → 101.53、193.51 → 97.26 MiB，两种 32 MiB 样本每对均降。
  32 MiB 多行 / 单行后台加载中位为 75.13 → 65.82 / 67.90 → 60.24ms，首次绘制工作为 15.99 → 25.59 / 12.49 → 11.78ms。定时绘制的多行点击到首屏为 254.33 → 322.69ms，其中等待绘制的中位为 158.67 → 230.15ms。各阶段中位不能相加推导总中位。
  `peak-prompt-01/prompt-verification.json` 在后台加载完成后立即驱动隐藏窗口绘制，两种样本各三对，峰值依然逐对下降。点击到首屏中位为 104.91 → 107.11 / 99.47 → 103.29ms，最慢一对多行增加 19.97ms。以 `ab --against=candidate-01 --prompt-paint 32m 32m-line` 复跑。
  `peak-edge-01` 真实窗口查找到单行末尾（字节 33554463..33554487）并验证切页位置，复制和导出各 33554548 字节，SHA256 与完整排版一致。诊断版把剪贴板写到夹具目录，未启动外部编辑器。`peak-normal-01` 用普通 exe 验证小正文和两种 32 MiB 正文的窗口操作。
  fmt、全工作区 release Clippy（-D warnings）、普通 release 构建和 668 项测试通过（13 项忽略）。原始记录和普通 exe 在 `peak-release-01/`，SHA256 `9a78f54947bc603292270e2e3f8fb301451d6a515387056c2fa216183dcd9c84`。
  复跑 `node .claudesktop/bench/raw-request/run.mjs <新目录> peak-probe-01 ab --against=candidate-01 tiny 1m 12m 32m 32m-line`，再用同目录 `verify-peak.mjs <新目录> candidate-01 peak-probe-01` 复核。
- **大原文缓冲容量与解析峰值**：以 `peak-probe-01` 的缓冲复用版作基线。`capacity-baseline-stages-01` 的两种 32 MiB 输入仍把解压容量撑到 64 MiB。
  gzip 尾部大小只作有界预留提示，解压器继续检查内容和校验值。仅改预留的 `capacity-ab-02` 单行峰值降 32.9%，多行降 11.7%。多行剩余峰值来自同时持有原文和完整解析树。
  逐项预算内使用借用的数组元素，逐项解析并写入有界排版缓冲。形状、嵌套或输出超预算时回退完整解析。`capacity-ab-03` 多行峰值降 34.5%，首屏中位却慢 27.37ms，其中一对慢 53.58ms。保留该批，后续只遍历一次 `messages` / `input` / `tools` 数组。
  `capacity-aa-01` 用同一 exe 校准首屏，最坏配对绝对差 70.68ms。分段时延帮助定位，最终验收仍看首屏配对门槛。
  `capacity-ab-05/capacity-verification.json` 含五组各三对，均通过绘制和字节数核对。32 MiB 多行和单行的打开峰值增量中位为 101.50 → 66.55、97.27 → 65.24 MiB。首屏中位差 +9.93 / -4.60ms，多行一对 +36.83ms 超过 +20ms 门槛。16 KiB / 1 MiB / 12 MiB 的后台加载和首绘中位差均在 +10ms 内。
  `--wake-on-load` 仅在测试驱动把等待后台完成的轮询改为 2ms。`capacity-aa-02` 校准通过，但 `capacity-ab-06` 多行一对 +71.09ms，后台、绘制和等待分别多 37.86 / 23.03 / 10.21ms。保留未达标记录。
  `--pin-bench-cpus` 仅给测试 PID 固定性能核，应用逻辑 4–11，驱动 0–1，每个采样读回核对。`capacity-aa-03` 最大绝对首屏差 6.91ms。相同条件的 `capacity-ab-07/capacity-verification.json` 六对全部达标：两种 32 MiB 输入的峰值增量中位 101.45 → 66.52、97.23 → 65.27 MiB，首屏中位分别多 9.27 / 1.40ms，最慢配对多 15.85ms。
  在同一机器用 `node .claudesktop/bench/raw-request/run.mjs <新目录> capacity-probe-05 ab --against=peak-probe-01 --prompt-paint --wake-on-load --pin-bench-cpus 32m 32m-line` 复跑，接着运行同目录 `verify-capacity.mjs <新目录>`。
  `capacity-normal-05` 验证普通版打开、切页、关闭、重开。`capacity-edge-05` 验证两种大原文文末查找、切页锚点和全文复制 / 导出哈希。`capacity-race-05` 验证加载中关闭与快速切页。
  普通 release、fmt、全工作区 release Clippy 和 672 项测试通过，13 项忽略。普通 exe 位于 `capacity-release-05/claudesktop.exe`，SHA256 `d4afa6938c57b68fbe1010ce3cd4db04356debad138c4ef2828ed3faceb8612a`。
- **用量索引按行补扫**：`.claudesktop/bench/usage-stream-01/final-ab-01/summary.json` 用 12 个普通隐藏进程验证约 48 MiB 的首次扫描、再追加约 12 MiB 后重启。每对缓存偏移、轮次和用量统计一致。
  首次索引的私有内存峰值中位 206.54 → 158.30 MiB，索引 42.51 → 21.54ms。追加后重启为 169.74 → 159.16 MiB、12.34 → 6.33ms。进程读取量基本持平，追加仅补读新字节。首次扫描的启动首帧中位 291.40 → 325.17ms，未观察到首帧加快。
- **历史重放与磁盘搜索**：`.claudesktop/bench/history-replay-01/ab-01/summary.json` 记录同一约 48 MiB 日志的三轮交替 A/B，12 个隐藏进程。重放峰值私有内存增量中位 123.63 → 58.08 MiB，搜索 100.54 → 0.26 MiB。日志读取到重放完成为 96.21 → 83.77ms，磁盘扫描为 79.26 → 26.68ms。
  搜索峰值在选中命中前读取，从命令面板输入并确认后核对实际跳转。两边 12 次有效对话内容指纹相同，包含回退、短回复、长思考与附件。`normal-02` 的普通版验过有效历史、展开提示词和原图进请求，新回复追加到原会话。fmt、全工作区 release Clippy（-D warnings）、普通 release 构建及 674 项测试通过，13 项忽略。
  用同一 Cargo 源码副本构建 A/B 时，复制源码后刷新修改时间，避免 Cargo 把候选 core 错判为 Fresh（`after-stale`）。`normal-01` 在系统临时数据目录收到请求但等不到日志追加。改用本轮输出下的数据目录后通过，未改系统权限。搜索扫到上限后仍读完当前日志，后续回退可能移除暂存命中。
  复跑 `node .claudesktop/bench/history-replay-01/run.mjs <新目录> ab`，使用该目录冻结的两个探针。
- **请求明细补读与费用汇总**。同一时间范围内按文件身份、字节偏移和尾部校验补读。尾部半行等下次，旧文件有变或手动重新读取时全读。费用一次遍历更新总额、模型和会话，每种模型只匹配一次单价。
  `.claudesktop/bench/request-log-memory-01/append-ab-01/summary.json` 有小、大两组各三轮交替对照。约 40 MiB 的样本追加一条时只解析 1 条，进程读量中位 41583020 → 480 字节，读盘 177.30 → 0.62ms，费用汇总 1506.53 → 13.60ms。刷新开始到绘制为 1825.04 → 278.69ms，前置的 2 秒合批另计。首开峰值增量中位 86.94 → 88.18 MiB。
  `first-append-ab-01/summary.json` 隔离前面的范围切换后，单次追加峰值增量三轮中位 97.62 → ≤11.74 MiB。候选三轮都未超过进程的既有高水位，采样中位 7.06 MiB 只代表下界。`live-append-03` 用真实回环对话核对自动补读和新增费用。首轮结束还会发 `GET /v1/models?limit=1000`，所以这个夹具会新增对话和列模型两条，列模型费用为零。
  fmt、全工作区 release Clippy（-D warnings）、678 项测试和普通 release 构建通过，13 项忽略。`raw-request/request-log-normal-01` 用普通版隐藏窗口验证点开明细和原文、切换请求与返回并重开。普通 exe 归档在 `request-log-memory-01/release-01/`。
  复跑 `node .claudesktop/bench/request-log-memory-01/run.mjs <新目录> indexed append small,large 3 6`。隔离首次追加用 `indexed append large 3 1 --append-only`，真实回环用 `append append edge 1 0 --append-only --live`。
- **请求明细字符串共享与增量归组**。补读保留已有下标，按发出时间把新请求并进涉及的回合，时间相同保留文件行顺序。连续请求用下标区间，交错请求用下标清单。文件替换或范围到期重建归组。费用和首字中位数仍按筛选结果汇总。
  `.claudesktop/bench/request-log-memory-01/grouped-ab-01/summary.json` 是 41,581,587 字节、96,004 条日志的三轮交替对照。首开私有内存峰值增量中位 88.16 → 66.22 MiB，解析完成时的私有内存增量 66.09 → 44.68 MiB，归组阶段再增 20.31 → 9.19 MiB。全量读取与解析 167.61 → 213.22ms，首次绘制 404.29 → 356.86ms。单条追加归组 19.43 → 0.010ms，刷新到绘制 232.65 → 220.86ms，18 次追加均未超过打开或切换范围时的峰值。
  `grouped-first-01/summary.json` 隔离范围切换后仍只读约 480 字节。首次追加的峰值未刷新历史高点，两版采样下界中位 7.23 → 1.71 MiB，历史高点给出的上界 11.42 → 14.88 MiB。上下界交叠，无法确定真实峰值变化。
  `grouped-late-01` 对照了迟到、同刻、筛选、各类费用和原文关联。`grouped-manual-01` 在隐藏窗口点重新读取后核对同文件改写。`grouped-live-01` 由真实回环网关自动补读两条并核对费用。普通版隐藏窗口原文交互见 `raw-request/request-log-normal-02`，exe 在 `request-log-memory-01/release-02/`。fmt、全工作区 release Clippy（-D warnings）、679 项测试和普通 release 构建通过，13 项忽略。
  复跑沿用 `request-log-memory-01/run.mjs`，用 `<新目录> append grouped large 3 6`，隔离首次追加用 `<新目录> append grouped large 3 1 --append-only`。两版 `derived` 探针的计时起点不同，比较归组用 `group_ms`，端到端用 `paint_ms`。
- **请求日志借用解析**。`LogRecord` 共用日志格式，读取时短字符串先从 JSON 行缓冲借用查表，未命中才创建 `Arc<str>`。转义交给 Serde 解码。范围过滤在共享前，旧 JSONL、损坏行和偏移检查规则照旧。
  `.claudesktop/bench/request-log-memory-01/borrowed-ab-01/summary.json` 的三轮交替对照（同一 41,581,587 字节样本）首开读取解析中位 412.93 → 276.18ms，首次绘制 766.97 → 599.38ms，私有内存峰值增量 67.27 → 63.61 MiB。`borrowed-first-01` 的隔离追加对照首开读取解析 303.78 → 195.53ms，首次绘制 393.60 → 381.65ms，峰值 65.27 → 66.43 MiB，首次追加仍读 480 字节。之前的约 170ms 绝对目标本次未达，三套同条件对照的首开读取解析缩短 33% 至 36%。`borrowed-pinned-01` 只绑定本次探针进程的性能核，466.01 → 295.98ms，结果另记。`parse-profile-01.json` 是包含释放开销的隔离诊断，部分采样与编译重叠，验收以桌面对照为准。
  `borrowed-edge-01` 对照乱序、筛选、手动重读，`borrowed-live-01` 核对真实回环追加和费用。fmt、全工作区 release Clippy（-D warnings）、680 项测试及普通 release 构建通过，13 项忽略。普通版隐藏窗口原文验收在 `raw-request/request-log-normal-03`，exe 在 `request-log-memory-01/release-03/`。归档脚本匹配 Cargo 错误时须锚定行首，宽泛的 `error:` 会匹配成功的 `json_error::tests`。
  复跑用 `node .claudesktop/bench/request-log-memory-01/run.mjs <新目录> grouped borrowed large 3 6`；隔离首次追加用 `grouped borrowed large 3 1 --append-only`。
- **请求日志读取缓冲与相邻文本复用**。按剩余字节分配至多 64 KiB 读缓冲，短字符串和上一条保留记录的同一字段相同时直接复用。`profile-02/results.json` 的拆测里解码约 95ms，查表及共享约 36ms，顺序样本排序不到 1ms。直接分割读缓冲和保留 `read_until` 的收益相近，采用后者。
  同一约 40 MiB 日志（96004 条）三套各三轮对照，`buffered-first-01`、`buffered-ab-01`、`buffered-paint-ab-01` 的首开读解析中位分别为 151.82 → 132.00、146.30 → 134.83、151.27 → 135.40ms。9 对都更快，候选最慢 140.04ms。首开峰值增量中位 65.32—66.27 MiB，首次追加仍读 480 字节，未刷新历史峰值。
  按 250ms 离屏定时器绘制的两组首绘中位分别多 19.61 / 24.68ms。后台提交后由驱动立即请求绘制的三轮中位 220.67 → 206.81ms；`buffered-paint-aa-01` 同版最大配对首绘差 28.10ms，首绘收益仍待确认。补测用 `borrowed buffered large 3 0 --append-only --prompt-paint --pin-bench-cpus`，只绑定本次探针及驱动 PID。
  `buffered-edge-01` 对照迟到、同刻、筛选与手动重读，`buffered-live-01` 核对回环对话的两条网关增量与费用。fmt、全工作区 release Clippy（-D warnings）、680 项测试和普通 release 构建通过，13 项忽略。`raw-request/request-log-normal-04` 完成普通版请求原文交互，exe 在 `request-log-memory-01/release-04/`，SHA256 `7fa097397916136be8dd70ecf09366f8e86997047d249df8a99386a07be340ab`。
- **请求明细首帧跳过屏外行**。加载占位时量起点，若还在视口和预排区下方，数据到齐的首帧先用估算高度占位。首帧已接近视口时正常排行，列表自己的 prepaint 更新实际起点。用量页与首页请求页签切换时重新量起点。
  `request-log-memory-01/frame-profile-01` 在后台提交后直接离屏绘制，未先画加载帧时，首帧会排 20 个屏外行，布局至内容 prepaint 中位 33.60ms。`frame-ab-01` 两版都先画加载帧，再在提交后直接绘制。约 1 / 40 MiB 样本各三对，首帧均为 20 → 0 行。40 MiB 布局 15.75 → 3.12ms，读解析以外 42.32 → 28.95ms，读取开始到首绘 186.32 → 165.13ms。1 MiB 首绘 70.48 → 61.38ms。`frame-aa-01` 的同版布局最大配对差 6.03ms，首绘差 38.38ms（主要在读解析）；按同组布局衡量收益。
  大日志首开私有内存峰值增量中位 66.70 → 66.90 MiB，单条追加继续读 480 字节。`frame-scroll-01` 切范围再滚入明细后截图逐字节一致，`frame-visible-03` 组合高窗与空会话统计夹具验证首帧在屏内的首行。高窗会被工作区限制实际客户区，单设尺寸的 `frame-visible-01` 只覆盖了接近视口的场景，`frame-visible-02` 的默认窗口也未达到可见断言。`frame-edge-01`、`frame-live-01` 覆盖迟到、筛选、手动重读及真实回环网关追加。
  fmt、全工作区 release Clippy（-D warnings）、680 项测试和普通 release 构建通过，13 项忽略。普通 exe 在 `request-log-memory-01/release-05/`，SHA256 `84c0956dd4c81b4216e74e66104b3353f9d332749b2e9f7f8ddad2eb56da94d6`，`raw-request/request-log-normal-05` 验过展开、原文请求与返回切换、关闭和重开。
  复跑 `node .claudesktop/bench/request-log-memory-01/run.mjs <新目录> frame-baseline frame-candidate small,large 3 1 --append-only --draw-on-commit --draw-loading --pin-bench-cpus`，用同目录 `frame-results.mjs <结果目录>` 看配对数据。
- **请求明细离页回收**。请求与归组数组容量合计达到 16 MiB 时，离页约 30 秒后释放明细、归组、游标和排版缓存。快速返回与小日志复用缓存。返回时按选定范围后台重读。
  `.claudesktop/bench/request-cache-01/ab-01/comparison.json` 用约 40 MiB、96,004 条日志做三轮交替普通隐藏窗口对照。离页 33 / 35 / 37 秒采样，进程 PB 中位 170.54 → 122.05 MiB，WS 113.13 → 76.18 MiB。快速返回中位 98 → 116ms，过回收期返回 115 → 337ms（点开到界面可操作，含离屏绘制与采样）。部分运行与验证构建并行，小差值受负载影响。
  `smoke-range-02` 用 7 天范围核对重读从 64,004 增至 64,005 条；`smoke-small-01` 的 2,404 条样本离页后继续复用。`expand-03` 核对应用模型筛选后，同一展开轮重开仍显示两处原文入口。首次窗口驱动的原子改名遇短暂 `EPERM`，只重试测试命令文件。普通 release 候选在 `request-cache-01/release/claudesktop.exe`，SHA256 `5b8c80c4922176ec71c4f8a8d5c85094ac7e0aa603ed1c74de7a1d2664b83431`。
  自动模式写入集成测试的临时目录不能放项目 `.claudesktop/` 内，否则会命中保护目录。改回默认临时目录后 683 项 workspace release 测试与 Clippy 通过（13 项忽略）。
- **温会话恢复减少大文本复制**。读取时校验内容哈希及全部引用长度，校验通过后每个块最后一处引用接收原缓冲。引用错误时保持所有预览，修复后可重试。
  `request-cache-01/warm-smoke-01` 用三个温会话和本地端点验证 50 次请求、24 次界面指纹和 38 次历史核对；坏块时消息留队、修复后只发送一次，退出清理通过。此轮恢复 p95 6.406ms，属于单版快查。

- **连接中的自研会话回收显示数据不能整对象换占位**：`from_records` 会清内核绑定，也丢工具图片和本轮文本附件。
  新路径在 `session/cache.rs` 只搬 `transcript` / `subagents`，以原子临时快照保存；保留同一个 `Session`、模式、队列和界面状态。
  后台任务按 token + 快照的 `Arc` 身份接回，删除中的会话不会被迟到结果复活；写失败把原数据交回，读失败保留快照、不降级成日志续接。
  内容区先检查显示数据是否加载，再判断子任务详情页，避免加载的空档误清 `viewing_task`。按需日志加载替换对象时也要搬回期间排队的消息。
- **显示回收 A/B 要隔离内核变量**：`tools/hibernate-bench.mjs --compare=display` 两边都开内核休眠，只切界面层；不能沿用旧测量的 `CLAUDESKTOP_DISABLE_RECLAIM=1`。
  启动时的空白 builtin 会话不计入「已加载的有内容会话」，否则 4 会话快查会误数成 5 个；实际内存仍按完整进程 / 进程树测。
  50 会话三轮已通过：PB 中位数 272.10 → 234.88 MiB（省 37.23），WS 179.95 → 142.19 MiB（省 37.76）；每轮各省 PB 21.56 / 37.23 / 46.46 MiB。
  最差界面数据读取 / 解析 / 回 UI 的 p95 为 6.405ms，预恢复 200ms 后发送额外等待 p95 最差 +2.295ms；每轮配对峰值均下降。
  这组增加了界面内容指纹校验，绝对占用不能直接与上次内核 A/B 的数字拼接；原始结果在 `.claudesktop/bench/display-ab50-01/`。
  200 会话三轮也通过：PB 589.34 → 438.61 MiB（省 150.73），WS 467.83 → 316.91 MiB（省 150.92）；三轮各省 PB 135.43 / 154.92 / 159.89 MiB。
  最差界面数据恢复 p95 5.190ms，预恢复后发送额外等待 p95 最差 +9.275ms；配对 PB 峰值比最差 0.8201。结果在 `.claudesktop/bench/display-ab200-01/`。
  两个规模合计 4506 次本地主会话请求、1500 次界面指纹和 1506 次模型历史前缀校验；实际采样最大间隔 809ms，峰值用 Windows 保存的 PeakPagefileUsage。
  文件控制和每 250ms 离屏绘制会增加压测 CPU / I/O，不能当普通应用空闲开销；显示数据只在落盘 / 唤醒时读写，不增加模型请求。
  长测分组结果逐组写盘，命令超时可用 `--resume` 复用已验证完成的组；中断组先保留到另一个目录，避免把半组数据当结果。
  最终源码已通过 610 项 workspace 测试、fmt、常规 Clippy 和普通 release 构建。界面快查还覆盖损坏快照 → 消息留队 → 修复后只发送一次，以及写入失败回滚和写入期间关闭清理。
  普通正式 exe 的真实窗口消息验收（4 会话、7 次主会话请求）也通过：运行中保护、无损恢复、取消、退出清理两层快照和重启续接；窗口始终隐藏且未到前台。
  原始记录在 `.claudesktop/bench/display-smoke-02/`、`display-release-02/`；普通 exe 在 `D:/t3/release/claudesktop.exe`，SHA256 `0d7922dfffb4bdaf5307b2bffcd3a0770a85605d4552b89f4e8dbdef206b57b6`。
  本机快查空目录首帧 253.49ms、4 会话重启首帧 499.87ms；未做冷缓存启动 A/B，不能据此推断启动变快。发送时跟随底部必须清掉待恢复的旧滚动锚，避免旧锚盖掉新消息的位置。
- **当前 HEAD `8bd19d0` 复验（2026-11-15）**：CI 成功；本地 fmt、Clippy、611 项 workspace 测试和普通 /
  截图 release 构建通过。50 会话三轮界面回收 A/B：PB 中位数 277.26 → 250.52 MiB（省 26.74），
  WS 181.75 → 149.64 MiB（省 32.11）；每轮各省 PB 28.64 / 41.48 / 18.66 MiB，最差配对峰值比 0.85074。
  界面恢复 p95 最差 4.458ms；预恢复后发送额外等待 p95 最差 +14.696ms，仍低于 20ms 门槛，
  但高于上一批的 +2.295ms，后续不能漏看这项。原始结果在 `.claudesktop/bench/head-display-ab50-01/`。
  ZIP 内普通 exe（SHA256 `f58e132b3dcb678d7daf60ed6b85902344dd24cd32318e7ea246af357a71207f`）的真实窗口消息快查也通过：
  4 会话、14 次请求、28.56 秒触发双层休眠，运行中保护、2 次精确历史校验、流取消、退出清理和重启续接成功；
  全程隐藏且未到前台。结果在 `.claudesktop/bench/head-release-fast-05/`。
  截图构建的故障注入 / 混合快查同样通过：33 次请求、19 次历史校验、4 次界面指纹；损坏界面快照后消息留队、
  修复后只发一次，读写 / 提问 / 子任务 / 后台命令 / 取消 / 压缩 / 回退 / 关闭各覆盖一次。
  结果在 `.claudesktop/bench/head-display-smoke-01/`。
  不设休眠开关和 idle 阈值的正式包验收也通过：603.464 秒后自动双层休眠，随后恢复 / 运行中保护 /
  停止 / 正常退出清快照 / 重启续接全部成功。结果在 `.claudesktop/bench/head-release-default-04/`。
- **M0 渲染缓存账本（2026-11-15）**：50 个不同图片 / 公式 / 长 Markdown 会话，交替三轮的原始结果在
  `.claudesktop/bench/m0-render-ab50-01/`。每轮工具图片内联字节 44.5 MiB、公式资源约 33.8 MiB、
  排版缓存约 5.1 MiB；解码图按 384×768 首样本已观察 22.5～37.1 MiB，合计远超 32 MiB 决策门槛，进入图片改造。
  `ImageCacheItem` 完成后的字节只在下次 `load/get` 才入账，离开视口后没再用的项会暂时显示 loading，不能拿这个字段当真实在途请求；
  后续样本改成 512×1024（约 100 MiB）且每次激活显式跳到图片行。M0  workspace 612 项测试通过；既有内核休眠在该样本有 2 组峰值比 >1.10，不能算图片阶段验收。
- **温会话压测别造一整段无换行文本**：8 MiB 单段 Markdown 曾把三个会话的 PB 放大到约 2 GiB，主要测到排版派生数据；
  改成多段、每会话 2 MiB 后才是可复用的温会话样本（`--sample=warm`，3 会话、8 轮频繁切换，24 次内容指纹校验）。
- **资源表持续写盘失败会死循环**（M1 故障测试实锤）：失败条目重新插回并标成新，两条会轮流成为最旧项，永远淘汰不完；
  现在每次 `trim` 最多尝试原有条目一轮，失败保留原字节并返回，回归测试 `failed_spill_keeps_original_bytes_and_missing_reads_return_none`。
- **图片缓存注入别包整个主区**：`ImageCacheElement` 替换原 body 会让内容区高度变 0、输入框跑到顶栏下；注入只放 `content_slot`
  原来的 `size_full` 位置。静止可见图片不一定每帧再调 `ImageCache::load`，只看时间戳会误淘汰；还要把缓存条目和
  `get()` 临时副本之外的 `Arc<RenderImage>` 引用当作钉住。截帧控制 `capture` 先等 `history_loaded`，标准视图只展开过程组，避免长思考挡住图片行。
- **图片 A/B 必须隔离内核休眠**：`m1-image-ab50-01` 同时切了内核休眠，PB 中位省 334.59 MiB 不能全归图片，留作诊断；
  正式对照两边都开同一内核休眠，只切 `CLAUDESKTOP_DISABLE_IMAGE_RECLAIM`，结果看 `m1-image-ab50-03`
  （`02` 的断言仍按开关推断休眠状态，首轮等错目标；已修为按比较类型判断）。
- **M1 图片缓存验收通过（2026-11-15）**：50 会话 × 3 轮隔离 A/B（两边同一内核休眠）在
  `.claudesktop/bench/m1-image-ab50-03/`。PB 中位省 113.875 MiB、WS 中位省 104.574 MiB；
  每轮 PB 分别省 104.504 / 106.688 / 115.195 MiB，最差峰值比 0.91617，唤醒 p95 最差 8.468ms，
  预恢复后发送额外等待 p95 最差 +3.532ms。开启组非钉住解码字节始终 64 MiB，加载中计数为 0；
  原始图片字节两组同为 39.52 MiB，工具图片内联引用仅 3350 B。首轮开关节截帧 PNG 字节完全一致。
- **M2 首轮不能直接算通过**：`m2-math-ab50-01` 的 PB / WS 中位分别省 17.305 / 11.367 MiB、
  最差峰值比 1.00331，公式缓存严格 8 MiB，截帧与 M1 字节一致；但第二轮发送等待 p95 多 31.243ms，
  超过 20ms 门槛，其余两轮为 -14.215 / -1.410ms，像尾延迟噪声但不能据此豁免。复验在 `m2-math-ab50-02`。
  SVG 图集的 `PlatformAtlas` 没有占用读取接口，未拿 SVG 源码字节冒充图集占用，也未改 vendor。
- **M2 公式去重验收通过**：混合图片样本两轮噪音较大（`m2-math-ab50-02` 中位 PB 为负），改用公式专用样本并采用
  4 MiB 排版缓存后，`m2-math-only-ab50-03` 三轮全部通过。PB 中位省 5.438 MiB、WS 中位省 8.402 MiB，
  每轮均为正；最差峰值比 1.02880，唤醒 p95 最差 2.310ms，发送额外等待 p95 最差 +2.556ms。
  `Formula` 只留内容哈希 / 尺寸 / 源码，排版按实际字节 LRU；workspace 619 项测试通过。
- **M3 温会话块缓存快查**：`m3-warm-smoke-02` 通过 24 次频繁切换内容指纹、损坏块后发送留队、
  修复后只发一次和正常退出清理；恢复 p95 5.388ms，两个非活动会话外置 4 项 / 5.47 MiB。
  `m3-warm-smoke-01` 仅因 `display-cache/blocks` 空父目录被遗留检查判失败，已补最后一个会话释放时清父目录。
  块恢复先读全并校验，再一次性替换预览；别让错误闭包吞掉写失败，也别在失败时清块文件。
- **M3 温会话三轮 A/B 通过**：`.claudesktop/bench/m3-warm-ab-01/`。PB 中位省 9.832 MiB、WS 中位省 7.359 MiB；
  每轮 PB 分别省 8.809 / 6.980 / 10.738 MiB，最差峰值比 1.03349，恢复 p95 最差 4.670ms，
  发送额外等待 p95 最差 +8.184ms。每轮 24 次内容指纹一致，无功能失败或异常回退。

- **M4 静态验收（2026-11-15）**：fmt、常规 Clippy、普通 / 截图 release 构建均通过；workspace 623 项测试通过。
  阶段提交为 `f036162`（账本）、`ca3c184`（图片）、`15c7f32`（公式）、`f266dc2`（温会话），未改版本号或 tag。
  既有 50 会话显示休眠回归 `m4-display-ab50-01` 三轮通过：150 次内容指纹与模型历史前缀校验，
  最差峰值比 0.82136、显示恢复 p95 5.583ms、发送额外等待为负值；200 会话回归在 `m4-display-ab200-01`。
  `m4-display-ab200-01` 的 600 次界面指纹和 600 次模型历史前缀校验通过，峰值 / 唤醒 / 发送门槛通过，
  但第二轮 PB 为 -21.766 MiB，脚本总门槛未过；复验 `m4-display-ab200-02` 三轮全部通过：
  PB 中位省 31.348 MiB、WS 中位省 32.570 MiB，每轮 PB 分别省 55.566 / 28.563 / 31.828 MiB，
  最差峰值比 1.01609，显示恢复 p95 最差 5.261ms，发送额外等待 p95 最差 +2.863ms。
  普通 exe 快查 `m4-release-fast-01` 和默认路径 `m4-release-default-01` 均通过：4 会话、14 次请求、
  默认 604.137 秒自动双层休眠、恢复 / 停止 / 清理 / 重启续接成功；首帧 277.87ms。
  默认路径最后 60 秒 CPU 为单核 1.594%、读 1052 B / 写 5537 B，包含每 250ms 无头离屏绘制开销，不能当普通产品空闲值。

- **冷会话回收的首次实测（2026-11-14，50 会话×3 轮 A/B，`CLAUDESKTOP_STRESS_SESSIONS`）**：
  逻辑数据确实回收了（`[mem]` retained 43 MB → 3 MB、loaded 45 → 4，淘汰 15 个省 ~15 MB），
  **但主进程 PB 没降**——两组末段稳态都是 ~186 MB。原因：Windows 系统堆不把释放的页
  还给系统（提交停在高水位），且 2/3 轮重加载又重新分配同量内存，峰值反而多 ~14 MB
  （201 vs 187）。WS 只降 ~6 MB（132 vs 138）。结论：**冷回收挡住了「无限增长」
  （不回收时 loaded=50 常驻），但要真降 PB 得分配器配合**（会话独立 arena、HeapCompact、
  或换 mimalloc 这类带 decommit 的分配器）——这是下一批的事。数据在
  `tools/mem-baseline.csv` / `tools/mem-reclaim.csv`。重加载的峰值代价也值得注意：
  高频来回切 50 个会话时，重分配可能让高水位不降反升。
- **资源登记表（公式 SVG / 远程图片）别再用 `Box::leak`**（2026-11-14 改）：现在字节装在
  `Arc<Vec<u8>>` 里、按 24 MiB 预算 LRU 淘汰，挤出内存的写到数据目录 `cache/assets/`，
  `Assets::load` 未命中内存再从盘读回。闭环成立的前提：**`svg().path()` 每次图集未命中
  都会重新走 `AssetSource::load`**（`window.paint_svg` 只按名字查 sprite atlas），
  所以登记表条目放掉后名字仍有效；`img()` 那边解码副本另存在全局 `RetainAllImageCache`
  （按 `hash(Resource)` 持 `Arc<RenderImage>`），登记表释放省的主要是原始字节那份。
  单条超过 12 MiB 的图不入表（一张图占掉大半个预算不划算，按「读不出来」处理）。
- **会话冷回收**：`Session::evictable` 豁免一切活状态（忙 / 排队 / 等应答 / 连着内核会话
  `acp_id.is_some()` / 加载中 / 读取失败）；`evict_to_placeholder` 换占位时**令牌不变**——侧栏、
  `last_active`、`session_index` 都按令牌引用。温会话保 2 个 + 32 MiB 预算（初始提案，
  `app/mod.rs` 的 `WARM_SESSIONS` / `SESSION_CACHE_BUDGET`），`CLAUDESKTOP_MEM_REPORT=1`
  时每 30 秒回收周期打一行 `[mem]`（会话保留字节、资源登记表现状）。
  记忆点：占位会话的 `has_user_message()` 也为真（按元信息算），删除保护照常工作。
- **手动离屏绘制必须清帧 arena**：调用 `window.draw(cx).clear(cx)`，不要丢掉 `draw` 返回的 `ArenaClearNeeded`。
  正常呈现会自动清，手动循环不清会让临时元素随帧堆积，污染内存验收；原截图 / 滚动测试入口也已一起修正。
- **无头截图不要只设 `QUIET`**：它仍会显示窗口。用 `--features headless-screenshot` 编译，设 `CLAUDESKTOP_SCREENSHOT=<PNG 路径>`，
  窗口从创建起隐藏，手动排帧后用 `render_to_image` 离屏导出并退出，不触碰前台。
  配 `CLAUDESKTOP_OPEN=model-menu` 可预览真实默认模型菜单；用组件的 `default_open(true)` 初始化，不用改受控模式。
  不开 `gpui-kit/test-support`（会牵出 gpui-fast 的 Cocoa 版本冲突），截图 feature 只开启现有 gpui / platform 的测试接口。
  Windows 的 `show=false` 原本不应用初始 bounds（只存 placement），截图总是默认尺寸；已在 vendor 隐藏分支用禁止激活 / 改层级的 `SetWindowPos` 修正。
  `CLAUDESKTOP_SCREENSHOT_SIZE=900x650`、`CLAUDESKTOP_SCREENSHOT_SCALE=140` 可验窄窗和大字号；只在截图构建中生效，不写用户设置。
- **流式输出的真正成本是「每个 chunk 整篇 markdown 重解析一次」**（2026-11-13 实测）：
  `reading_markdown` 流式时补齐半截标记（`\n``` ` 之类），交给 TextView 的文本每帧都不是上一帧的
  前缀，gpui-kit 的增量解析（`TextViewState::push_str`，前缀追加只解析增量）整条用不上——
  压测里 7 KB 的流式消息解析一帧 p95 3ms+、max 10ms，消息越长越贵。修法不是改解析，
  是合批：chunk 先进 `Session::stream_pending`，50ms 才并进正文一次（300 chunk → 32 次上屏），
  没并进的 chunk 连 `cx.notify()` 都跳过。另外 `flush_stream` 不分思考 / 正文一次全并，
  正文 chunk  handler 里「先收尾思考」不能直接调它（会把正文缓冲也提前并掉，合批失效），
  要先看 `stream_pending_thought`。
- **PowerShell 5.1 的 `Start-Process -PassThru` 可能拿不到退出码**：截图已生成且 `WaitForExit` 返回真，`ExitCode` 仍可能为空。验收同时要核对退出码时，用 `[Diagnostics.Process]::Start(ProcessStartInfo)` 启动并等待。
- **压测只收尾自己启动的 PID**：Git Bash 的 `$!` 可能是包装进程，不要按 `claudesktop.exe` 名称整批杀进程。
  用 Node `spawn` 取得真实 PID 并等待退出（新休眠验收脚本已如此处理）；未退出的实例会占住 exe，导致下次链接失败。
- **压测数字本身有 ±0.5ms 漂移**（机器负载、热状态）：滚动 p50 同一基线不同时间跑出 2.56→3.94ms。
  对比优化前后要交替 A/B 连跑几次取中位，别拿两次单跑说事。
- **模型页压测后不一定回到顶部**：`CLAUDESKTOP_STRESS_VIEW=settings:4` 的合成滚轮落在右侧模型清单时，
  内层滚动区会先吃掉事件，往返后外层仍可能停在底部；截首屏要在左侧分类导航上滚回顶部，别把压测中的画面当成初始位置。
- **无前台且窗口被遮挡时，滚轮压测可能一直不出统计**：`on_next_frame` 等不到可见窗口的正常呈现，别把超时当通过，
  也别为了拿数字抢前台；功能验证可用 `WM_MOUSEWHEEL` 的 `PostMessage` 驱动真实窗口，再用 `shot.ps1` 截图。

- **实时预览槽已有截图钩子，别另起一个**（2026-11-25）：`CLAUDESKTOP_OPEN=live-preview:<thinking|tool-empty|tool-output|tool-other|parallel|tool-done>`
  在 `ChatApp::debug_live_preview` 里摆状态（仅 `headless-screenshot` 特性）；新增预览样式先往这里加 kind，
  不要再建 `CLAUDESKTOP_PREVIEW_*`（会撞同名方法）。
- **`D:/t3/debug/claudesktop.exe` 被正在运行的实例占用时链接报 `os error 5`**（2026-11-25）：
  先 `Get-Process claudesktop` 看 PID / 路径，确认是自己起的才关；用户前台开着的实例不要动。
  不想关它时可以把 `claudesktop.exe` / `.pdb` `Move-Item` 成别的名字（运行中的镜像允许改名、不允许删除），
  再正常构建——老进程照常跑，名字留到关闭后再删。
- **验证 UI 别用会抢前台的招**：`tools/shot.ps1` 走 PrintWindow，不抢焦点不移动光标，
  随便截；`tools/demo-drive.ps1` 要置顶 + SetCursorPos + 点击，会打断用户手头的事，
  用户明确要求"起无头的、不占前台"后就不要再用它驱动交互。交互逻辑改走单元测试
  （把逻辑提成纯函数测，比如 `BuiltinSettings::menu_flat`）；
  另外合成分发点不到 Popover 的 trigger（普通 on_click 元素可以），弹层类交互别指望它。
- **输入卡片工具栏的按钮被顶到卡片外面**（发送 / 停止键飘在右下角画出来）：工具栏是
  `h_flex` 一行不换行，而 `MenuChip` 的标签是一串不可断行的文字、`Popover` 的 trigger 容器
  按内容撑到 min-content，模型名（`cn:deepseek-v4.1-flash`）这种长单 token 缩不下去，
  加上字号放大就到卡片右边去了，卡片又没裁切（gpui 不自动 clip）就照原样画在外面。
  修法：`MenuChip::shrink()`（`flex_shrink_1` + `min_w(CHIP_MIN_W)` + 标签 `min_w_0`/省略号），
  `menu_popover` 对可缩 chip 的 trigger 容器也套 `trigger_style(flex_shrink_1 + min_w)`——
  **只给 chip 设 min_w 不够**，外层 trigger 撑在 min-content 上，里面照样缩不了；
  发送键外层 `flex_shrink_0`，卡片 `overflow_hidden` 兜底（deferred 弹层不受它影响）。
  **新的 chip 里放长度不由我们定的文字（项目名 / 模型名 / 分支名）时记得 `.shrink()`。**

## 内核 / 工具

- **插话（steer）只能在步间注入，且不能跟新消息抢第 0 步**（2026-11-25）：`run_steps` 第 0 步正上方
  刚由 `prepare_prompt` 提交用户消息，此时再取插话就成了连续两条 `user`（Anthropic 400），所以第 0 步
  不收、留到第 1 步；模型说完最后一段话时若 `pending_steer` 非空要 `continue` 再跑一步，否则输入框
  已清空的插话既没送到模型也没退回；步数耗尽 / 取消时由 `finish` / `cancel` 丢弃并发 `Notice`，
  绝不能让它在下一轮被幽灵注入。协议侧还有两处配套：Anthropic 的工具结果后面紧跟插话要并进同一条
  `user` 消息（`wire_messages`），OpenAI 兼容端点开头多条 `system` 要合并成一条（`request::WireMessages`，
  vLLM / Ollama 模板只认一条）。界面侧目前只允许插纯文字——`SteerInjected` 不带附件，带图 / 文件插话
  会在气泡和续接历史里缺内容。
- **`ask_user` 确认要进入自动分类器的上下文**（2026-11-25）。此前主模型收到了工具返回的答案，分类器只读对话消息，用户点了同意后仍会被拦。实时应答和日志重放共用 `auto::UserInput::question_answer`，只提交选中的答案及其题目和描述。沙箱内回环 HTTP 测试报 Windows 10013，可用 `cargo test -p claudesktop-builtin --lib auto`，进程内模拟分类器会检查当前回合、续接和回退的请求内容。

- **提示词里禁止「工具前旁白」会把阶段汇报也一起压掉**（2026-11-24）：10-23 的提速改动加了「don't narrate what you are about to do before a tool call」，
  同一模型在重度轮次里的中间汇报从每轮 8.4 次掉到 1.0 次，93% 的中间请求正文为空，最长连续 33 分钟不出声。
  改成只禁逐步旁白，并点名阶段完成、计划变化、卡住三种要汇报的时机（只注入主会话：子会话被告知没人看得到它、
  也不能提问，两边的指令会打架）；内核在连续 10 步静默（`PROGRESS_REMIND_STEPS`）或连续 3 步的工具调用全失败
  （`FAILURE_REMIND_STEPS`，按步不按调用，避免并行试探几条路径被当成卡住）时挂一次 `<progress-reminder>`
  （只进这一次请求，不进历史）。
  **提醒标签别用 `<system-reminder>`**：现有清单提醒测试按这个子串断言，第 10 步就会撞上。
  计数器是 `run_steps` 的局部变量（只管这一轮，续接 / 回退不用存）；聊天会话不提醒（没有长工具链）。
  同时 `todo_write` 改回常驻：清单是用户看进度的主渠道，搜一次才能用会让模型干脆不用（13 个长会话里 11 个一次没加载）。

- **工具定义共享（2026-11-17，固定测量条件下验收通过）**：`ToolSpec` 用 `Arc<Value>` 持定义；MCP 清单更换时替换整份定义，请求只克隆引用，搜索只取名字 / 说明，调用目标不带 schema 副本。三个 provider 借用工具字段序列化。
  `serde_json` 在本仓库启用了保留插入顺序：Anthropic 工具原顺序是 name → input_schema → description → cache_control，Responses 是 type → name → description → parameters → strict；工具在请求顶层也要占回原来的字段位置。首轮字节断言抓到顺序差异，修复后内核 release 196 项单测通过。
  大型 MCP 基准在 `request-memory-bench.mjs` 加 `--mcp-tools=0,200 --loaded-tools=none,small,all`，默认把 JSON / SHA 校验移到轮后，保留 20ms 发送 / 整轮门槛；A/A 要求同一 exe 哈希且看绝对差。原文只归一化隔离 cwd 后按字节校验，同时核对完整 JSON。
  `tool-memory/smoke-01` 和 `cache-path-check-01` 的请求成功，但夹具额外依赖的 C 盘临时目录 mcp-tools.json 未出现；同夹具的旧基线 `cache-baseline-check-01` 也如此，未确定磁盘缓存未生成的原因。schema 体积改为测试前从模拟 MCP 的真实 tools/list 应答采集，不依赖缓存文件。
  `smoke-02` 已过三协议 × 未加载 / 加载 3 个 / 加载 200 个共 540 次真实请求，实际 MCP 清单 204 项、schema 序列化字节 3500336；旧基线与候选 OpenAI 全加载的 60 次请求，JSON 和归一化隔离 cwd 后的原文字节均一致。本批与编译并行，只算功能快查，性能值不用于收益结论。
  普通桌面夹具也支持 `--mcp-tools=200 --loaded-tools=all`；从日志重放已加载清单，预热后等 MCP 连接再测。`desktop-normal-03` 的第 16 次 Enter 未触发新用户消息 / 请求，保留失败；最终用 `--window-layout=current` 按居中阅读列 / 264px 侧栏 + 6px 手柄计算发送按钮位置，`legacy` 保留旧左对齐坐标。只向指定隐藏窗口发鼠标消息，发送方式记录在 summary 中，旧结果不能与新驱动直接拼接。
  图片要核对像素：read_file 会把 PNG 重新编码，不能要求发给模型的 base64 和源 PNG 字节相同。接收端解析实际三协议的图片结构，校验 1×1 夹具的 RGBA（100/120/140/255）；Anthropic 另断言原思考签名，`desktop-normal-02` 的错误字节断言失败保留。
  本轮工作区 release 659 项通过、13 项忽略，fmt / all-targets release Clippy（-D warnings）和普通 release 构建通过。普通 exe 为 `D:/t3/release/claudesktop.exe`，SHA256 `efdff176f9b7e6e67f4db2bea2e2a9bae118a3dc48590267f3d1ff26c4e5796f`；`desktop-normal-04` 三协议成功 189 次主请求 + 3 次标题，186 次图片像素与 62 次签名校验，保存原文请求 / 响应校验完整，窗口始终隐藏且正常退出。单版本快查的性能状态为 null。
  `aa-01` 同一 exe 三协议 × 三轮的 1080 次请求内容均一致，但仅 1/9 对延迟达标，最大绝对发送 / 整轮 p95 差为 53 / 51.010ms，A/B 未启动。原始失败保留。
  用现有 Node v22.22.1 SDK 编译本次测试端点探针（`tool-memory/tcp-buffer-helper.cc`），真实连接读回 SO_RCVBUF 65536 → 1048576，控制 Node 请求 1ms 定时并在退出释放；只改夹具条件，未动产品发送路径。`aa-02` 固定条件后 9/9 通过，1080 请求，最大绝对发送 / 整轮 p95 差 7 / 4.9039ms，每个接收缓冲都实读验证。
  `ab-02` 三协议 × 零 MCP / 200 项 MCP 的未加载、加载 3 个、加载 200 个 × 交替三轮，共 36 对 / 4320 次真实请求全部达标；2160 次成对 JSON 与仅归一化隔离 cwd 的字节校验全部一致。全矩阵最差额外发送 / 整轮 p95 为 +1 / +1.0283ms，峰值比最高 1.02212，候选末轮闲置 PB 增长最差 2.137 MiB。
  200 项全加载的内核 PB 峰值三轮中位：OpenAI 53.078 → 37.816 MiB、Anthropic 50.672 → 36.641、Responses 51.805 → 35.188；两中位差为 15.262 / 14.031 / 16.617 MiB，全加载 9 对每对均降（至少 13.480 MiB）。每组 60 请求的 CPU 中位分别为 1781.25 → 500 / 1656.25 → 546.875 / 1687.5 → 453.125ms。
  未加载和只加载 3 个的峰值并非每对都降，最差各多 0.621 / 0.676 MiB；零 MCP 的 OpenAI / Responses CPU 中位各多 31.25 / 15.625ms（60 请求，Windows 计量步长 15.625ms），不宣称所有场景 CPU 都下降。数据只代表短历史 + 合成大型 schema 的内核测试进程，MCP 服务与桌面 UI 另计，不与此前历史 / 网关节省量相加；本次通过不替代旧网关原条件的桌面总验收。
  共用 Cargo target 的测试 exe 名字相同，候选构建会覆盖基线：基线必须先归档校验。本轮冻结 SHA 为基线 `fc84f666ee216278d4c9625bb614c55d2881ba82d19d5e2e2a8bc34e21c46192`、候选 `6d09f1d5be61241473bb79bb87b232332c69dcf773124045fe831455ea0a7774`；实际运行副本位于 `D:/t3/tool-memory/{baseline,candidate}/claudesktop.exe`，与归档字节一致。`verified-results.json` 和 `comparison-controlled-receipt.json` 记录复核结果，源码与两边测试夹具哈希复核一致。

- **ACP 通知可能早于 `Started`**：agent 会在 `session/new` 响应到达前先推命令 / 用量 / 压缩，
  此时界面还没有 `acp_id`，直接按 sessionId 找会话会全丢。先在 `AcpShared::early` 缓存，`Started` 绑定后按序回放；
  `AvailableCommandsChanged` 还要刷新已打开的 `/` 面板，否则晚到 Skills 永远不显示。
- **ACP form 不能直接套自研提问的提交逻辑**：把第一个文本答案单独包成 `Form` 会提前唤醒 agent，丢掉后续字段；
  多选若点一项就推进又无法再选。现在用 `text_answers + answered` 暂存进度，多选显式确认，全部字段完成后按原类型一次提交。
  `question_cursor` 对旧快照仍按 `picks` 推断；数值拒绝 NaN / 无穷。验收图是 `.claudesktop/bench/acp-m3-elicitation.png`。
- **终端登录必须等真实进程退出**：Windows 用 `cmd /c start "" /wait …`，漏掉 `/wait` 会把「窗口已打开」误当登录成功；
  等待放在连接任务里，不能卡住其他会话；Agent 的 `authenticate` 也要 `connection.spawn`，不能在指令循环里直接 await。
  回归测试用 2 秒慢登录，要求另一个 `session/new` 在 1 秒内返回。macOS 当前明确报不支持，Linux 走 `x-terminal-emulator`，两者都未实机验收。
  Agent 登录的隐藏窗口重试已验收（`.claudesktop/bench/acp-m3-auth.png`），真实终端登录和付费 Harness 未实测。
- **Git Bash 会把 `/skill` 这类环境变量转成 Windows 路径**：截图调试的 `CLAUDESKTOP_COMPLETION` 曾变成
  `D:/Program Files/Git/skill`。Unix 风格值前加 `MSYS_NO_PATHCONV=1`，别在产物里兼容这种路径误转换。
- **ACP 的 `content` 与 `rawOutput` 不能共用一个输出字段**：`content` 是全量替换的结构化展示内容，
  后到的 `rawOutput` 若覆盖同一字段，会把此前的资源文本 / 图片说明冲掉。现在 `ToolCallContent` 独立保存并入日志，
  `ToolCallUpdated` 继续管原始输出；`ToolCallDiff` 也必须显式加入 `Record::worth_keeping`，否则重启后 Diff 消失。
- **视觉验收别用 1×1 透明 PNG**：它能通过协议和图片解码断言，却在画面上完全不可见。工具图片验收用 32×32
  红蓝 PNG，并额外数实际像素；带 Diff 的工具卡也要检查图片和结构化内容，不能只验证 wire 事件。
- **自动演示在懒启动后要统一走 `submit_to`**：只给新会话写 `autoprompt` 不会启动内核，截图会一直停在首页。
  隔离数据里预写 ACP 档案和默认内核，避免 `CLAUDESKTOP_KERNEL=acp` 在自定义档案创建前解析，及本机 magpie 探活干扰。
- **网关统计不能直接用 `IgnoredAny` 跳过正文**（2026-11-16）：serde_json 的跳过路径不按 `Value` 的规则检查数字范围和嵌套深度。
  现在用 `deserialize_any` 的访问器消费值，只保留模型 / 会话标记和两个数组长度，重复键仍取最后一个、`messages` 存在时遮住 `input`。
  单个转义长字符串仍需反序列化器的复用解码缓冲，不能宣传零分配；无效 JSON 必须整包失败，不能留下部分统计。
- **普通桌面长测夹具要分开计标题请求**：续接后 `SessionState::turns` 从 0 起，第一轮正常结束仍会发 `purpose=title`。
  模拟端点给标题单独回文字，主会话的工具循环按 `purpose=turn` 核对；轮询会话只读初始日志偏移之后的追加段。
  Node 原地覆写控制文件会和 PowerShell 的 `ReadAllText` 争用，实测报 `EBUSY`；改成临时文件 + 改名，并限次等待自己的短暂读锁；读方遇到短暂 I/O 争用保留上一相位重试。
  控制进程先 `SetProcessDPIAware` 再取客户区：本机目标是 144 DPI、1920×1230 物理像素，不声明 DPI 时坐标被虚拟化，会点错输入区 / 发送键。
  隐藏窗口每 250ms 才强制画一帧，聚焦后先等输入处理器登记；发送用窗口内的按钮，时间从鼠标按下前算，不依赖前台修饰键状态。
  `history::rebuild` 有 20 万字符上限，往日志塞 12 MiB 文本实际会被裁掉，不能当长请求；桌面样本改用 3 张有效 PNG 历史附件，端点断言每次主请求 ≥10 MiB。
  `desktop-ab-01` 未通过夹具验收且请求偏小，仅留诊断；`desktop-ab-02` 才使用大图片请求。普通隐藏 D3D 窗口的 PrintWindow 曾返回全黑，不能冒充视觉验收。
  `kernel-ab-01` 的 4320 次请求逐字节一致，但 2/36 组延迟超标；`desktop-ab-02` 的 2268 次主请求与 36 次标题成功，但 8/18 组延迟门槛未过，不能算验收通过。
  后续 `gateway-bench-observer.mjs` 用 3 个可转移的 32 MiB 缓冲循环接收，JSON / SHA 校验不再挡住固定 40ms 模拟响应；每组重置校验 worker，产品退出后才清控制进程的垃圾。
  `kernel-ab-02` 的 4320 次请求字节与统计一致，长历史各组峰值均降，但 6/36 组延迟门槛未过；`desktop-ab-03` 在第 8 个进程停于日志 / 接收序列的消息数校验，不能算完整结果。
  网关在线程结束时追加日志，会和接收顺序不同；夹具按 `at_ms` + `req_id` 排序后，`order-smoke-01` 的 Anthropic 关原文场景 126 次主请求统计和字节一致。
  `kernel-aa-01` 用同一个基线 exe 连跑两组，各 60 轮 / 180 请求：发送 p95 127 → 121ms，整轮 p95 相差 -52.378ms，仅证明本机存在波动，不能豁免 A/B 的 20ms 门槛。
  `desktop-ab-04` 完整三轮已结束：36 进程、2268 次主请求（另有 36 次标题）、1134 份 gzip 请求 / 响应校验，字节和统计全一致；18 对中仍有 8 对延迟超标，发送 p95 差最坏 +28ms、整轮 +74ms，不能宣称性能验收通过。
  保存原文开启时，普通桌面 PB 峰值三轮中位数：OpenAI 232.391 → 224.762 MiB（两中位差 7.629）、Anthropic 233.859 → 223.980（差 9.879）、Responses 232.430 → 224.500（差 7.930）。关原文时有一对 OpenAI 峰值反增 0.746 MiB，不能说每轮都省。
  内核 `kernel-ab-02` 长历史保存原文开启时，PB 峰值中位 75.168 → 63.133 / 83.652 → 68.621 / 73.734 → 61.906 MiB（三协议同上），不当桌面总占用；两批不能直接相加。
  普通候选构建 SHA256 `95bebf178c36ed498911119fe986b30529fe76d98726734bff2a15d40cc62fdb`，归档于 `D:/t3/gateway-memory/candidate/claudesktop.exe`。
  该构建的 workspace release 632 项通过、12 项忽略，fmt、all-targets release Clippy（-D warnings）和普通构建通过；4 次请求的普通隐藏窗口快查也通过图片 / 签名回传与正常退出。
  产品解析代码在这些批次之间未改。随后工作区出现独立 UI 改动，以上构建 / 回归只覆盖保存的网关候选快照；尾延迟回归尚未排除，当前仅为待性能验收候选。
  完整报告：`C:/Users/asus/.answer-me-with-html/pages/claudesktop-gateway-20261116-100334.html`。待办保留性能验收，不能把基准脚本的 `passed: false` 改成通过。
- **网关尾延迟分段诊断（2026-11-16）**：`RequestLog.total_ms` 从读取请求前算，`at_ms` 却在请求体读完后记录，不能直接相加推响应结束；内核诊断按原始 jsonl 追加顺序对齐 `gateway_writes`，完成时刻减写入耗时才是响应后的收尾起点。
  `gateway::request_stats::tests::compare_parse_latency` 是忽略式测试，读取 `CLAUDESKTOP_GATEWAY_PARSE_BENCH` 指定的 gzip 请求，报告写到 `CLAUDESKTOP_GATEWAY_PARSE_REPORT`。
  `parse-alternating-01` 在同一进程内逐次交替旧 / 新解析，各预热 20 对后量 100 对，三协议真实请求加一个 12 MiB base64 字符串样本共 400 对；解析 p95（旧 → 新）为 12.845 → 8.399 / 15.323 → 9.127 / 13.280 → 8.603 / 4.855 → 2.440ms。这不能代替桌面端到端验收。
  `latency-trace-kernel-01` 的 Anthropic 长历史关原文场景各 60 轮，360 次主请求字节 / 统计一致，发送 p95 却多 42ms，整轮多 7.918ms；端点读取请求体的最大耗时为旧 249 / 新 195ms，新增接收开始、响应发出时刻用于区分传输和模拟响应等待。
  `latency-receiver-events-01` 用同一基线改为事件读取，接收尾延迟仍在，不能认定异步迭代器是原因；工具默认仍用原来的 `iterator`，`--receiver=events` 仅供诊断。
  桌面 `--observer=idle` 在每轮完整结束后才校验 JSON / SHA，校验完终止 worker，再开始下一轮，最多保留 4 个接收缓冲（含预热标题）；产品计时不包含校验，全部请求仍逐条验证。
  `latency-idle-desktop-01` 三轮 Anthropic 开原文对照共 378 次主请求、6 次标题，189 次成对字节和 378 份保存原文请求 / 响应校验均一致，但第一对整轮 p95 仍多 49ms，其余两对 -46 / +13ms；该批仍未通过，不能把问题全归校验线程。
  该批响应发出到 UI 记录完成的分组 p95 为 2～4ms、全样本最大 5ms，异常主要出现在相邻请求的传输阶段。内核的 idle 校验握手只编进 Windows 测试，普通 exe 不含新入口。
  `latency-aa-desktop-01` 用完全相同的普通基线 exe 交替三轮，378 次主请求、6 次标题、189 次成对字节和 378 份原文请求 / 响应校验通过，但两对发送 p95 差 +65 / +90ms、整轮 +43 / +32ms，另一对 -23 / -2ms；说明当前测量自身波动已超过门槛，不能据此认定所有 A/B 超标都是噪声，也不能豁免验收。
  `latency-affinity-aa-01` 再把测试控制进程固定到逻辑 0～1、产品固定到 4～11（`cpu-topology.json` 确认均为性能核；仅这些测试 PID），同一基线三轮仍有两对发送 p95 差 +47 / +36ms；不是核心分配的修复证据。`--control-affinity` / `--product-affinity` 仅作为诊断选项，不改默认分配、不改优先级或电源策略。
  两批 A/A 与 idle A/B 的每个窗口都隐藏且不在前台，三批共 1134 次主请求、18 次标题；问题尚未缩小到可确认的产品侧根因，验收仍卡在本地大请求传输的可重复性。不要根据单项 p95 相加算整轮 p95，也不要把 A/A 的任何内存差称为优化收益。
  新诊断入口通过 builtin release 单测 191 项（6 项忽略）、该包 all-targets release Clippy（-D warnings）、fmt、JS 和 PowerShell 语法检查。`latency-tool-smoke-01` 用新测试构建实际验证三协议的 idle 握手与事件接收，180 次请求的统计和 gzip 原文一致；单版本快查没有 A/B 配对，不算性能验收通过。
  普通候选 exe 仍是已归档的 `95bebf178c36ed498911119fe986b30529fe76d98726734bff2a15d40cc62fdb`，本轮只加测试诊断与记录，没有新的产品逻辑修复，也没有更新 dist。
  系统级跟踪脚本是 `.claudesktop/bench/gateway-memory/trace-latency.ps1`，配置是 `gateway-latency.wprp`；PowerShell 语法和 WPR 配置校验通过。脚本跟踪一次本地隐藏基线场景，使用独立实例名，只停止自己的实例；本次管理员级系统跟踪已获用户明确授权。
  **WPR 内置 CPU.light + Network.light 的缓冲不轻**：本机配置共约 960 MiB；准备的自定义配置改为系统 64 MiB + TCP 16 MiB，只保留调度 / 采样 / TCP 事件。80 MiB 是配置缓冲，不是实测总开销，跟踪期间的数据不能用于性能门槛验收。
  本轮诊断报告：`C:/Users/asus/.answer-me-with-html/pages/claudesktop-gateway-20261116-114944.html`；原性能验收仍未通过，候选版状态不变。
  用户随后通过结构化选项明确授权本次管理员级系统跟踪，已运行准备的脚本，目标目录 `latency-etw-01`；后续状态以该目录和工具任务完成结果为准，授权不需重复询问。
  `latency-etw-01` 的 80 MiB 内存环形记录覆盖了主要请求时段：虽然 EventsLost 为 0，完整内核事件仅留最后约十几秒，不能用来判断产品线程行为。已改 `-filemode`，配置增加 WPR 的 `NetworkTrace`（不是 `NetworkTCPIP`）；`latency-etw-02` 572 MiB ETL 完整覆盖 63 次请求，7,490,708 条事件，丢失 0；两次均正常停止，`wpr -status` 无运行记录。
  ETL 解码用微软 NuGet `Microsoft.Diagnostics.Tracing.TraceEvent 4.0.0`，DLL 签名 Valid；依赖 `System.Runtime.CompilerServices.Unsafe 6.1.2`（程序集 6.0.3.0，独立 exe.config 做 bindingRedirect），都在 bench 的 `traceevent-run/`，没有加入产品依赖。
  D 盘目录会被 .NET Framework 判为网络来源，Unblock-File 不解决；仅独立分析 exe 的 config 允许 loadFromRemoteSources。原自写 OpenTrace 读取只留诊断，最终用微软 Reader 解码，`decoded-events.jsonl` 含产品 1948、控制端 36624、驱动 8832 三个 PID。
  `timing-analysis.json` 对齐 TCP 与调度：慢请求上传 49ms，其中网关线程阻塞约 43.265ms、运行 4.103ms、就绪等待 1.503ms；控制端阻塞 32.740ms、运行 15.840ms、就绪 0.273ms，收发同时出现约 17.3～17.6ms 空档。栈在恢复运行时对齐，确认网关 `ws2_32!send`（+349）→ `ZwWaitForSingleObject`（+20），控制端 `GetQueuedCompletionStatusEx`（+207）→ `ZwRemoveIoCompletionEx`（+20）；不是解析或 CPU 排队证据。
  `write-buffer-ab-01` 用同一个 Windows 测试构建临时对照默认 128 KiB 与 16 KiB 发送批，360 次请求逐字节 / 统计一致，但 16 KiB 的三轮整轮 p95 分别多 42.555 / 75.784 / 73.395ms，CPU 各多 1250 / 1546.875 / 1250ms；假设未成立，实验环境变量入口与 CLI 参数已移除，生产传输未改。原实验 exe 和结果保留复核。
  本机 Node v22.22.1 的 HTTP 请求与 socket 读取水位实测均为 16 KiB，`createServer({ highWaterMark: 1024 * 1024 })` 可改为 1 MiB；两套工具加 `--high-water-mark` 用来单独诊断接收端背压，默认仍沿用原值，不改变产品配置。单版本快查的总 `passed` 改为 null，避免空配对数组被 `.every` 当成性能通过，功能校验照常失败即退出。
  `endpoint-hwm-aa-01` 两边仍是同一个内核基线，接收水位 1 MiB，360 次请求字节 / 统计一致；三对发送 p95 差 -15 / -10 / -5ms、整轮 -1.822 / -18.127 / -24.543ms。前两对因长历史峰值未下降总门槛为 false（A/A 没有优化，不可解释成内存收益）；不能仅凭这批认定提高水位已消除抖动。
  `endpoint-hwm-desktop-aa-01` 同一个普通桌面基线、idle 校验、接收水位 1 MiB，378 次主请求与 6 次标题、189 次成对字节、378 份 gzip 请求 / 响应校验通过；三对发送 p95 差 -11 / 0 / -3ms、整轮 +20 / -10 / +1ms，原 20ms 门槛下三对均过。其峰值差为负是 A/A 波动，不是优化收益。
  `desktop-controlled-ab-01` 已完整结束，仍是归档普通 exe，三协议 × 原文 on/off × 三轮，36 进程、2268 次主请求 + 36 次标题；1134 次成对请求逐字节校验、2268 次统计和 1134 组 gzip 请求 / 响应校验均通过，每次主请求 ≥10 MiB、每个窗口隐藏且未到前台。原 20ms 门槛下 17/18 对通过，发送 p95 差最坏 +19ms，唯一未过是第一轮 Anthropic 开原文的整轮 p95 388 → 417ms（+29ms），总 `passed` 仍为 false。
  该批开原文时 PB 峰值三轮中位数：OpenAI 230.391 → 223.543 MiB（两中位差 6.848）、Anthropic 233.078 → 224.023（差 9.055）、Responses 232.906 → 225.352（差 7.555）；18 对峰值均下降，最少省 1.289 MiB。CPU 不能宣称全面下降，尤其 Responses 关原文的中位由 3078.125 → 3765.625ms。
  唯一失败组的逐轮时序保存在各自 `turn-analysis.json`，选择出 p95 样本的分解是 `remaining-failure.json`；它只解释这次 29ms 差由各阶段如何组成，不能相加各阶段 p95，也不能据此确定原因。该批未采 ETW，不可直接把先前基线的 Winsock 等待认作本次剩余超标的全部原因。
  目前没有新的产品逻辑修复；发送批实验已清除，`gateway.rs` SHA256 与此前验收候选仍完全一致。诊断工具的 JS / PowerShell 语法复查和普通归档二进制哈希核对通过，系统跟踪已全部停止。原完整内核 / 桌面性能失败记录继续保留；未因剩余 9ms 放宽门槛，也未无理由重跑同一批碰运气，性能验收仍待完成。
- **Anthropic 剩余延迟的独立分段诊断与接收缓冲修复（验收中）**：用户再次要求继续。副本在 `.claudesktop/bench/gateway-memory/profile-build-v3/`，UI / core 取归档 HEAD，覆盖已验收的请求共享源码；两边只在网关旧 / 新解析上不同，都加相同 `request_profile` 计时。计时覆盖 submit、prompt_prepare、history_snapshot、step_prepare、json_build、provider_send / response、gateway_receive / roundtrip、gzip_save、statistics_parse、log_lock / write。
  此模块只存在独立副本，不进入主工作区产品源码；二进制名 `claudesktop-gateway-profile`，不会替换普通 `claudesktop.exe`。CLI `--profile=1` 才为该诊断 exe 指定记录路径，工具要求每次主请求有完成的 gateway_write 记录。分析器是 `.claudesktop/bench/gateway-memory/analyze-request-profile.mjs`；这些带打点的耗时只能定位，不能替代普通 exe 的性能验收。
  准备脚本 `prepare-profile-build.mjs` 用 GNU tar 时必须转 `/d/...` 路径并带 `--force-local`；对 git archive 取出的文本先统一 CRLF 再按完整片段替换，替换数量一律断言，测试模块不改。桌面还要带上 `tools/markdown-sample.md`（`include_str!` 编译依赖，不能只归档 apps / crates）。早期 v1 / v2 是未完成副本，最终仅用 v3。
  两份预先建好的源码副本共享 target 时，曾出现 Cargo 对第二份只报 Fresh、产出的候选与基线 SHA256 完全相同；已在构建候选前刷新该副本主入口 / lib / gateway / profile 的修改时间，要求实际重编。最终仍必须核对 exe 哈希与运行记录中的 variant，不能把同一 exe 误当 A/B。
  `profile-desktop-01` 与 `profile-io-smoke-01` 没有通过夹具验收：该诊断 exe 对 `C:/WINDOWS/TEMP/...` 的原文和网关日志创建返回 Windows 错误 5（PermissionDenied），会话也未追加；请求虽已跑完，不能用这些耗时作结论。没有改系统 ACL。
  诊断专用 `--data-in-output=1` 把应用数据放在本次 bench 输出的 runtime-data 下，工作目录仍是同一个仓库外临时 cwd，避免注入真实项目说明；只允许与 `--profile=1` 一起使用，不改普通验收默认路径。
  `profile-data-smoke-01` 成功：63 次主请求、1 次标题，统计 / gzip 原文 / 完整分段记录通过；在该快查中提示词准备 p95 1.467ms、历史快照很小，JSON 构建三次合计 p95 51.777ms，本地网关读取合计 p95 72.011ms。它们是独立分位值，不能相加解释一轮总耗时；带打点且数据盘位置不同，只能用于定位。
  三轮分段对照 `profile-desktop-02` 已完成，使用 `.claudesktop/bench/gateway-memory/profile-io-baseline.exe` / `profile-io-candidate.exe`，378 次主请求、6 次标题、189 次成对字节与 378 组原文校验通过；带打点的三对整轮 p95 差 -17 / +5 / -62ms。候选统计解析三次合计 p95 10.491 / 15.522 / 12.050ms，基线 18.407 / 24.076 / 22.322ms；gzip 相近、传输波动较大。诊断通过不能覆盖普通 exe 的 29ms 失败。
  `profile-local-buffer-ab-01` 同一候选解析，只把 Anthropic 发往本机网关的 ureq 输出缓冲由 128 KiB 改成 1 MiB：378 次请求 / 6 次标题内容和统计一致，但三对 PB 峰值多 1.930 / 1.844 / 1.422 MiB，整轮 p95 差 +49 / +47 / -32ms，不合入；副本里的该行已撤销。
  独立 WinSock 读取确认本机 TCP 默认 SO_SNDBUF / SO_RCVBUF 均 65536 字节（`socket-defaults.rs`）。`profile-receive-buffer-ab-01` 只把已接受连接的 SO_RCVBUF 设成 1 MiB，两边均为按需解析：378 次主请求、6 次标题、189 次成对字节、378 组 gzip 校验通过，三对整轮 p95 为 384 → 353 / 420 → 360 / 416 → 389ms，均过原门槛。
  第一跳网关读取三次合计 p95 为 80.404 → 23.834 / 80.976 → 31.565 / 83.403 → 35.346ms；发送 p95 差 -20 / -12 / 0ms，CPU 各少 218.75 / 265.625 / 531.25ms。PB 差分别省 0.855 / 0.910 / -0.145 MiB，只是单因素诊断，不能充作普通桌面验收或称零额外内存。
  产品现在由 `gateway/socket.rs` 为 Windows 本地已接受连接设同样的接收缓冲，设置失败保留默认；未改变 provider 输出缓冲、外部服务器连接或依赖。系统 TCP 缓冲上限每活动连接比本机默认多约 960 KiB，不完全包含在进程 PB / pool 计数中。
  新修复已通过 builtin 包 release 292 项测试（11 项忽略）、all-targets release Clippy（-D warnings）、fmt 及 JS / PowerShell 语法检查；其中真实 loopback 的 getsockopt 测试确认接受连接实际至少 1 MiB。普通无打点候选从独立快照 `release-receive-fix/` 构建成功，归档 UI / core 避免混入并行界面工作；构建当时的 builtin 全部源码与工作区逐文件一致。后续并行工作继续改动了内核和 ACP，以上验收只覆盖冻结快照，不代表后续全部工作区改动。
  普通候选 SHA256 `cb521af354ea01c5dbdddae0b1393b54632e59e337e99416736e5ace81e99feb`，确认不含诊断环境变量标记。`receive-fixed-desktop-targeted-01` 的候选从项目 bench 下的自定义文件名启动，已完成预热三次请求却无日志 / 会话追加，夹具超时，不能作为性能数据。
  将同一字节产物复制到常规 `D:/t3/gateway-memory/receive-fixed/claudesktop.exe` 后，`receive-fixed-normal-name-smoke-01` 在原 C 盘临时数据目录成功：63 次主请求、1 次标题、统计 / gzip 原文完整、0 条诊断记录，始终隐藏。未修改 ACL 或搬数据盘；只能确认 exe 位置或文件名影响了行为，不能声称已确定具体安全机制。新三轮 A/B 继续使用常规产物位置，旧普通失败保留。
  `receive-fixed-desktop-targeted-02` 的普通无打点 Anthropic 开原文三轮全部通过：378 次主请求、6 次标题、189 次成对字节、378 组原文请求 / 响应校验一致。三对发送 p95 差 -17 / -4 / -44ms，整轮 p95 差 -7 / -82 / -62ms，PB 峰值分别省 7.355 / 9.242 / 7.680 MiB；沿用原 C 盘临时数据目录和 20ms 门槛。`receive-fixed-desktop-full-01` 全三协议 × 原文 on/off × 三轮结束：2268 次主请求、36 次标题、1134 次成对字节与 1134 组 gzip 校验均通过，但仅 16/18 对达标，Responses 第 2 轮和 OpenAI 第 3 轮开原文整轮 p95 差 +43 / +42ms。Anthropic 六对均过，全部发送 p95 差 ≤20ms；原文关闭有两对 PB 峰值增加 2.711 / 5.176 MiB，不能说每对都省。
  开原文 PB 峰值三轮中位：OpenAI 232.191 → 224.289、Anthropic 233.199 → 223.977、Responses 233.453 → 224.047 MiB；CPU 并非全降，OpenAI 开原文中位多 78.125ms。新增进程非分页池计数中位约 422 KiB，不能据此推断系统 TCP 的全部内存成本。
  `remaining-failures.json` 按各组真实 p95 对应回合拆分：Responses 候选在首个网关之前少 22ms、三次上游上传合计多 62ms；OpenAI 对应为少 35ms、多 91ms。两组额外时延主要落在网关 → 模拟端点的上传，不能相加各阶段独立 p95。
  独立原生测试探针证实 Node 已接受的 socket 内核 SO_RCVBUF 仍为 65536，`highWaterMark=1 MiB` 并没有改它。测试专用 `tcp-buffer-helper.cc` / `build-tcp-buffer-helper.cmd` 放在本节 bench 目录，无产品依赖；用 Node-API + libuv 按本进程两个回环端口找唯一连接再设置 / 读取缓冲。不要把 Node `_externalStream` 当 libuv handle，它实际不能传给 uv_fileno。
  两个基准工具新增显式 `--receive-buffer=1048576 --tcp-buffer-helper=.claudesktop/bench/gateway-memory/tcp-buffer-helper.node`，默认不改；每次请求记录 `socket_receive_buffer.before/after`，summary 标记端点配置。真实回环快查确认 65536 → 1048576。`endpoint-rcvbuf-desktop-aa-01` 同一普通基线的双协议 × 三轮 A/A 完成：756 次主请求、12 次标题、378 次成对字节与 756 组 gzip 校验一致，但只有 5/6 对达标；Responses 第 2 轮发送 / 整轮 p95 仍差 +38 / +48ms。这个实验没有证明测量已稳定，工具默认仍不设置端点内核缓冲，也不能据它豁免旧 A/B 的失败。
  交付快照初次包测试因缺 `tools/mock-mcp-server.mjs` 导致 2 项 MCP 集成失败；补齐同一夹具后只重跑这 2 项和未执行的 MCP / truncation / doc，全部通过，快照累计 292 项成功。原失败和补跑日志分别为 `receive-fix-snapshot-tests.log` / `receive-fix-snapshot-retry.log`。
  内核对照另建 `release-kernel-baseline/`，从交付快照复制，只换回 `gateway-baseline.rs`（旧 Value 统计、系统默认接收缓冲）；两边完全相同的 `request_memory.rs` 支持 idle 握手。基线测试 exe SHA256 `e9d3048ce9889c29de591522760b1596cea71b09f9ee539a97d41feb147c4172`，候选快照测试 exe 为 `172ba8f9149edd1b9450214fedb08bac49758728dfc0c85740e13956dc028dba`，均归档在本节 bench 目录。
  `receive-fixed-kernel-full-01` 三协议 × 短 / 长历史 × 原文 on/off × 三轮已结束：4320 次主请求、2160 次成对字节和 2160 组 gzip 校验全部一致，35/36 对达标，唯一失败为 OpenAI 第 1 轮长历史开原文整轮 p95 多 26.5492ms（发送多 6ms）。长历史每对 PB 峰值均降，最少省 11.457 MiB；开原文 PB 峰值中位为 OpenAI 75.406 → 63.270、Anthropic 83.676 → 68.766、Responses 73.746 → 62.027 MiB，只代表内核测试进程。
  本批仍是原端点内核缓冲、读取水位 1 MiB、idle 校验；不豁免超标。`remaining-failure.json` 按真实 p95 回合对齐：候选首个网关前少 8ms，上游三次上传多 76ms，候选服务器等待反而少 12ms；不能把失败直接归于统计解析。
  为分离变量，新建 `release-parse-only-baseline/`：两边都固定产品的 1 MiB 网关接收缓冲，仅恢复基线的旧 Value 统计。普通基线为 `D:/t3/gateway-memory/parse-only-baseline/claudesktop.exe`（SHA256 `911f9c941849fa4aaeddff1d834d696e1bc71c3fd1fb72b01b16622d76c2a2f4`），配套测试 exe 为 `parse-only-baseline-kernel.exe`（SHA256 `5b1fdf0b296e10775fa6d903a4c92f3b90d9c902c01492ef2e8fc73cc88662be`）；两边测试入口完全相同，基线真实 socket 单测也通过。
  `endpoint-timer-probe.json` 同进程各量 80 次 `setTimeout(40)`：默认 p95 47.790ms、最大 54.930ms；只给 Node 控制进程 `timeBeginPeriod(1)` 后 p95 40.985ms、最大 42.961ms。原生测试助手的环境清理钩子调用 timeEndPeriod，未修改产品进程或系统设置；新 CLI `--precise-timers=1` 显式启用，summary 记录 `controller_timer_ms`。
  `fixed-clock-desktop-aa-01` 同一普通候选、两跳接收缓冲均 1 MiB、控制端精确定时，双协议 × 三轮 A/A 完成：756 次主请求、12 次标题、378 次成对字节与 756 组 gzip 校验一致，仍只有 5/6 对达标。OpenAI 第 2 轮整轮 p95 273 → 302ms（+29ms，发送反少 5ms）；`failure-analysis.json` 对齐真实 p95 回合，上传合计多 34ms、其余合计少 5ms，不能再把剩余超标归于未固定的第一跳或定时精度。
  进一步用独立 std::net HTTP 客户端隔离 WinSock SO_SNDBUF（`socket-send-bench.rs` + `socket-send-probe.mjs`，构建用同目录 cmd）：同一进程交替默认 64 KiB / 1 MiB，128 KiB 输出批，固定 1 MiB 接收缓冲和 1ms 控制时钟，5 对预热后量 50 对。110 次每次 12 MiB 传输全部 SHA256 一致，但发送 p95 28.246 → 33.246ms、完整往返 p95 76.551 → 86.737ms、端点读取 p95 28.989 → 39.680ms；最大接收空档两边都约 22ms。该假设未成立，没有修改产品发送缓冲或引入新连接层 / 依赖。
  当前仍无法在本机证明 20ms 门槛下可重复通过：旧组合桌面 16/18、内核 35/36，最新同版本 A/A 5/6。新的只隔离解析的对照构建已完成但没有在 A/A 未稳定时重跑整套碰运气。下一步需要在独立、空闲 Windows 环境先验 A/A，再运行同样的完整 A/B；本机功能回归、内存收益和失败数据已齐，性能状态保持未通过，未提交或发布。
- **独立网关验收包（2026-11-16，待远端执行）**：本机未检测到可调用的 Hyper-V / VMware / VirtualBox / Sandbox 管理器，同仓库 GitHub 自托管 runner 为 0；现有 CI 为 GitHub Windows 托管运行器。未安装虚拟机或改系统设置。
  `tools/gateway-validation.mjs pack` 把已核对的三组桌面 / 内核二进制、Node v22.22.1、工具、原生探针和许可证直接流式压入 ZIP，不复制整套源码或重编；每个条目解压流重新核对 SHA256。PowerShell 5.1 的 `@(... | ConvertFrom-Json)` 曾把顶层数组包成一个元素，不能对它再枚举属性；修为直接赋解析结果。
  冻结包 `.claudesktop/bench/gateway-validation-kit-20261116-01.zip` 为 98135781 字节、22 个 ZIP 条目（manifest 包含 20 个受校验文件），SHA256 `54f7e9e4fe51345757733592090a991b9e2a8a6a0be459ae8268f3478cd68622`。manifest SHA256 `d0eaf0ddca1279e4c7ab28d08af33ac25e50f239dc0d7aabbb6367a3b60b52b2`，不含原用户配置 / key / 真实会话；Node 官方许可证从 nodejs/node 的 v22.22.1 精确版本取得。
  包内 `run.cmd verify` 逐文件核对，`smoke --scope=all` 只做功能快查；`run --scope=all --set=combined --condition=reference` 用 Windows UUID + 主机名的哈希防止原机误算独立环境。先跑三轮三协议 A/A，任何绝对发送 / 整轮 p95 差 >20ms 就不启动 A/B；只有完整原组合、原端点条件、两范围全过才 `full_combined_passed=true`。parser / calibrated 必须显式选择且单独标记。
  两个原基准新增 `--comparison=aa`，先要求二进制 SHA256 相同，再看延迟绝对差，不套内存收益条件；A/B 保留单向额外等待 20ms、峰值比 ≤1.1、内核长历史 PB 峰值必降。旧 54 对 A/B 判定逐项回放完全一致；最新旧 A/A 用严格绝对差会拒绝 4/6 对（原 5/6 是单向门槛），不得改写原始结果。
  包损坏注入可在启动前拒绝，恢复后完整性通过；原机 `run` 拒绝且 `ab_started=false`、0 子任务、性能 false。实际从 ZIP 解包到系统临时目录后，三组桌面 / 内核各三协议共 18 进程，1107 主请求 + 9 标题，1107 组 gzip 请求 / 响应和统计均过；桌面隐藏且未到前台，`performance_passed=null`。新 A/A CLI 的短历史 120 请求真实快查也过，但因矩阵不完整被总验收器拒绝，不能充完整校准。
  receipts 在 `.claudesktop/bench/gateway-validation-input/{package-validation,guard-tests}.json`，详细 smoke 结果在这次解包目录（`extracted-01.json` 记录位置）。D 盘空间仍紧：失败发送缓冲原型已无损压缩为 `profile-local-buffer.exe.gz`，原 SHA256 验过再删原文件；没有清理别人的构建。
  手动流程 `.github/workflows/gateway-validation.yml` 从同仓库 Release 资产 ID 下载精确 SHA256 的包，只读权限，不 checkout / 不重编，失败也上传结果；现有 `ci.yml` 仅在手动传 `gateway_asset_id` 时调用它，正常 push / PR 保持原检查。YAML、PowerShell 语法、路由结构和 actionlint v1.7.12 均通过（下载包按同一次 Release 的 checksums 核对）。具体待授权动作在 `.claudesktop/bench/gateway-validation-input/remote-actions.json`：只提交两份工作流到临时分支、诊断 Release 草稿上传 93.59 MiB 包、手动运行原组合 / 原条件 all；当前本地 HEAD 与远端 master 同为 `7951eada`。用户随后通过结构化选项明确授权完整远端步骤。已用独立 Git index 创建并推送临时分支 `codex/gateway-validation-20261116`，提交 `46cedfd523e749d77a589e8844af92be9bf1252f` 只含两份工作流；当前工作区仍在 master、其他改动未入提交。固定包已上传到 `diagnostic-gateway-validation-20261116` 诊断 Release 草稿（release ID 447067582，草稿 URL 的 tag 显示为 untagged-635073dcae321ba33a92）；asset ID 754932863，GitHub 返回的 digest 与本地 SHA256 完全一致、大小 98135781。
  第一轮远端作业 `43874795094` 在下载时被 403 `Resource not accessible by integration` 拦住，0 性能测试；草稿资产的 GITHUB_TOKEN 需要 contents:write 才能读取。仅给诊断 job / 可复用流程补此权限，常规检查仍无新增写动作；不 checkout 仓库，token 只传下载步骤。补提交 `a2f84f9c459d297c35cf92b902890aaefe4e2730` 已推到同一临时分支，并开 `include-hidden-files` 保留 bench 目录的结果。
  第二轮作业 `43875031087` 的下载 / 文件校验成功，已在独立 GitHub Windows 运行器执行：内核三协议 × 三轮 A/A 的 1080 次主请求、540 次成对字节和 1080 组 gzip 校验全过，但 0/9 对延迟达标，最大绝对整轮 p95 差 150.7767ms，发送绝对差最大 41ms；总状态 calibration_failed，桌面和 A/B 均未启动。artifact 14230131799（29147991 字节）已成功上传，原始结果已下载到 `gateway-validation-input/remote-43875031087/`；本地总验收器重新核对矩阵、计数、二进制哈希与门槛，结果保存在 verification-receipt.json。运行器为 Windows Server 2025、4 逻辑 CPU 的 Hyper-V VM；不把共享托管 VM 的独立性当作测量稳定性。
  `timing-analysis.json` 对齐最大差异的实际 p95 回合：两边首个网关前相差 1ms，三次上传从合计 194 → 350ms（+156ms），服务器等待合计反少 10ms；其余约 +4ms，组成整轮 +150.7767ms。同一 exe 的上传波动仍大，不能当作候选回退或放行证据。
  第三轮 `43876139647` 用同一冻结包和同一临时工作流显式选择 calibrated（端点接收缓冲 1 MiB、控制端 1ms 时钟）：独立内核 A/A 9/9 对通过（1080 次主请求与 gzip），桌面 A/A 7/9 对通过（1134 次主请求 + 18 标题、567 次成对字节、1134 组 gzip）。桌面失败为 Anthropic 第 2 轮整轮差 -42ms、OpenAI 第 3 轮 -24ms，严格绝对差门槛拒绝；全流程 calibration_failed，A/B 未启动，原条件失败继续保留。
  该批 artifact 14230662760（30022522 字节）已生成。直接 `gh run download` 解包途中因 D 盘满而失败；仅清理本次不完整解包目录后，改为流式下载原始 `gateway-validation-input/remote-43876139647.zip`，按上传时 SHA256 `f82a671c1f71f21e12fd20e7a92a5c9f2bf6eb599b40e615273099e48446495d` 验整包，再只提取摘要和失败组的 gateway-logs 到同名目录。完整明细仍保存在原始 ZIP，不必展开数千 gzip 文件。原始 ZIP 已完整下载并验 SHA256，8 个关键文件提取后由本地总验收器复核通过：内核仍 9/9（最大绝对发送差 6ms、整轮差 16.2488ms）、桌面仍 7/9；receipt 和两组真实 p95 回合拆分分别是该目录的 verification-receipt.json / timing-analysis.json。桌面 Anthropic 的 42ms 差由首个网关前 10ms、上传合计 21ms、响应等待 5ms、请求间 6ms 组成；OpenAI 的 24ms 分散为 4 / 10 / 5 / 5ms，未发现足以支持再改产品传输参数的单一原因。第四轮 `43878228861` 仍用同一包，选择 kernel / combined / calibrated，已成功结束：内核 A/A 9/9、完整 A/B 36/36 全部通过，A/B 4320 次主请求 / 2160 组保存原文全部校验一致。作业摘要 performance_passed=true、full_combined_passed=false；单独内核与改变端点条件不替代桌面 / 原条件总验收。artifact 14231522885（65944117 字节、SHA256 `af5923e373a0b59eaff3e5e2f18a0a62c4dd7d29b6bda670ac4d7e93d16cbc06`）已流式下载并完整验哈希，归档 `gateway-validation-input/remote-43878228861.zip`；只提取四份分析 JSON 后，本地总验收器重新核对矩阵、二进制哈希、请求数与全部门槛，结果一致。为腾出本任务落盘空间，旧 `profile-candidate.exe` 已校验后无损压缩为 .gz，没有改冻结候选字节。
  最新独立内核的 A/A 最大绝对发送差 3ms、整轮差 19.9961ms（贴近 20ms 门槛，不据此宣称有大余量）；A/B 最差额外发送 p95 +3ms、额外整轮 p95 +16.3469ms、最高峰值比 0.990060。全部长历史配对峰值均降，最少省 7.082 MiB；开启原文的 PB 峰值三轮中位为 OpenAI 75.336 → 63.125（差 12.211）、Anthropic 82.805 → 61.211（差 21.594）、Responses 73.449 → 61.617（差 11.832）MiB，内核进程数据不能当桌面总占用，也不能与桌面数据相加。
  开原文长历史的 CPU 中位分别少 20.0% / 21.9% / 13.4%；短历史 Responses 开原文反多 46.875ms / 60 请求，不宣称所有场景 CPU 都降。候选 20 轮后的闲置 PB 增长最差 2.148 MiB，未单独设置新门槛。细项与复核 receipt 为 `gateway-validation-input/remote-43878228861/verification-receipt.json`。
  内核和固定条件的验收已完成；桌面 A/A、原条件的整体性能仍未通过，当前没有运行中的验证作业，不再按同一条件重跑碰运气。保留冻结候选、诊断 Release 草稿、临时工作流分支和所有失败记录，未合并到 master 或发布产品。
- **请求 A/B 的校验别堵住 HTTP 主线程**（2026-11-16）：大 JSON 的解析、排序和哈希放在独立 worker，
  否则回环接收计时会混进校验线程的 GC / 调度成本。每组 3 次发送的 p95 实际就是最大值，正式样本改为 20 次发送 / 60 次真实请求，
  另记请求构建完成时间以区分构建与传输；20ms 发送等待门槛不变。夹具 cwd 和数据目录放系统临时目录，
  别放仓库的 `.claudesktop/bench` 下，那里会读到真实项目说明和项目记忆；仅原始结果留在 bench，子进程代理环境单独清空。
  `request-memory/ab-01` 有一次回环连接超时，`ab-02` 的内存 / 内容校验通过但尾延迟门槛未过；`ab-03` 在旧实现也出现建连超时。
  服务端允许 keep-alive 后，`ab-04` 实测仍是每 60 次请求 60 次 TCP 连接，不能假定已复用；SSE 在结束标记返回，没有继续读到 EOF。
  `ab-04` 一对 Anthropic 长历史的发送 p95 多 24ms（构建 p95 少 47ms），整组未过；接收端改为复用大缓冲后复验 `ab-05`。
  前四批保留诊断，不能合并成已通过的数据。
- **请求历史共享 / 借用序列化验收通过**（2026-11-16）：`request-memory/ab-05`，三种协议 × 短 / 长历史 × 交替三轮，
  每组 20 次发送 / 60 次请求，共 2160 次真实回环请求、1080 次成对 JSON 内容校验，18 对场景全部达标。
  14.71 MiB 合成历史下，PB 峰值的三轮中位数：OpenAI 70.02 → 46.28 MiB（省 23.74）、Anthropic 87.75 → 44.73（省 43.02）、
  Responses 81.53 → 43.69（省 37.84）；长历史累计 CPU 中位分别下降约 39% / 50% / 43%，20 轮后的闲置 PB 增长最差 0.629 MiB。
  最差配对发送 p95 差为 +2ms（门槛 20ms）；长历史三种协议的最差差值为 -26 / -5 / -9ms，构建 p95 也均下降。
  短历史进程 CPU 含采样线程且按 15.625ms 跳变：Anthropic / Responses 中位多 46.875 / 31.25ms（每组 60 请求），不宣称短场景 CPU 都下降。
  基线为 `12033a9` 的请求实现加相同测试计时入口；计时仅编进 Windows 测试二进制，普通 exe 没有新增监控开销。数据仅代表内核测试进程，不能当桌面总占用。
  workspace release 627 项通过、12 项忽略；最终内核单测 187 项通过、5 项忽略；fmt、workspace all-targets release Clippy（-D warnings）通过。
  最终普通 release exe 的隐藏窗口验收 `request-memory/desktop-normal-03` 通过：4 次本地请求、工具图片与思考签名回传、正常退出，始终隐藏且未到前台。
  普通 exe 在 `D:/t3/release/claudesktop.exe`，SHA256 `a6e827d1178ef14e894375c348ff6e045d5ad61b6ba9adfde3a00571087d4733`；未改版本号。
- **自研历史休眠别只看 `TurnEnded`**（2026-11-14）：标题生成 / 记忆提炼在这之后仍读历史，靠 `history_activity`
  阻止搬空；事件一到就立刻 `hibernate_session` 可能什么都不做（不是状态机坏了）。竞态测试先完成一次休眠 +
  唤醒把这段活动放完，再等 `transitioning_sessions == 1`，才是真的 `Spilling`。Windows 原子重命名前先 `drop(File)`。
- **自研历史快照流式写盘**：按条借用历史，通过 64 KiB 缓冲写 v1 JSON。失败时只删除本次成功创建的临时文件；
  `create_new` 失败要保留原路径。`encode_write_ms` 合计编码和缓冲写出，`commit_ms` 含同步及改名。
  `.claudesktop/bench/snapshot-stream-01/normal-02/` 用普通 release 的隐藏窗口走定时双层休眠、无损继续和重启续接。夹具的不透明 PNG 经 `read_file` 重编码成 JPEG，按实际媒体类型验图，再比对唤醒前后的请求内容。`normal-01` 保留初次误判记录。
  v1 文件字节兼容、写入期间关闭 / 发送、读写失败恢复及 675 项 release 测试通过（13 项忽略），fmt、Clippy 和普通构建通过。
  `snapshot-stream-01/verified-write.json` 复核 50 / 200 会话各三对，双方均开内核休眠，界面回收关闭，732 份成对快照消息字节及 2250 对请求指纹相同。取首批落盘结束的 Windows 高水位减落盘前 PB，三轮中位 62.45 → ≤10.42、72.80 → ≤16.22 MiB。候选未刷新写盘前高水位，只能报上界。
  200 会话单文件写入 p95 中位 118.69 → 221.23ms，首批完成 433 → 377ms，其中一对慢 185ms。六对切回等 200ms 再发送，唤醒 p95 最高 4.71ms，发送额外等待 p95 最大 +8.42ms。单独看全流程峰值，50 会话有一对多 5.03 MiB。
- **快照流式读取试验**：`snapshot-stream-01/read-isolated-01` 在同一诊断 exe 中比较 `from_slice` 和 64 KiB `BufReader` + `from_reader`，内核和界面各三个样本，每个七对。24.85 MiB 多条快照的内核峰值增量中位 50.71 → 26.86 MiB，读取 p95 22.75 → 174.45ms。界面为 39.93 → 16.39 MiB、28.08 → 83.73ms。
  含 24 MiB 单字段时，`from_reader` 的字符串暂存缓冲使两层峰值都上升。84 次读取内容一致，10 种损坏或归属不符的输入两边均拒绝。50 会话各单对实际唤醒 p95 为内核 3.98 → 29.04ms、界面 6.18 → 6.54ms。产品保留整包读取。
  首次界面对照的采样器读 `phase.txt` 遇共享冲突退出。诊断夹具改用临时文件加改名，采样遇短暂 I/O 冲突时沿用上个阶段，`display-read-desktop-02` 通过。两层数据在 `verified-read.json`，读盘试验只编入诊断副本。
- **自研历史休眠的内核合成测试（2026-11-14，release，50 会话 × 约 2.25 MiB × 3 轮）**：仅测试进程的 PB
  中位数 117.3 → 2.0 MiB、WS 120.2 → 7.3 MiB，不能当桌面应用总占用。唤醒 p95 5.16ms；
  0.002ms 只是驻留后再次 wake，不是真实发送延迟。118.5 MiB 峰值每 5 会话才采一次，不能据此认定峰值门槛通过。
  复跑 `cargo test -p claudesktop-builtin hibernation_ab_memory --release -- --ignored --nocapture`；正式验收用下面的完整桌面场景。
- **完整桌面休眠 A/B（2026-11-15）**：`tools/hibernate-bench.mjs --mode=ab`，release + `headless-screenshot`，
  50 个真实内核完成会话 × 每个约 1.92 MiB 快照，含有效 PNG、完整工具结果和思考签名；保留界面正文，交替 3 轮。
  主进程 PB 中位数 366.53 → 236.50 MiB（省 130.04），WS 251.09 → 143.19 MiB（省 107.89）；A/B 无外部子进程，进程树相同。
  每轮分别达标：最差峰值比 1.00436（用 Windows 的 PeakPagefileUsage，非采样峰值），恢复 p95 ≤2.617ms，
  激活预恢复 200ms 后实际发送至本地服务收到请求的 p95 差值 ≤1.410ms，内核发送前处理 p95 差值 ≤0.047ms。
  外部采样目标间隔 100ms、实测最长间隔 263ms；稳态只读约 4KiB 控制数据，没有读取约 2MiB 的历史快照。
  PowerShell 采样用 `OpenProcess` 持句柄、`DateTimeOffset.ToUnixTimeMilliseconds()` 记时间，避免动态 Handle 空值和 UTC / 本地时间相差 8 小时。
  窗口隐藏、每 250ms 手动离屏重画，CPU 含控制与绘制开销，不代表正常应用空闲 CPU。结果在 `.claudesktop/bench/desktop-ab-01/`。
- **30 分钟混合验收（2026-11-15）**：`tools/hibernate-bench.mjs --mode=soak`，实际 1800.414 秒、60 轮、1280 次请求、
  700 次历史内容校验。读文件 / 单次授权写文件 / 提问 / 子任务 / 后台命令 / 停止流式 / 压缩 / 回退 / 新建关闭各 60 次，
  后台任务运行时确认不休眠，所有会话 ID 和权限模式保持不变，无失败、无重复 Started，正常退出无遗留快照。
  长测主进程 PB 峰值 439.19 MiB、恢复 p95 3.751ms；后台命令的进程树另计，不与无子进程的 A/B 峰值混比。
  原始结果在 `.claudesktop/bench/desktop-soak-01/`。通过后默认开启（仍保留当前 + 最近 2 个会话、闲置 10 分钟），
  可用 `CLAUDESKTOP_HIBERNATE=0` 关闭；`--mode=smoke --sessions=4 --activation=default` 验证不设开关时的真实路径。
  默认开启版本已重新通过 workspace 604 项测试、普通构建和 release 截图构建；默认路径混合快查也通过（31 次请求、17 次内容校验）。
- **测试用 `BuiltinConfig::new` 要隔离默认路径**：它会带上用户的 MCP / 说明 / 记忆 / 规则文件。测试显式设对应字段为 `None`，否则会连真实 MCP，Windows 上还可能因子进程占着临时工作目录导致清理报 os error 32。
- **每一步的用量当场报**：`run_steps` 里每次 `stream_chat` 拿到用量就 `Inner::report_usage`，
  `context_tokens` = 这一步的输入 + 输出（`Steps::Done` 不再带用量）。原来攒到轮末才报一次，
  表现是用量环跑着的时候不动、轮末跳一下。压缩和子会话报 `context = 0`，
  界面把 0 当「这次不更新上下文」（只并进本轮累计）。
- **自动模式改不了本程序自己的配置**（`%APPDATA%\claudesktop\mcp.json`、`settings.json`、插件里的 `plugin.json`）：
  被判成「agent 自己的配置」，写操作会被拦。要改就让用户自己贴，别反复重试。
- **REA 这类大目录 MCP 服务**（207 个工具）：默认 `tools/list` 一帧 3.1 MB（含 outputSchema），
  真正进请求的 inputSchema 约 66.7 万字节（≈17 万 token）+ 描述 9 万字节；
  注册时给 env 加 `REA_MCP_INPUT_SCHEMA_PROFILE=compact`、`REA_MCP_OUTPUT_SCHEMA_PROFILE=off`
  能降到 schema 8.6 万字节（≈2.1 万 token），帧降到 219 KB。会话不用时在「MCP」页关掉它。
- **REA + Ghidra 在 Windows 上**：Ghidra 只认 `12.1.x`（`GhidraReleasePolicy.js` 里写死
  `SUPPORTED_GHIDRA_VERSION=12.1.4`，装最新的 12.2 会被判 unsupported），要 JDK 21+（JDK 17 不够）。
  本机装在 `E:\DevTools\rea-engine\`（Ghidra 12.1.4 + Corretto 21），环境变量见
  `~/.rea-ghidra-env.ps1`。**REA 给 Ghidra 的默认堆只有 2G，分析 whoami.exe 这种小文件都会
  `OutOfMemoryError`**，必须设 `GHIDRA_HEADLESS_MAXMEM=4G`（`GhidraJavaLaunch.js` 读这个变量，
  兜底顺序 `GHIDRA_HEADLESS_MAXMEM` → `GHIDRA_MAXMEM` → `2G`）。
- 本机直连 GitHub release 只有 ~70 KB/s，下载走系统代理 `http://127.0.0.1:7890`（Clash）能到 3 MB/s；
  `curl.exe --proxy` 一把就完，`Invoke-WebRequest` 默认不吃系统代理。
- **REA + JADX（APK）**：JAR 是 `jadx-headless-mcp 0.7.1`（作者 1013503897，不是 skylot/jadx 本体），
  REA 源码里写死了地址和 sha256（`dist/android/JadxRelease.js`），本机在
  `E:\DevTools\rea-engine\jadx-mcp\`。它要一个带 `jdk.compiler` 的**完整 JDK**（`java --list-modules`
  里得有），Ghidra 那份 JDK 21 直接能复用。REA 不代下，也不给装 ADB/Apktool（那两个另外配）。
- **本机自测 APK 要自己组装**：`aapt2 link` 出来的 base.apk 没有 `classes.dex`，
  `zip/tar` 追加进 zip 的条目会让 `zipalign` 说 “Unable to open as zip archive”，
  要用 .NET 的 `System.IO.Compression.ZipFile` 以 `Update` 模式写进去，再 `zipalign` + `apksigner`。
  引用了 `R.string.*` 时要先 `aapt2 compile --dir res -o compiled.zip` 再 `aapt2 link -R compiled.zip --java gen`，
  否则 javac 找不到 R 类。
- **REA + Apktool**：REA 要的是 apktool **可执行启动器**（`REA_APKTOOL_COMMAND`，必须绝对路径），
  不是 JAR。自己写个 `.bat` 包一层 `java -jar` 就行——但**这个 .bat 里不能有中文**：
  cmd 按系统码页（GBK）读批处理，UTF-8 的中文注释会被拆成乱码命令执行（报 `'D' is not recognized`）。
  另外不要带 BOM。Windows 上 `.bat` 会被套一层 `cmd.exe` 跑，路径里不能有 `" % ^ ! & | < >`。
- **REA + ADB**：`REA_ADB_PATH` 同样要绝对路径；不给就在 PATH 里找。本机用 SDK 自带那份
  `E:\Android\Sdk\platform-tools\adb.exe`（37.0.1），只探测客户端版本，不碰设备。
- **输入框模型选择器的弹层内容要做成独立 entity**（2026-11）：`gpui-kit` 的 `Popover::content`
  是 `Fn`，每次父视图渲染都会重跑，但 `PopoverState` 自己 `notify` **不会**重建面板那棵树——
  悬停换说明卡时想只重绘弹层，必须把内容做成 `window.use_keyed_state` 里 `cx.new` 出来的 entity
  （`ModelPickerContent`），让它观察 `ChatApp` 并在自己的 `Render` 里读状态；否则只能通知整个
  `ChatApp`，会话区跟着重画。注意 `use_keyed_state` 返回的是 `Entity<Entity<T>>`（init 的返回值外面
  再包一层），要 `.read(cx).clone()` 取里面的 entity。
- **`ScrollHandle::scroll_to_item` 在首帧会被丢掉**：新弹层第一次渲染时还没有子元素的 bounds，
  active_item 被消费掉但滚不动。要连着请求几帧（`scroll_attempts`）直到 `offset().y != 0`
  （GPUI 向下滚是**负**的，别写 `> 0`），并补 `window.request_animation_frame()` 让 prepaint
  真的跑一次；`window.on_next_frame` 里的回调在无头截图路径下不一定触发，别只靠它。
  同理，滚动请求要作用在挂了 `track_scroll` 且带 `overflow_y_scroll` 的同一个元素上。
- **底部锚定的弹层，悬停区不能随内容伸缩**（2026-11 模型选择器）：说明卡高度随文案行数变化时，
  面板从顶边往下长，指针下的行会跟着位移、看起来「不断跳变」。给卡片定高（`h(CARD_H)` +
  `justify_center()`），没有说明的模型也留一行淡色占位，各行高度才一致；别用「有就显示、没有就
  不显示」的写法。
- **对话列表的行下标和记录下标是两套**（2026-11 滚动手感改造）：`Session::rows` 把很大的已完成回复
  拆成多个虚拟行之后，`list` / `measured` 用**行下标**，`layout` / `transcript` / `is_turn_end` 仍是
  **记录下标**；两者用 `Session::row_of(ix)` 换算。新代码碰列表下标前先想清楚在哪一套里：
  `reveal`、`pin_row`、闪烁（`with_flash`）、搜索结果跳转都传记录下标、内部换算；
  元素 id（`("agent", key)`、`("agent-msg", key)`）用行下标，否则拆开的几块 id 撞车。
- **重建行划分要自己记锚点**：`ListState::reset` 会清掉 `logical_scroll_top`（跟随中的用户会直接落回顶部）。
  先在旧 `rows` 里把视口顶的记录下标和 `offset_in_item` 记下来，重建后行都塞进去（`splice_with_hints`）
  再 `scroll_to` 回去；顺手 `wheel = None`、`pin = None`。`reset(1)` 之后**不要**再 `reset(n+1)` 再补行，
  会变成 2n+1 行——补行走 `sync_list` 里已有的新增分支。
- **拆 Markdown 的围栏状态**：判断「关闭围栏」必须看**读这一行之前**围栏是否已经打开，
  在 push 之后再置 `fence = None`；先置后 push 会把开围栏那一行当成关围栏，整段代码块被当成普通文本切。
  表格同理：空行要结束表格，否则空行后的一行表格会被当成「还在表里」而不补表头。
  拆块文本是复制出来的（`Rc<[SharedString]>`），`retained_breakdown` 里已经计入这份额外内存。
- **跟随判定不能只看 `is_scrolled_to_end()`**：列表有未测 / 估高行时它返回 `None`，一帧内判断不了。
  `sync_follow` 用距底像素 + 最近输入方向 + `is_tail_measured()`；向上任何一格就 `pause_following_tail`
  （流式追加会抢位置），向下回到 80px 内且尾行已测才重新 `FollowMode::Tail`，中间带保持原状。
  程序化定位（reveal、恢复位置）后要 `quiet_follow(700ms)`，否则会被自动吸附拉走。
- **多内核默认选择**（2026-11）：`settings_view/form.rs` 的 `form_settings()` 以前结尾写死 `s.kernel = None`，
  保存一次设置就把默认内核清空，`Settings::kernel()` 于是永远回落自研。改成保留原值（`keep_kernel_choice`）：
  所选 ACP 档案被停用 / 删掉时置空；默认是 `Builtin` 但自研已没配好、又有启用的 agent 时也置空，
  否则保存会被自研校验锁死。输入框内核菜单选完会写成默认（`choose_session_kernel`）。
  注意设置页「内核」里的自研选项（自动模式 / 运行参数 / 用量记录）**不能**再按 `f.kernel == Builtin` 判断，
  否则默认选成 agent 后整块消失——改看「自研是否已配置，或一家 agent 都没启用」。
- **原地换内核不能直接 `new_session`**：会把 `task` 重算、`worktree` 元信息丢掉（项目会话被降成任务）。
  `replace_session` 现在用 `new_session_of(..., sk)` 并回写 `task` / `worktree`；切到 ACP 仍保持懒启动（等首次发送）。
- **ACP 首轮 Prompt 前的 `AgentMessageChunk` 不是回复**（pi-acp 会在 `session/new` 后塞开机清单）：
  `AcpShared::prompted` 记发过 Prompt 的会话，之前的正文降级成 `KernelEvent::Notice`（淡色提示、不进日志）。
  pi-acp 的 `setTimeout(..., 0)` 可能晚于客户端首轮 Prompt（懒启动 autoprompt）：再用 `agent_name == "pi-acp"`
  + 该会话第一个正文块 + 以 `## Context` / `pi v` 开头兜一层。`session/load` 重放本来就被 `replaying` 丢弃；
  别按 `Retrying (attempt` 前缀硬拦正文（会误伤正常回复）。命令重复校验比较时 Windows 上要剥掉 `cmd /c` 前缀。
- **侧栏「按项目 / 按时间」与项目管理**（2026-11-26）：`Settings::sidebar_by_date` 降级成只读迁移字段，
  新字段 `sidebar_grouping`（None = 按项目），`normalize` 里折算完置空、不再写盘；单测覆盖迁移后序列化里没有旧键。
  路径比较一律走 `settings::same_project_path`（Windows 大小写 / 斜杠 / `\\?\`），否则同一个目录会在侧栏里出现两次。
  `sync_sidebar` 的 12 元组已满，`sidebar_projects` / `confirm_remove_project` 要单独 `.hash`（漏了侧栏不跟着重画）。
  `set_sidebar_grouping` 必须 `pub(crate)`：`app::sidebar` 不在 `settings_view` 的子树里，`pub(super)` 编译不过。
  移除项目要和「幽灵复现」一起处理：真会话归档 + 纯空会话按**令牌**删（按下标连删会错位），
  否则只删 `sidebar_projects` 里的路径，下一帧 `active_session_groups` 又把这一组带出来。
  项目组头的移除确认条只替换组头、保留会话行（整组消失会让侧栏跳一下），确认态顺手清掉 `renaming` / `confirm_delete`；
  窄侧栏一行放不下「移除 X 并归档 N 个会话？」，所以标题一行、后果（`归档 N 个会话`）和按钮排第二行，
  内边距跟组头一样（第一组 10、其余 16），确认时整块不上下跳。
  按时间分组不拆「任务」区，任务会话只能从「+ 新会话」（跟随当前会话的 task）或项目顶盖的「不选项目（任务）」进。
  截图钩子 `CLAUDESKTOP_OPEN=sidebar-project|sidebar-time`（`:flow` 走添加 / 移除项目的断言，`:pending` 摆出移除确认条）
  全程只动内存：先把 `settings_path` 摘掉再跑，否则换分组 / 登记项目 / 切 workspace 会真的改写用户的 `settings.json`。
  验收时顺手比一下 `%APPDATA%\claudesktop\settings.json` 的哈希，钩子跑完不能变。
