//! 主工作目录的分支缓存与切换：磁盘和 git 都只在后台线程跑。

use super::ChatApp;
use claudesktop_core::git;
use gpui_kit::component::notification::Notification;
use gpui_kit::*;

impl ChatApp {
    /// 会话 / 目录 / 外部分支可能变了时刷新。分支没变就不让输入区重画。
    pub(super) fn refresh_git_branch(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(s) = self.sessions.get(idx) else { return };
        if s.is_chat() || s.task || s.worktree.is_some() {
            let changed = s.git_branch.is_some() || s.git_detached;
            let s = &mut self.sessions[idx];
            s.git_branch = None;
            s.git_detached = false;
            if changed && self.active == idx {
                self.refresh_composer(cx);
            }
            return;
        }
        let cwd = s.cwd.clone();
        let token = s.token;
        cx.spawn(async move |this, cx| {
            let result = {
                let cwd = cwd.clone();
                cx.background_executor().spawn(async move { git::current_branch(&cwd) }).await
            };
            let _ = this.update(cx, |app, cx| {
                let Some(idx) = app.session_index(token) else { return };
                if app.sessions[idx].cwd != cwd
                    || app.sessions[idx].task
                    || app.sessions[idx].worktree.is_some()
                {
                    return;
                }
                let name = result.as_ref().map(|b| SharedString::from(b.name.clone()));
                let detached = result.as_ref().is_some_and(|b| b.detached);
                let s = &mut app.sessions[idx];
                if s.git_branch == name && s.git_detached == detached {
                    return;
                }
                s.git_branch = name;
                s.git_detached = detached;
                if app.active == idx {
                    app.refresh_composer(cx);
                }
            });
        })
        .detach();
    }

    /// 分支菜单每次打开都重读当前分支和本地分支表。
    pub(super) fn fetch_branches_for_menu(&mut self, cx: &mut Context<Self>) {
        let s = self.session();
        if s.task || s.is_chat() || s.worktree.is_some() {
            return;
        }
        let cwd = s.cwd.clone();
        let token = s.token;
        if self.git_branches_loading && self.git_branches_cwd.as_deref() == Some(cwd.as_path()) {
            return;
        }
        if self.git_branches_cwd.as_deref() != Some(cwd.as_path()) {
            self.git_branches.clear();
        }
        self.git_branches_loading = true;
        self.git_branches_cwd = Some(cwd.clone());
        cx.spawn(async move |this, cx| {
            let (current, branches) = {
                let cwd = cwd.clone();
                cx.background_executor()
                    .spawn(async move { (git::current_branch(&cwd), git::list_branches(&cwd)) })
                    .await
            };
            let _ = this.update(cx, |app, cx| {
                app.git_branches_loading = false;
                if app.session().token != token || app.session().cwd != cwd {
                    return;
                }
                if let Ok(branches) = branches {
                    app.git_branches = branches;
                }
                if let Some(current) = current {
                    let idx = app.active;
                    app.sessions[idx].git_branch = Some(current.name.into());
                    app.sessions[idx].git_detached = current.detached;
                }
                app.refresh_composer(cx);
            });
        })
        .detach();
    }

    /// 切分支只改主工作目录；已有对话不提供这个入口，但令牌和目录仍再次校验。
    pub(super) fn switch_branch(&mut self, target: String, cx: &mut Context<Self>) {
        let s = self.session();
        if self.git_switching || s.busy || s.worktree_busy.is_some() {
            self.toast(Notification::warning("当前操作还没结束，稍等再切分支"), cx);
            return;
        }
        if s.task || s.is_chat() || s.worktree.is_some() || s.has_user_message() {
            return;
        }
        if !s.git_detached && s.git_branch.as_deref() == Some(target.as_str()) {
            return;
        }
        let cwd = s.cwd.clone();
        self.git_switching = true;
        cx.spawn(async move |this, cx| {
            let result = {
                let (cwd, target) = (cwd.clone(), target.clone());
                cx.background_executor()
                    .spawn(async move { git::switch_branch(&cwd, &target) })
                    .await
            };
            let _ = this.update(cx, |app, cx| {
                app.git_switching = false;
                match result {
                    Ok(()) => {
                        for session in &mut app.sessions {
                            if session.cwd == cwd
                                && session.worktree.is_none()
                                && !session.is_chat()
                                && !session.task
                            {
                                session.git_branch = Some(target.clone().into());
                                session.git_detached = false;
                            }
                        }
                        app.git_branches_cwd = None;
                        app.completion.invalidate_files();
                        if app.right_panel.open {
                            app.right_panel.refresh();
                        }
                        app.toast(Notification::success(format!("已切到分支 {target}")), cx);
                    }
                    Err(err) => {
                        app.toast(Notification::error(format!("切换分支失败：{err}")), cx);
                        let active = app.active;
                        app.refresh_git_branch(active, cx);
                    }
                }
                app.refresh_composer(cx);
                cx.notify();
            });
        })
        .detach();
    }
}
