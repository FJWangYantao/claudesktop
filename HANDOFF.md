# HANDOFF：踩过的坑

接新任务前先读这一页；踩到新的坑就往对应小节加一行（是什么现象、为什么、怎么绕开）。
按主题分节，写完超过一屏就往下归档。

## 构建 / 测试

- **调试环境变量是 `CLAUDESKTOP_*`，不是 `CLAUDEDESKTOP_*`**：多拼一个 `DE` 会让截图、样本、启动计时等钩子全部无效，表现为窗口照常启动但没有截图或日志。先核对 `main.rs` 实际读取的名字，不要据此推断启动卡死。
- **markdown crate 在 vendor 里打过补丁（2026-11-14）**：GFM 自动链接只把 ASCII 空白当 URL 终点，
  全角标点（`，。（）`）被连字吞进 URL（`http://a.b/，好的` 整句成链接；`**http://a.b/**（注）` 星号露出）。
  改动在 `vendor/markdown/src/construct/gfm_autolink_literal.rs`（标 `claudesktop patch`，详见
  `vendor/README.md`），回归测试 `chat.rs` 的 `autolink_ends_at_fullwidth_punctuation`。
  坑：路径里的 ASCII 标点（`/` `?` `=` `&`）必须继续吞、只有非 ASCII 标点才收尾——第一版补丁按
  「是标点就收」写，URL 的路径被砍掉，被这个测试当场抓回来。
- 本机内存小，本地 `cargo test` / `cargo check` 会因页面文件太小（os error 1455）失败：改动直接提交，验证交给 CI。
- **v0.15.0 分发验收**：本地普通 release 已通过 604 项 workspace 测试、fmt 和 Clippy；打包核对 exe 版本 / x64、ZIP 内三份文件及两份 SHA256。
  隐藏运行实际分发 exe，走真实窗口消息新建 / 切换 / 发送，4 个会话、7 次回环模型请求已验证历史无损恢复、运行中回合不休眠、流取消、正常退出清快照和重启续接。
  快查仅把 idle 阈值缩到 2 秒，仍走生产的 30 秒回收定时器；默认 10 分钟另验。原始记录在 `.claudesktop/bench/release-local-07/`。
- **正式包也要用同一份 exe 验收**：`CLAUDESKTOP_HEADLESS=1` 隐藏窗口、每 250ms 离屏绘制；不用截图 feature 或文件控制入口。
  本机把 GUI 放到 `CreateDesktop` 新建桌面时，进程在 `main` 前以 `0xc0000142` 退出，不能拿它判断业务启动失败；
  验收走隐藏窗口、隔离数据目录和回环模拟端点，按自己创建的 PID 发送窗口消息，不切前台、不移动用户鼠标。
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
