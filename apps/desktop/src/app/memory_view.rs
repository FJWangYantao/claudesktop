//! 设置页「记忆」：内核跨会话记住的用户偏好、项目约定、做过的决定、踩过的坑。
//!
//! 分「这个项目 / 全局」两栏，上面一个搜索框，下面是条目卡片（点 × 删除、立即生效）。
//! **文件是唯一真相**：这一页每次打开、每次内核记下一条都重读一次，不缓存到内存里长期用；
//! 删一条就是读 → 去掉那条 → 原子写回（`claudesktop_core::memory`），内核下一轮 `send_prompt`
//! 重读文件时就不带了。
//!
//! 项目记忆在 `<项目根>/.claudesktop/memory.json`（跟 `AGENTS.md` 一样会进 git，团队共享），
//! 全局的在数据目录的 `memory.json`。三层东西怎么分工见「设计说明-记忆系统.md」第七节。

use super::ChatApp;
use super::catalog_view::{matches, no_match};
use super::settings_kit::{how_to, note, section, seg_control};
use super::widgets::Surface;
use crate::theme::rgb;
use crate::theme::*;
use claudesktop_core::memory::{self, Entry, Scope};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;

/// 设置页「记忆」分类的下标
pub(super) const MEMORY_TAB: usize = 7;

/// 条目攒到这个数就提示整理（设计说明第七节第 2 条）
const TIDY_HINT: usize = 30;

/// 「记忆」页的状态：看哪一栏、搜索词、读到的两份记忆。
pub(super) struct MemoryForm {
    pub(super) scope: Scope,
    pub(super) search: Entity<InputState>,
    /// 项目的记忆（按文件里的顺序，新的在后）
    project: Vec<Entry>,
    global: Vec<Entry>,
    /// 两份文件的位置（底部说明、空状态里给出来）
    project_path: PathBuf,
    global_path: Option<PathBuf>,
    /// 读不出来 / 没建过的原因（文件坏了、数据目录不可用）
    warnings: Vec<String>,
}

impl MemoryForm {
    pub(super) fn new(window: &mut Window, cx: &mut Context<ChatApp>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("搜索内容或标签"));
        // 打字即筛
        cx.subscribe(&search, |_: &mut ChatApp, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        Self {
            scope: Scope::Project,
            search,
            project: Vec::new(),
            global: Vec::new(),
            project_path: PathBuf::new(),
            global_path: None,
            warnings: Vec::new(),
        }
    }

    /// 这一栏的条目（新的在前）
    fn entries(&self) -> Vec<&Entry> {
        let list = match self.scope {
            Scope::Project => &self.project,
            Scope::Global => &self.global,
        };
        list.iter().rev().collect()
    }

    /// 这一栏记忆文件的位置
    fn path(&self) -> Option<&PathBuf> {
        match self.scope {
            Scope::Project => Some(&self.project_path),
            Scope::Global => self.global_path.as_ref(),
        }
    }

    fn count(&self, scope: Scope) -> usize {
        match scope {
            Scope::Project => self.project.len(),
            Scope::Global => self.global.len(),
        }
    }

    /// 搜索词（去掉首尾空白）；空 = 不筛。
    fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().trim().to_string()
    }
}

impl ChatApp {
    /// 重读两份记忆文件（打开设置页、切到「记忆」、内核记下一条时）。不负责重画。
    pub(super) fn reload_memory_view(&mut self) {
        let cwd = self.sessions[self.active].cwd.clone();
        let project_path = memory::project_file_or(&cwd);
        let global_path = memory::global_path();
        let (project, project_warning) = memory::load(&project_path);
        let (global, global_warning) =
            global_path.as_ref().map(|p| memory::load(p)).unwrap_or_default();
        let form = &mut self.memory_form;
        form.project = project;
        form.global = global;
        form.project_path = project_path;
        form.global_path = global_path;
        form.warnings = Vec::new();
        form.warnings.extend(project_warning);
        form.warnings.extend(global_warning);
        if form.global_path.is_none() {
            form.warnings.push("找不到数据目录，全局记忆不生效".into());
        }
    }

    /// 删一条记忆：读 → 去掉这条 → 原子写回。立即生效（内核每轮重读文件）。
    fn remove_memory(&mut self, scope: Scope, entry: Entry, cx: &mut Context<Self>) {
        let Some(path) = (match scope {
            Scope::Project => Some(self.memory_form.project_path.clone()),
            Scope::Global => self.memory_form.global_path.clone(),
        }) else {
            return;
        };
        let (mut entries, _) = memory::load(&path);
        entries.retain(|e| e != &entry);
        match memory::save(&path, &entries) {
            Ok(()) => self.reload_memory_view(),
            Err(err) => {
                self.toast(Notification::error(format!("删不掉 {}：{err}", path.display())), cx)
            }
        }
        cx.notify();
    }

    /// 用资源管理器打开记忆文件（没建过就打开它所在的文件夹）。
    fn reveal_memory(&self, scope: Scope, cx: &mut Context<Self>) {
        let path = match scope {
            Scope::Project => Some(self.memory_form.project_path.clone()),
            Scope::Global => self.memory_form.global_path.clone(),
        };
        let Some(path) = path else {
            return;
        };
        let target =
            if path.exists() { path } else { path.parent().map(PathBuf::from).unwrap_or(path) };
        cx.reveal_path(&target);
    }

    pub(super) fn render_memory(&self, _window: &mut Window, cx: &mut Context<Self>) -> Div {
        let form = &self.memory_form;
        let scope = form.scope;
        // 两栏切换 + 搜索框
        let bar = div()
            .h_flex()
            .items_center()
            .gap_3()
            .child(seg_control(
                "memory-scope",
                [
                    (Scope::Project, format!("这个项目 {}", form.count(Scope::Project))),
                    (Scope::Global, format!("全局 {}", form.count(Scope::Global))),
                ],
                scope,
                |this, scope, _, cx| {
                    this.memory_form.scope = scope;
                    cx.notify();
                },
                cx,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(CARD))
                    .px_1()
                    .child(
                        Input::new(&form.search)
                            .appearance(false)
                            .bordered(false)
                            .cleanable(true)
                            .prefix(
                                Icon::new(IconAll::Search).size_3p5().text_color(rgb(INK_SOFT)),
                            ),
                    ),
            );
        let query = form.query(cx);
        let shown: Vec<&Entry> = form
            .entries()
            .into_iter()
            .filter(|e| matches(&query, &[(e.text.as_str()), &e.tags.join(" ")]))
            .collect();
        let total = form.count(scope);
        let body = if total == 0 {
            self.memory_empty(scope)
        } else if shown.is_empty() {
            no_match(&query)
        } else {
            self.memory_list(&shown, cx)
        };
        // 底部：文件位置 + 攒多了提示整理
        let path_line = match form.path() {
            Some(path) => note(IconAll::Folder, format!("文件：{}", path.display())),
            None => note(IconAll::Info, "找不到数据目录，全局记忆不生效"),
        };
        let footer = div()
            .v_flex()
            .gap_1p5()
            .when(scope == Scope::Project, |d| {
                d.child(note(
                    IconAll::Info,
                    "这个文件在项目目录里，会跟着仓库一起进 git，团队共享；只想自己知道的事记到「全局」",
                ))
            })
            .child(path_line)
            .when(total >= TIDY_HINT, |d| {
                d.child(note(
                    IconAll::Lightbulb,
                    format!(
                        "已经 {total} 条了：关于怎么构建、代码规范这类该写进 AGENTS.md 的，整理过去之后可以在这里删掉"
                    ),
                ))
            });
        let warnings = form.warnings.clone();
        let actions = div()
            .h_flex()
            .gap_1p5()
            .child(
                Self::text_button("memory-reveal", "打开文件位置", false)
                    .small()
                    .on_click(cx.listener(move |this, _, _, cx| this.reveal_memory(scope, cx))),
            )
            .into_any_element();
        section(
            "记住的事",
            "跨会话记得的偏好、项目约定、做过的决定和踩过的坑；内核每轮自动带上相关的几条",
            Some(actions),
            div().v_flex().gap_4().child(bar).child(body).child(footer).children(
                warnings.into_iter().map(|w| {
                    div().text_size(TEXT_SMALL).line_height(LEADING).text_color(rgb(RED)).child(w)
                }),
            ),
        )
    }

    /// 条目卡片：一句话结论 + 灰色标签 + 右侧删除。
    fn memory_list(&self, entries: &[&Entry], cx: &mut Context<Self>) -> Div {
        let scope = self.memory_form.scope;
        let mut card = div().v_flex().rounded_xl().border_1().border_color(rgb(LINE)).bg(rgb(CARD));
        for (i, entry) in entries.iter().enumerate() {
            let target = (*entry).clone();
            let tags = entry.tags.clone();
            card = card.child(
                div()
                    .h_flex()
                    .items_start()
                    .gap_2()
                    .px_3()
                    .pr_1p5()
                    .py_2p5()
                    .when(i > 0, |d| d.border_t_1().border_color(rgb(LINE)))
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1p5()
                            .child(
                                div()
                                    .text_size(TEXT_BODY)
                                    .line_height(LEADING)
                                    .text_color(rgb(INK))
                                    .child(entry.text.clone()),
                            )
                            .when(!tags.is_empty(), |d| {
                                d.child(div().h_flex().flex_wrap().gap_1().children(
                                    tags.into_iter().map(|tag| {
                                        div()
                                            .px_1p5()
                                            .rounded_sm()
                                            .bg(rgb(SAND))
                                            .text_size(TEXT_META)
                                            .text_color(rgb(INK_SOFT))
                                            .child(tag)
                                    }),
                                ))
                            }),
                    )
                    .child(
                        Self::quiet_button(("memory-remove", i), IconAll::X, Surface::Paper, cx)
                            .small()
                            .tooltip("删掉这条：不再带进对话")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_memory(scope, target.clone(), cx)
                            })),
                    ),
            );
        }
        card
    }

    /// 空状态：怎么让它记起来。
    fn memory_empty(&self, scope: Scope) -> Div {
        match scope {
            Scope::Project => how_to(
                "这个项目还没有记住的事",
                &[
                    "跟它说「记一下：构建只用 cargo test -p claudesktop-core」，它就把这句存进这个项目",
                    "项目约定、试过又否掉的方案、踩过的坑，它觉得以后还用得上会自己记一条",
                    "记下的都列在这里；点 × 删掉，下一轮对话就不再带上它了",
                ],
            ),
            Scope::Global => how_to(
                "还没有全局的记忆",
                &[
                    "全局的记忆在所有项目里都生效，比如「回复一律用中文」「别用 npm」",
                    "只跟某个仓库有关的事让它在「这个项目」里记，那样会跟着仓库一起进 git",
                    "这里删掉的条目立即失效：内核每轮都重读这个文件",
                ],
            ),
        }
    }
}
