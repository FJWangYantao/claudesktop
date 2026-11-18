# HANDOFF：踩过的坑

接新任务前先读这一页；踩到新的坑就往对应小节加一行（是什么现象、为什么、怎么绕开）。
按主题分节，写完超过一屏就往下归档。

## 构建 / 测试

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

## UI / gpui

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
- **压测只收尾自己启动的 PID**：Git Bash 的 `$!` 可能是包装进程，不要按 `claudesktop.exe` 名称整批杀进程。
  用 Node `spawn` 取得真实 PID 并等待退出（新休眠验收脚本已如此处理）；未退出的实例会占住 exe，导致下次链接失败。
- **压测数字本身有 ±0.5ms 漂移**（机器负载、热状态）：滚动 p50 同一基线不同时间跑出 2.56→3.94ms。
  对比优化前后要交替 A/B 连跑几次取中位，别拿两次单跑说事。
- **模型页压测后不一定回到顶部**：`CLAUDESKTOP_STRESS_VIEW=settings:4` 的合成滚轮落在右侧模型清单时，
  内层滚动区会先吃掉事件，往返后外层仍可能停在底部；截首屏要在左侧分类导航上滚回顶部，别把压测中的画面当成初始位置。
- **无前台且窗口被遮挡时，滚轮压测可能一直不出统计**：`on_next_frame` 等不到可见窗口的正常呈现，别把超时当通过，
  也别为了拿数字抢前台；功能验证可用 `WM_MOUSEWHEEL` 的 `PostMessage` 驱动真实窗口，再用 `shot.ps1` 截图。

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
