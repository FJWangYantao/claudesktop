//! 插件卡片上的版本 chip：`v1.2.0 ▾`，点开列全部 tag（当前那版打勾），选中就 `git checkout` 过去。
//!
//! tag 列表来自本地克隆（`plugin_git::versions`），**点开菜单时**在后台线程读一次、记在
//! [`VersionForm`] 里——不在 render 里跑 git，也不在 render 里读 `ChatApp`（正在借用）。
//!
//! 菜单的开关状态由 gpui-kit `Popover` 自己持有，不往 `ChatApp` 里加 `*_open`。

use super::ChatApp;
use super::menu::{MenuChip, MenuList, menu_item, menu_panel};
use crate::theme::rgb;
use crate::theme::*;
use claudesktop_core::plugin::Plugin;
use claudesktop_core::plugin_git;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::*;
use gpui_kit::*;
use std::collections::HashMap;
use std::path::PathBuf;

/// 版本菜单的状态：正在读的是哪个仓库、读到了哪些 tag、正在换到哪一版。
#[derive(Default)]
pub(crate) struct VersionForm {
    /// 正在读 tag 列表的仓库（读着时菜单里转圈）
    loading: Option<PathBuf>,
    /// 读到的 tag 列表，按仓库目录存
    tags: HashMap<PathBuf, Vec<String>>,
    /// 正在换到哪一版（菜单开着时不让连点）
    switching: Option<SharedString>,
}

impl VersionForm {
    /// 这个仓库读到了哪些 tag（还没读过 = None）。
    fn tags(&self, repo: &PathBuf) -> Option<Vec<String>> {
        self.tags.get(repo).cloned()
    }

    fn loading(&self, repo: &PathBuf) -> bool {
        self.loading.as_ref() == Some(repo)
    }

    fn busy(&self) -> bool {
        self.switching.is_some()
    }
}

impl ChatApp {
    /// 后台读一次这个仓库的 tag（结果进 `plugin_versions.tags`）。`force` = 已经读过也重读。
    pub(super) fn load_versions(&mut self, repo: PathBuf, force: bool, cx: &mut Context<Self>) {
        if !force && self.plugin_versions.tags.contains_key(&repo) {
            return;
        }
        if self.plugin_versions.loading.as_ref() == Some(&repo) {
            return;
        }
        self.plugin_versions.loading = Some(repo.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let read = repo.clone();
            let result =
                cx.background_executor().spawn(async move { plugin_git::versions(&read) }).await;
            let _ = this.update(cx, |app, cx| {
                app.plugin_versions.loading = None;
                // 没有 tag 的仓库很常见（等于「只有最新」），不当成错误
                app.plugin_versions.tags.insert(repo, result.unwrap_or_default());
                cx.notify();
            });
        })
        .detach();
    }

    /// 换到一个版本：后台 `git checkout`，回来重新扫描插件目录（内核拿到新内容）。
    pub(super) fn switch_version(&mut self, repo: PathBuf, rev: String, cx: &mut Context<Self>) {
        if self.plugins_busy() || self.plugin_versions.busy() {
            return;
        }
        self.set_plugins_busy(Some(format!("正在换到 {rev}…").into()));
        self.plugin_versions.switching = Some(rev.clone().into());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (r, v) = (repo.clone(), rev.clone());
            let result =
                cx.background_executor().spawn(async move { plugin_git::checkout(&r, &v) }).await;
            let _ = this.update(cx, |app, cx| {
                app.set_plugins_busy(None);
                app.plugin_versions.switching = None;
                match result {
                    Ok(now) => {
                        app.rescan_plugins(cx);
                        // 换过版本，「当前」标记要重算：丢掉 tag 列表，下次点开重读
                        app.plugin_versions.tags.remove(&repo);
                        app.toast(Notification::success(format!("换到 {now} 了")), cx);
                    }
                    Err(err) => app.toast(Notification::error(format!("没换成：{err}")), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// 版本号显示：`v1.2.0` 原样，`1.2.0` 补个 v；已经是短提交（十六进制）就原样。
pub(super) fn version_label(version: &str) -> String {
    let v = version.trim();
    if v.is_empty() {
        return String::new();
    }
    let hex_only = v.len() >= 7 && v.chars().all(|c| c.is_ascii_hexdigit());
    if hex_only || v.starts_with('v') { v.to_string() } else { format!("v{v}") }
}

/// 推荐列表里的版本标注：长的提交哈希只留前 7 位。
pub(super) fn short_rev(rev: &str) -> String {
    let rev = rev.trim();
    let hex_only = rev.len() > 7 && rev.chars().all(|c| c.is_ascii_hexdigit());
    if hex_only { rev[..7].to_string() } else { rev.to_string() }
}

/// 插件卡片上的版本：从 git 装的给一个能换版本的 chip，手动放进来的只显示清单里的版本号。
pub(super) fn version_chip(
    i: usize,
    p: &Plugin,
    handle: Entity<ChatApp>,
    focus: FocusHandle,
    cx: &mut Context<ChatApp>,
) -> AnyElement {
    let current = p.version.clone();
    let Some(repo) = p.repo.clone() else {
        return match current.as_deref() {
            Some(v) => div()
                .flex_shrink_0()
                .text_size(TEXT_SMALL)
                .text_color(rgb(INK_SOFT))
                .child(version_label(v))
                .into_any_element(),
            None => div().into_any_element(),
        };
    };
    let form = handle.read(cx);
    let form = form.version_form();
    let loading = form.loading(&repo);
    let tags = form.tags(&repo);
    let busy = form.busy();
    let label: SharedString = match current.as_deref() {
        Some(v) => version_label(v).into(),
        None => "版本".into(),
    };
    let chip = MenuChip::new(IconAll::Tag, label);
    let popover = menu_panel(Popover::new(("plugin-version", i)), &focus)
        .anchor(Anchor::BottomRight)
        .w(rems(16.))
        .trigger(chip)
        .on_open_change({
            let handle = handle.clone();
            let repo = repo.clone();
            move |&open: &bool, _, cx| {
                if open {
                    // 点开菜单时才读 tag（不在 render 里跑 git）
                    handle.update(cx, |this, cx| this.load_versions(repo.clone(), false, cx));
                }
            }
        })
        .content(move |_, _, cx| {
            let popover = cx.entity();
            let mut list = MenuList::new(("plugin-version-list", i), &handle, &popover);
            match (&tags, loading) {
                (_, true) => {
                    list = list.child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .px_2p5()
                            .py_2()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK_SOFT))
                            .child(super::motion::spinner())
                            .child("正在读版本…"),
                    )
                }
                (Some(tags), _) if tags.is_empty() => {
                    list = list.child(
                        div()
                            .px_2p5()
                            .py_2()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK_SOFT))
                            .child("这个仓库没打版本号（tag）：只能跟最新"),
                    )
                }
                (Some(tags), _) => {
                    for (n, tag) in tags.iter().enumerate() {
                        let active = is_current(current.as_deref(), tag);
                        let row = menu_item(
                            ("plugin-version-item", n),
                            IconAll::Tag,
                            tag.clone(),
                            "",
                            active,
                        );
                        let (repo, tag) = (repo.clone(), tag.clone());
                        list = list.pick(row, move |this, cx| {
                            this.switch_version(repo.clone(), tag.clone(), cx);
                        });
                    }
                }
                (None, _) => {
                    list = list.child(
                        div()
                            .px_2p5()
                            .py_2()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK_SOFT))
                            .child("再点一下读版本"),
                    )
                }
            }
            if busy {
                list = list.child(
                    div()
                        .px_2p5()
                        .py_1()
                        .text_size(TEXT_SMALL)
                        .text_color(rgb(INK_SOFT))
                        .child("正在换版本，稍等"),
                );
            }
            list.finish(&focus)
        });
    popover.into_any_element()
}

/// 这一版是不是现在检出的那版（tag 可能写成 `v1.2.0`，清单里写 `1.2.0`）。
fn is_current(current: Option<&str>, tag: &str) -> bool {
    match current {
        Some(c) => c == tag || c.trim_start_matches('v') == tag.trim_start_matches('v'),
        None => false,
    }
}
