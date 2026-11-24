# vendor：打过补丁的上游 crate

经根 `Cargo.toml` 的 `[patch.crates-io]` 替换 crates.io 上的同名版本。工作区 `exclude` 了本目录，
`cargo test --workspace` / clippy 不会跑到这里。

## gpui-base 0.7.3

gpui-kit 0.7.3 的基础组件层（Markdown 的 `TextView`、表格、列表都在这里）。拷自 crates.io 发布包，
删了 `.cargo-ok` / `.cargo_vcs_info.json` / `Cargo.toml.orig` / `benches/` / `tests/`，并把清单里
对应的 `[[test]]` / `[[bench]]` 条目一并去掉，否则 `cargo` 会去找不存在的文件。

**当前有一处改动**（`grep -rn "claudesktop patch" vendor/gpui-base/` 核对）：

- **长表格按外层视口虚拟化**（`src/text/node.rs` 的 `render_table` / `VirtualTableRows`）：48 行以上的表格
  只布局、绘制视口内（含上下各 6 行 overscan）的行，轨道高度用行高前缀和整份报出，滚动范围和未改动时一致；
  可见行量出的真实行高缓存在解析出的 `Table` 上，下一帧替换单行估算值（长文本换行不再抖）。
  视口取窗口内容遮罩与表格自身边界的交集，不依赖外层 `ListState` 的滞后偏移；单元格序号按整张表算，
  `TextLeafKey` 不变，选中 / 高亮 / 渐隐继续对齐。
  验收：`tools/markdown-sample.md` 换成 256 行表格，`CLAUDESKTOP_OPEN=sample-reply` 配合
  `--features headless-screenshot` 跑 `CLAUDESKTOP_STRESS=0`（300 帧滚动），帧时间 p50 / p95
  从 57.7 / 85.2 ms 降到 24.4 / 34.5 ms（debug + 软件渲染）。

## gpui-pre 0.3.9

gpui-kit 0.7.3 依赖的 gpui 快照（lib 名 `gpui`）。拷自 crates.io 发布包，只删了清单里的
`[[example]]` / `[[test]]`（对应文件没拷）。

改动都标了 `claudesktop patch`（`grep -rn "claudesktop patch" vendor/`）：

- **中文避头尾**（`src/text_system/line_wrapper.rs` 的 `is_cjk_break_forbidden`）：
  收尾标点（`，。、；：？！）」》` …）前不断行，开头标点（`（「《“` …）后不断行，`——` 中间不断开。
  上游在任意两个汉字之间都能断，逗号、右括号会落到行首。
  换行候选有三处一样的逻辑，都接了这个判断：`LineWrapper::wrap_line`（Markdown `TextView` 用）、
  `LineWrapper` 的按行数截断、`LineLayout::compute_wrap_boundaries`（普通文字元素用）。
  测试在 `apps/desktop/src/app/chat.rs` 的 `cjk_wrap_follows_kinsoku`（用 `NoopTextSystem` 驱动真的 `LineWrapper`），
  升级后补丁没打上它会挂。
- **变高列表重排时保留滚动锚点和估算高度**（`src/elements/list.rs` 的 `List::with_estimated_item_height`）：
  源码预览用估算高度让未测量行参与滚动范围。宽度或估算高度变化时，保住视口顶部条目和行内比例，再按需测量实际高度。
  滚动越过估算范围时从实际末项向上排。`bounds_for_item` 从视口顶部累加高度，以免大坐标相减丢失精度。
- **对话列表的行高估高与跟随判定辅助**（`src/elements/list.rs`，2026-11 scroll polish）：
  `splice_with_hints` / `remeasure_with_hints` 让新行和整表重估一次带上估高（滚动条、底部判定第一帧就准）；
  宽度变化时保留每行原有的 `size_hint`；`set_auto_reengage(false)` 关掉「离底 1px 自动重贴」，
  由 `Session::sync_follow` 按几何 + 方向接管；`is_tail_measured` 改为「视口顶到末尾全部实测」，
  避免中间某段估高偏小时提前判定到底。
- **虚拟列表滚动时对齐物理像素**：`src/elements/list.rs` 的 `prepaint_items` 保留连续的逻辑 Y 坐标，
  每项 prepaint 时单独对齐绘制起点。定位自动滚动请求时使用该项的绘制起点。半物理像素统一向正方向取整，
  让顶行切换和跨越零点时的整数物理像素位移一致。

- **滚动时平移重放固定尺寸视图**：`src/view.rs` 的 `Entity::cached_scroll` 保留正文的原始坐标，
  `src/scene.rs` 录制未受外层视口裁切的图元，`src/window.rs` 接力上一帧的布局、元素状态和输入处理器。
  每帧沿 Y 轴按物理像素平移，再应用当前视口裁切。指针按下、抬起或纯横向滚动前同步正文布局，
  避免命中回调使用旧坐标。整窗刷新、缩放变化和 atlas 失效都会更新快照。
- **合成器帧时间戳**：`PlatformWindow::frame_vsync_at` 提供默认空值，`Window::frame_vsync_at` 将它转发给滚轮控制器。
  时间戳独立于 `RequestFrameOptions` 和 profiler。

## markdown 1.0.0

gpui-kit → gpui-base 的 Markdown `TextView` 用的解析器。原样拷自 crates.io 发布包（`Cargo.toml` / `LICENSE` /
`README.md` / `src`）。

改动都标了 `claudesktop patch`（`grep -rn "claudesktop patch" vendor/markdown/`）：

- **GFM 自动链接遇全角标点收尾**（`src/construct/gfm_autolink_literal.rs` 的 `path_inside` /
  `trail` 分支及 `is_non_ascii_punctuation_after` 辅助函数）：上游只把 ASCII 空白当 URL 终点，全角逗号 / 句号 /
  括号被当普通字符连着后面的字一起吞进链接（`http://a.b/，好的` 整句变成 URL；`**http://a.b/**（注）`
  的 `**` 进了 URL，加粗失效、星号原样露出）。按整字符分类，让非 ASCII 标点收尾；路径中的 `/`、`?`、`=` 等 ASCII 标点仍保留。
  测试在 `apps/desktop/src/app/chat.rs` 的 `autolink_ends_at_fullwidth_punctuation`（和 gpui-base 同
  gfm+math 选项直接看 AST），升级后补丁没打上它会挂。

### 升级

markdown crate 换版本时：把新版发布包拷进来，重打上面的补丁（`grep -rn "claudesktop patch"` 核对三处），
跑 `cargo test -p claudesktop autolink`。上游若已修（micromark 上游同样只认 ASCII 空白，短期不会），
删掉本目录和根 `Cargo.toml` 的 patch 行。

## gpui-pre-windows 0.3.9

- **绘制前校验失效区**：`src/events.rs` 的 `draw_window` 拿到绘制锁后立即调用 `ValidateRect`，保留绘制期间下一次 VSync 提交的失效区。重入保护仍校验已收到的 `WM_PAINT`。
- **Windows VSync 时钟**：`src/vsync.rs` 的 `LatestVsyncClock` 用每窗一个原子槽保留最新的 `DwmFlush` 完成时刻。
  `draw_window` 在校验失效区后取出时间戳，限当次 `request_frame` 回调使用。隐藏、最小化和 `DwmFlush` 回退时清空槽，
  离屏节拍测量可用自己的时钟。profiler 继续使用独立的 `PlatformFrameSignal` 记录首个未消费信号。

gpui 的 Windows 平台层（lib 名 `gpui_windows`），原样拷自 crates.io 发布包（删了 `Cargo.toml.orig` / `Cargo.lock`）。

- **文字纵向位置不算 lineGap**（`src/direct_write.rs` 的 `layout_line` / `face_extents` / `DrawGlyphRun`）：
  上游拿 DirectWrite 的行度量当 ascent / descent，那份含字体的 lineGap，gpui 再整份居中进行框，
  lineGap 大的字体（不少宋体 / 华文字体）在设置里选上后文字整体下沉，单行裁切处（面包屑、chip、卡片标题）被切掉下伸部。
  改成取字体自身的 ascent / descent（不含 lineGap）：主字体打底，绘制时并进每段实际用到的字体（中文回退）。
  lineGap 为 0 的字体（内嵌的 Inter、微软雅黑）排版不变。只在 Windows 上编译，Linux 上验证用
  `cargo check -p gpui-pre-windows --target x86_64-pc-windows-msvc`（先 `rustup target add`）。

- **隐藏窗口也应用初始尺寸**（`src/window.rs` 的 `WindowsWindow::new`）：上游在 `show=false` 时只保存 placement，
  实际窗口仍使用 `CW_USEDEFAULT` 的大小，导致无头截图忽略指定的宽高。隐藏分支用
  `SetWindowPos(SWP_NOACTIVATE | SWP_NOZORDER)` 应用 bounds，不显示窗口、不激活、不调整层级。

### 升级

gpui-kit 换了 gpui-pre 版本时（gpui-pre-windows 跟着同一版本号走，一并换）：把新版发布包拷进来（同样删掉 `[[example]]` / `[[test]]`），
再把上面的改动按 `claudesktop patch` 标记重新打一遍，跑 `cargo test -p claudesktop`。
（发布包里的上游测试离开 Zed 仓库编译不了，所以测试放在我们这边。）上游已经有某项改动了就删掉对应目录和 patch。
