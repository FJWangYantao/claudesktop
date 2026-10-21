//! git worktree 会话：在后台建 / 删 worktree，顶栏的分支 chip 和它的菜单。
//!
//! 位置在数据目录 `worktrees/<仓库名>-<短哈希>/<名字>`，不往用户仓库里塞东西（见「设计说明-worktree.md」）。
//! 界面上一律显示会话标题 + 分支名，不显示目录名；"看状态" 只在点开菜单和一轮结束时跑
//! `git status`，不在 render 里跑。

use super::ChatApp;
use super::menu::{MenuChip, MenuList, menu_item, menu_popover};
use crate::session::Session;
use crate::theme::rgb;
use crate::theme::*;
use claudesktop_core::WorktreeMeta;
use claudesktop_core::git;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::base::PopoverState;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::*;
use std::path::{Path, PathBuf};

/// 展开 / 收起目录和分支 chip 的菜单宽度。
const CHIP_MENU_W: Rems = rems(21.);

/// worktree 目录：`<数据目录>/worktrees/<仓库名>-<短哈希>/<分支名里的那一段>`。
/// 短哈希让同名仓库（不同位置的 `app`）不撞车；分支名里的 `/` 换成 `-` 当目录名。
pub fn worktree_path(data: &Path, repo: &Path, branch: &str) -> PathBuf {
    let name = repo.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let folder: String = branch
        .rsplit('/')
        .next()
        .unwrap_or("wt")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '-' })
        .collect();
    data.join("worktrees").join(format!("{name}-{}", short_hash(repo))).join(folder)
}

/// 路径的短哈希（8 位十六进制）：同一台机器上同名仓库靠它分开。
fn short_hash(path: &Path) -> String {
    use std::hash::Hasher as _;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(path.to_string_lossy().to_lowercase().as_bytes());
    format!("{:016x}", hasher.finish())[..8].to_string()
}

impl ChatApp {
    /// 输入框上方的目录菜单里的「在新 worktree 中开始」：当前目录是 git 仓库时才有。
    /// 后台建 worktree（大仓库检出要几秒），会话的 `cwd` 换过去，消息照常发（没建好时排队）。
    pub(crate) fn start_in_worktree(&mut self, cx: &mut Context<Self>) {
        let repo = match git::repo_root(&self.session().cwd) {
            Some(repo) => repo,
            None => {
                self.toast(Notification::warning("这个目录不是 git 仓库，建不了 worktree"), cx);
                return;
            }
        };
        let Some(data) = claudesktop_core::default_data_dir().ok() else {
            self.toast(Notification::error("找不到数据目录"), cx);
            return;
        };
        let offset = crate::usage::local_offset_secs();
        let branch = git::new_branch_name(now_ms(), offset);
        let base = match git::base_of(&repo) {
            Ok(base) => base,
            Err(err) => {
                self.toast(Notification::error(format!("读不出分支：{err}")), cx);
                return;
            }
        };
        let path = worktree_path(&data, &repo, &branch);
        let active = self.active;
        // 正在准备中：输入框照常可写，期间发的消息排队（见 `submit_to`）
        self.sessions[active].worktree_busy = Some(branch.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let job = {
                let (repo, path, branch) = (repo.clone(), path.clone(), branch.clone());
                cx.background_executor()
                    .spawn(async move { git::add_worktree(&repo, &path, &branch).map(|()| path) })
            };
            let result = job.await;
            let _ = this.update(cx, |app, cx| {
                // 期间多半还在同一个会话上（换会话了就不动它）
                if active >= app.sessions.len() {
                    return;
                }
                let meta = WorktreeMeta { repo: repo.clone(), branch: branch.clone(), base };
                app.sessions[active].worktree_busy = None;
                match result {
                    Ok(path) => app.adopt_worktree(active, path, meta, cx),
                    Err(err) => app.toast(
                        Notification::error(format!(
                            "没建成 worktree：{err}（这段对话还在原目录）"
                        )),
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 建好了：会话换到 worktree 目录，并在内核上重新开一个会话（还没发过消息的原地换；
    /// 已经有对话的了另开一个对应的，见 [`Self::adopt_worktree`] 的说明）。
    fn adopt_worktree(
        &mut self,
        idx: usize,
        path: PathBuf,
        meta: WorktreeMeta,
        cx: &mut Context<Self>,
    ) {
        let s = &self.sessions[idx];
        let has_chat = s.has_user_message() || s.busy;
        if has_chat {
            // 已经有对话：换个目录就等于换个会话（对话不能跨目录），开一个新的接着来
            let kind = s.kernel;
            let new = self.open_session(kind, path.clone());
            self.sessions[new].worktree = Some(meta);
            self.sessions[new].task = false;
            self.active = new;
            self.toast(
                Notification::success(
                    "worktree 建好了：新对话在它里面开始（原来的对话留在原目录）",
                ),
                cx,
            );
            return;
        }
        let kind = s.kernel;
        // 建 worktree 期间排队的消息：换会话（`replace_session`）会新建一个 Session，先收好再放回去
        let queued = std::mem::take(&mut self.sessions[idx].queued);
        self.replace_session(idx, kind, path.clone());
        let s = &mut self.sessions[idx];
        s.worktree = Some(meta);
        s.task = false;
        s.queued = queued;
        // 元信息还没建档的会话：建档时按 `Session::worktree` 写（见 `ensure_log`）
        if let Some(existing) = &mut s.meta {
            existing.worktree = s.worktree.clone();
            existing.task = false;
            existing.cwd = path;
            let meta = existing.clone();
            if let Some(store) = &self.store {
                let _ = store.save_meta(&meta);
            }
        }
        // 建好之前写的消息接着发（会话还没就绪时留着，Started 之后由 `dispatch_queued` 带走）
        self.dispatch_queued(idx, cx);
        cx.notify();
    }

    /// 会话是不是停在一个已经不存在的 worktree 目录上（用户手动删了 / 换了电脑）。
    pub(crate) fn worktree_is_missing(s: &Session) -> bool {
        s.worktree.is_some() && !s.cwd.is_dir()
    }

    /// 打开历史会话时发现 worktree 目录没了：分支还在就重新检出接上，分支也没了就说明白。
    pub(crate) fn repair_worktree(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(meta) = self.sessions[idx].worktree.clone() else {
            return;
        };
        let path = self.sessions[idx].cwd.clone();
        if path.is_dir() {
            return;
        }
        if !meta.repo.is_dir() {
            self.sessions[idx].note(format!(
                "worktree 目录和主仓库都不在了（{}）：这段对话只能看，不能再跑",
                meta.repo.display()
            ));
            return;
        }
        let branch = meta.branch.clone();
        let (repo, path2) = (meta.repo.clone(), path.clone());
        cx.spawn(async move |this, cx| {
            let job = {
                let (repo, path, branch) = (repo, path2, branch);
                cx.background_executor().spawn(async move {
                    if git::branch_exists(&repo, &branch) {
                        git::reattach_worktree(&repo, &path, &branch)
                    } else {
                        Err(format!("分支 {branch} 已经不在了"))
                    }
                })
            };
            let result = job.await;
            let _ = this.update(cx, |app, cx| {
                if idx >= app.sessions.len() {
                    return;
                }
                match result {
                    Ok(()) => app.sessions[idx].note("已重新检出 worktree"),
                    Err(err) => app.sessions[idx].note(format!(
                        "worktree 接不上了：{err}（这段对话只能看，改动要么在主目录里做，要么新建对话）"
                    )),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 刷新第 `idx` 个会话的 worktree 状态（脏文件数、领先 / 落后几个提交）。后台跑，不阻塞界面。
    pub(crate) fn refresh_worktree(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(meta) = self.sessions[idx].worktree.clone() else {
            return;
        };
        let path = self.sessions[idx].cwd.clone();
        if !path.is_dir() {
            // 目录不在（还没接回来 / 被删了）：没有状态可读
            self.sessions[idx].worktree_state = None;
            return;
        }
        self.sessions[idx].worktree_state = Some(None);
        cx.spawn(async move |this, cx| {
            let job = {
                let (repo, base, branch) =
                    (meta.repo.clone(), meta.base.clone(), meta.branch.clone());
                let path = path.clone();
                cx.background_executor()
                    .spawn(async move { git::status(&repo, &path, &base, &branch) })
            };
            let result = job.await;
            let _ = this.update(cx, |app, cx| {
                if let Some(s) = app.sessions.get_mut(idx) {
                    s.worktree_state = Some(result.ok());
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 分支 chip 上的文字：`⎇ 分支名 · 3 个文件 · 领先 2`，状态还没读到时只显示分支名。
    pub(crate) fn worktree_chip_label(&self) -> Option<SharedString> {
        let s = self.session();
        let meta = s.worktree.as_ref()?;
        let name = meta.branch.clone();
        let mut parts = vec![name];
        match s.worktree_state {
            Some(Some(status)) => {
                if status.dirty_files > 0 {
                    parts.push(format!("{} 个文件", status.dirty_files));
                }
                if status.ahead > 0 {
                    parts.push(format!("领先 {}", status.ahead));
                }
                if status.behind > 0 {
                    parts.push(format!("落后 {}", status.behind));
                }
                if status.is_disposable() {
                    parts.push("干净".into());
                }
            }
            Some(None) => parts.push("状态读不出来".into()),
            None => {}
        }
        Some(parts.join(" · ").into())
    }

    /// 分支 chip 的菜单：打开目录、复制分支名 / 路径、合并回 base、删除 worktree。
    pub(super) fn worktree_menu_items(
        app: &ChatApp,
        handle: &Entity<ChatApp>,
        popover: &Entity<PopoverState>,
    ) -> MenuList {
        let mut menu = MenuList::new("worktree-menu-list", handle, popover);
        let Some(meta) = app.session().worktree.clone() else {
            return menu;
        };
        let branch = meta.branch.clone();
        let path = app.session().cwd.clone();
        let repo = meta.repo.clone();
        let branch_tip = branch.clone();
        let base = meta.base.clone();
        let path_tip = path.display().to_string();
        let copy_branch = branch.clone();
        let copy_path = path_tip.clone();
        let repo_tip = repo.display().to_string();
        let name = meta.repo_name();
        let reveal_path = path.clone();
        menu = menu
            .pick(
                menu_item(
                    "worktree-reveal",
                    IconAll::FolderOpen,
                    "在资源管理器中打开",
                    repo_tip,
                    false,
                ),
                move |_, cx| cx.reveal_path(&reveal_path),
            )
            .pick(
                menu_item("worktree-copy-branch", IconAll::Copy, "复制分支名", branch_tip, false),
                move |this, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_branch.clone()));
                    this.toast(Notification::success("分支名已复制"), cx);
                },
            )
            .pick(
                menu_item("worktree-copy-path", IconAll::Copy, "复制路径", path_tip.clone(), false),
                move |this, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_path.clone()));
                    this.toast(Notification::success("路径已复制"), cx);
                },
            );
        // 合并回 base：让这个会话的 agent 去跑（它能在 worktree 里解冲突），比在主目录里迁就
        let prompt = format!(
            "把当前分支 {branch} 合并回主目录（{}）的 {base} 分支：先在命令行里 `git fetch` 一下、\
             把 {base} 合并进当前分支并解决冲突，跑完测试，然后在主目录里 `git merge --no-ff {branch}`。\
             主目录停在别的分支或有没提交的改动时不要动它，先把原因告诉我。",
            repo.display()
        );
        menu = menu.pick_in(
            menu_item(
                "worktree-merge",
                IconAll::GitMerge,
                "合并回主目录",
                format!("让这个会话的 agent 做：{name} 的 {base}"),
                false,
            ),
            move |this, window, cx| this.fill_prompt(&prompt, window, cx),
        );
        // 夹一行说明：不能一键合并的原因
        let reason = if path.is_dir() {
            git::merge_blocker(&repo, &meta.base)
        } else {
            Some("worktree 目录不在".to_string())
        };
        if let Some(reason) = reason {
            menu = menu.child(
                div()
                    .px_2p5()
                    .py_1()
                    .text_size(TEXT_META)
                    .text_color(rgb(INK_SOFT))
                    .child(format!("现在不能自动合并：{reason}")),
            );
        }
        menu.child(div().mt_0p5().border_t_1().border_color(rgb(LINE))).pick(
            menu_item(
                "worktree-remove",
                IconAll::Trash,
                "删除 worktree",
                if path.is_dir() {
                    "分支和目录一起删；有没提交的改动会一起丢"
                } else {
                    ""
                },
                false,
            ),
            move |this, cx| this.remove_session_worktree(cx),
        )
    }

    /// 调试钩子 `CLAUDESKTOP_OPEN=worktree`：在临时目录里建一个 git 仓库当工作目录，
    /// 再照目录菜单那条路建一个 worktree 会话（截图 / 自检用，不改用户的数据目录）。
    pub(crate) fn debug_worktree(&mut self, cx: &mut Context<Self>) {
        let base = std::env::temp_dir().join(format!("claudesktop-wt-demo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("demo-repo");
        if std::fs::create_dir_all(&repo).is_err() {
            self.toast(Notification::error("建不了临时仓库"), cx);
            return;
        }
        let run = |args: &[&str]| {
            let _ = std::process::Command::new("git").args(args).current_dir(&repo).output();
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "demo@example.com"]);
        run(&["config", "user.name", "demo"]);
        let _ = std::fs::write(repo.join("README.md"), "demo\n");
        run(&["add", "."]);
        run(&["commit", "-qm", "第一版"]);
        let kind = self.settings.kernel();
        self.active = self.open_session(kind, repo);
        self.start_in_worktree(cx);
    }

    /// 删掉当前会话的 worktree 和分支（有没提交的改动时先问一句，见 `worktree.rs` 的菜单）。
    pub(crate) fn remove_session_worktree(&mut self, cx: &mut Context<Self>) {
        let idx = self.active;
        let Some(meta) = self.sessions[idx].worktree.clone() else {
            return;
        };
        let path: PathBuf = self.sessions[idx].cwd.clone();
        let (repo, branch) = (meta.repo.clone(), meta.branch.clone());
        let branch_name = branch.clone();
        self.sessions[idx].note("正在删除 worktree…".to_string());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let job = {
                let (repo, path, branch) = (repo, path, branch);
                cx.background_executor().spawn(async move {
                    let result = git::remove_worktree(&repo, &path, true);
                    let _ = git::delete_branch(&repo, &branch, true);
                    result
                })
            };
            let result = job.await;
            let _ = this.update(cx, |app, cx| {
                let Some(s) = app.sessions.get_mut(idx) else {
                    return;
                };
                s.worktree = None;
                s.worktree_state = None;
                if let Some(existing) = &mut s.meta {
                    existing.worktree = None;
                    if let Some(store) = &app.store {
                        let _ = store.save_meta(existing);
                    }
                }
                match result {
                    Ok(()) => s.note(format!(
                        "worktree 和分支 {branch_name} 删掉了（改动如果没合并就一起没了）"
                    )),
                    Err(err) => s.note(format!("没删干净：{err}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 会话被丢弃 / 归档时顺手清掉它从没改过东西的 worktree（别在数据目录里堆空目录）。
    pub(crate) fn cleanup_worktree(&mut self, idx: usize) {
        let Some(meta) = self.sessions.get(idx).and_then(|s| s.worktree.clone()) else {
            return;
        };
        // 有对话就不管了：删掉可能丢东西，交给菜单里的「删除 worktree」由用户决定
        if self.sessions[idx].has_user_message() {
            return;
        }
        cleanup_worktree_now(&meta, &self.sessions[idx].cwd);
    }

    /// 同上，会话已经从列表里摘下来了（删除会话时用）。
    pub(crate) fn cleanup_worktree_of(&self, s: &Session) {
        let Some(meta) = s.worktree.clone() else {
            return;
        };
        if s.has_user_message() {
            return;
        }
        cleanup_worktree_now(&meta, &s.cwd);
    }
}

/// 干净（没有改动、也没有未合并的提交）就删掉 worktree 和它的分支；否则留着。
fn cleanup_worktree_now(meta: &claudesktop_core::WorktreeMeta, path: &Path) {
    let meta = meta.clone();
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        let Ok(status) = git::status(&meta.repo, &path, &meta.base, &meta.branch) else {
            return;
        };
        if !status.is_disposable() {
            return;
        }
        if git::remove_worktree(&meta.repo, &path, true).is_ok() {
            let _ = git::delete_branch(&meta.repo, &meta.branch, false);
        }
    });
}

/// 现在几点（毫秒）：分支名里的「月日-时分」用它。
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// 顶栏的分支 chip（Popover trigger）。
pub(super) fn worktree_chip(label: SharedString) -> MenuChip {
    MenuChip::new(IconAll::GitBranch, label)
}

/// 顶栏上的分支 chip：只有 worktree 会话才画（见 `chrome.rs` 的 `render_header`）。
pub(super) fn render_worktree_chip(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    focus: &FocusHandle,
) -> Option<Popover> {
    let label = app.worktree_chip_label()?;
    Some(menu_popover(
        "worktree-menu",
        Anchor::TopRight,
        CHIP_MENU_W,
        handle,
        focus,
        worktree_chip(label),
        ChatApp::worktree_menu_items,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// worktree 目录：数据目录 / worktrees / <仓库名>-<短哈希> / <分支名尾段>
    #[core::prelude::v1::test]
    fn worktree_paths_live_under_the_data_directory() {
        let data = Path::new("C:/data");
        let repo = Path::new("C:/work/app");
        let path = worktree_path(data, repo, "claudesktop/1020-1432-k7f");
        assert!(path.starts_with(data.join("worktrees")), "{}", path.display());
        assert_eq!(path.file_name().unwrap(), "1020-1432-k7f");
        let folder = path.parent().unwrap().file_name().unwrap().to_string_lossy().to_string();
        assert!(folder.starts_with("app-"), "{folder}");
        assert_eq!(folder.len(), "app-".len() + 8);
        // 同一个仓库 → 同一个目录；不同位置的同名仓库 → 不同目录
        assert_eq!(path, worktree_path(data, repo, "claudesktop/1020-1432-k7f"));
        assert_ne!(short_hash(repo), short_hash(Path::new("D:/other/app")), "同名仓库靠短哈希分开");
        // 分支名里不合法的字符换成 -
        let odd = worktree_path(data, repo, "claudesktop/a b/c");
        assert_eq!(odd.file_name().unwrap(), "c");
    }
}
