# vendor：打过补丁的上游 crate

经根 `Cargo.toml` 的 `[patch.crates-io]` 替换 crates.io 上的同名版本。工作区 `exclude` 了本目录，
`cargo test --workspace` / clippy 不会跑到这里。

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

## gpui-pre-windows 0.3.9

gpui 的 Windows 平台层（lib 名 `gpui_windows`），原样拷自 crates.io 发布包（删了 `Cargo.toml.orig` / `Cargo.lock`）。

- **文字纵向位置不算 lineGap**（`src/direct_write.rs` 的 `layout_line` / `face_extents` / `DrawGlyphRun`）：
  上游拿 DirectWrite 的行度量当 ascent / descent，那份含字体的 lineGap，gpui 再整份居中进行框，
  lineGap 大的字体（不少宋体 / 华文字体）在设置里选上后文字整体下沉，单行裁切处（面包屑、chip、卡片标题）被切掉下伸部。
  改成取字体自身的 ascent / descent（不含 lineGap）：主字体打底，绘制时并进每段实际用到的字体（中文回退）。
  lineGap 为 0 的字体（内嵌的 Inter、微软雅黑）排版不变。只在 Windows 上编译，Linux 上验证用
  `cargo check -p gpui-pre-windows --target x86_64-pc-windows-msvc`（先 `rustup target add`）。

### 升级

gpui-kit 换了 gpui-pre 版本时（gpui-pre-windows 跟着同一版本号走，一并换）：把新版发布包拷进来（同样删掉 `[[example]]` / `[[test]]`），
再把上面的改动按 `claudesktop patch` 标记重新打一遍，跑 `cargo test -p claudesktop`。
（发布包里的上游测试离开 Zed 仓库编译不了，所以测试放在我们这边。）上游已经有某项改动了就删掉对应目录和 patch。
