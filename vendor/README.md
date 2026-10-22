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

### 升级

gpui-kit 换了 gpui-pre 版本时：把新版发布包拷进来（同样删掉 `[[example]]` / `[[test]]`），
再把上面的改动按 `claudesktop patch` 标记重新打一遍，跑 `cargo test -p claudesktop`。
（发布包里的上游测试离开 Zed 仓库编译不了，所以测试放在我们这边。）上游已经有避头尾了就删掉本目录和 patch。
