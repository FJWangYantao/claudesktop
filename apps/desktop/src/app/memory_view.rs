//! 设置页「记忆」：内核跨会话记住的用户偏好、项目约定、做过的决定、踩过的坑。
//!
//! 分「这个项目 / 全局」两栏，上面一个搜索框，下面是条目卡片：编辑正文和标签、置顶（`pinned`）、
//! 跳到记下它的那一轮（「来源」）、删除；顶部是按模型提炼出来的「待确认」候选（收下才落盘）。
//! **文件是唯一真相**：这一页每次打开、每次内核记下一条都重读一次，不缓存到内存里长期用；
//! 删一条就是读 → 去掉那条 → 原子写回（`claudesktop_core::memory`），内核下一轮 `send_prompt`
//! 重读文件时就不带了。怎么进上下文（系统提示词那一节 + `<memory-context>`）见 `memory` 模块。
//!
//! 项目记忆在 `<项目根>/.claudesktop/memory.json`（跟 `AGENTS.md` 一样会进 git，团队共享），
//! 全局的在数据目录的 `memory.json`。三层东西怎么分工见「设计说明-记忆系统.md」第七节。

use super::ChatApp;
use super::catalog_view::{matches, no_match};
use super::settings_kit::{how_to, note, section, seg_control};
use super::widgets::Surface;
use crate::theme::rgb;
use crate::theme::*;
use claudesktop_core::MemoryCandidateView;
use claudesktop_core::memory::{self, Entry, Scope};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;

/// 设置页「记忆」分类的下标
pub(super) const MEMORY_TAB: usize = 7;

/// 条目攒到这个数就提示整理（设计说明第七节第 2 条）
const TIDY_HINT: usize = 30;

/// 「记忆」页的状态：看哪一栏、搜索词、读到的两份记忆、待确认的候选、正在编辑的那条。
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
    /// 内核把哪一条标成了「怎么记都不对」的提示（界面上闪一行红字）
    pub(super) error: Option<String>,
    /// 正在编辑的条目 id（`Some` = 那条的卡片换成编辑框）
    editing: Option<String>,
    /// 编辑框里的正文和标签（标签一行、逗号分隔）
    edit_text: Entity<TextareaState>,
    edit_tags: Entity<InputState>,
    /// 新增记忆的输入框（和编辑分开，免得两处的“保存”混起来）
    adding: bool,
    new_text: Entity<InputState>,
    new_tags: Entity<InputState>,
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
        let edit_text = cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 8));
        let edit_tags = cx.new(|cx| InputState::new(window, cx).placeholder("标签，用逗号隔开"));
        let new_text =
            cx.new(|cx| InputState::new(window, cx).placeholder("一句话结论，带一句为什么"));
        let new_tags = cx.new(|cx| InputState::new(window, cx).placeholder("标签，用逗号隔开"));
        Self {
            scope: Scope::Project,
            search,
            project: Vec::new(),
            global: Vec::new(),
            project_path: PathBuf::new(),
            global_path: None,
            warnings: Vec::new(),
            error: None,
            editing: None,
            edit_text,
            edit_tags,
            adding: false,
            new_text,
            new_tags,
        }
    }

    /// 这一栏的条目：置顶的在前，其余按最近改过 / 用过的
    fn entries(&self) -> Vec<&Entry> {
        let list = match self.scope {
            Scope::Project => &self.project,
            Scope::Global => &self.global,
        };
        let mut list: Vec<&Entry> = list.iter().collect();
        list.sort_by_key(|e| (!e.pinned, std::cmp::Reverse(e.updated_ms.max(e.used_ms))));
        list
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

    /// 编辑框里的标签：「构建, 测试」→ `["构建", "测试"]`。
    fn edited_tags(&self, cx: &App) -> Vec<String> {
        split_tags(&self.edit_tags.read(cx).value())
    }
}

/// 一句话里的标签拆开（中英文逗号、顿号、空格都算分隔）。
pub(super) fn split_tags(text: &str) -> Vec<String> {
    text.split([',', '，', '、', ' ', '\n', '\t'])
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .take(memory::MAX_TAGS)
        .collect()
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

    /// 这一栏记忆文件的位置。
    fn memory_path_of(&self, scope: Scope) -> Option<PathBuf> {
        match scope {
            Scope::Project => Some(self.memory_form.project_path.clone()),
            Scope::Global => self.memory_form.global_path.clone(),
        }
    }

    /// 改这两栏的文件：`OnBroken::Refuse`（读不了 / 格式坏了就不动它，
    /// 否则会把一张空表写回去，全部记忆都没了）。项目记忆进 git，改完交给检查点。
    fn edit_memory_file<R>(
        &mut self,
        scope: Scope,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut Vec<Entry>) -> R,
    ) -> Option<R> {
        let path = self.memory_path_of(scope)?;
        let idx = self.active;
        self.edit_memory_at(scope, path, idx, cx, change)
    }

    /// 同 [`Self::edit_memory_file`]，但文件和记检查点的会话由调用方指定
    /// （收下候选时写进提出它的那个会话的项目，不是设置页当前显示的那个）。
    fn edit_memory_at<R>(
        &mut self,
        scope: Scope,
        path: PathBuf,
        idx: usize,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut Vec<Entry>) -> R,
    ) -> Option<R> {
        let snapshot = |path: &PathBuf| {
            (scope == Scope::Project)
                .then(|| claudesktop_core::checkpoint::read_snapshot(path))
                .flatten()
        };
        let before = snapshot(&path);
        let result = memory::update(&path, memory::OnBroken::Refuse, |entries| {
            let value = change(entries);
            (value, entries.len())
        });
        if let Some(before) = before
            && let Some(after) = snapshot(&path)
            && before != after
        {
            self.record_file_change(idx, path.to_string_lossy().into_owned(), before, after);
        }
        match result {
            Ok((value, notice)) => {
                if let Some(notice) = notice {
                    self.toast(Notification::warning(notice), cx);
                }
                self.reload_memory_view();
                cx.notify();
                Some(value)
            }
            Err(err) => {
                self.memory_form.error = Some(err.clone());
                self.toast(Notification::error(err), cx);
                cx.notify();
                None
            }
        }
    }

    /// 删一条记忆：读 → 去掉这条 → 原子写回（和内核记新的一条排队，不会互相覆盖）。
    /// 立即生效（内核每轮重读文件）。按 `id` 删：页面打开后内核可能更新过这条。
    fn remove_memory(&mut self, scope: Scope, entry: Entry, cx: &mut Context<Self>) {
        self.memory_form.editing = None;
        self.edit_memory_file(scope, cx, |entries| {
            entries.retain(|e| if entry.id.is_empty() { e != &entry } else { e.id != entry.id })
        });
    }

    /// 置顶 / 取消置顶：置顶的永远带进上下文，也永远不会被淘汰。
    fn pin_memory(&mut self, scope: Scope, entry: Entry, pinned: bool, cx: &mut Context<Self>) {
        self.edit_memory_file(scope, cx, |entries| {
            if let Some(e) = entries.iter_mut().find(|e| e.id == entry.id) {
                e.pinned = pinned;
                e.updated_ms = memory::now_ms();
            }
        });
    }

    /// 点「编辑」：把这条放进编辑框（正文 + 标签）。
    fn begin_edit_memory(&mut self, entry: Entry, window: &mut Window, cx: &mut Context<Self>) {
        let form = &mut self.memory_form;
        form.editing = Some(entry.id.clone());
        form.adding = false;
        let text = entry.text.clone();
        let tags = entry.tags.join(", ");
        form.edit_text.update(cx, |state, cx| state.set_value(text, window, cx));
        form.edit_tags.update(cx, |state, cx| state.set_value(tags, window, cx));
        cx.notify();
    }

    /// 保存编辑：改正文、标签、置顶都走这里（`updated_ms` 换新的）。
    fn save_edit_memory(&mut self, scope: Scope, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.memory_form.editing.clone();
        let Some(id) = id else {
            return;
        };
        let text = self.memory_form.edit_text.read(cx).value().trim().to_string();
        if text.is_empty() {
            self.toast(Notification::error("正文不能空着"), cx);
            return;
        }
        let tags = self.memory_form.edited_tags(cx);
        let pinned =
            self.memory_form.entries().iter().find(|e| e.id == id).is_some_and(|e| e.pinned);
        self.edit_memory_file(scope, cx, |entries| {
            memory::edit(entries, &id, &text, tags, pinned, memory::now_ms())
        });
        self.memory_form.editing = None;
        // 焦点回设置页的焦点锚（编辑框已经不画了）
        self.settings_focus.focus(window, cx);
        cx.notify();
    }

    /// 新增一条（用户在设置页自己写）：和内核记下的是一条路，只是没有来源。
    fn add_memory(&mut self, scope: Scope, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.memory_form.new_text.read(cx).value().trim().to_string();
        if text.is_empty() {
            self.toast(Notification::error("写一句要记住的话"), cx);
            return;
        }
        let tags = split_tags(&self.memory_form.new_tags.read(cx).value());
        self.edit_memory_file(scope, cx, |entries| {
            let mut entry = Entry::new(text.clone(), memory::now_ms());
            entry.tags = tags.clone();
            memory::upsert(entries, entry);
            memory::enforce_limits(entries, scope.max_entries());
        });
        self.memory_form.adding = false;
        self.memory_form.new_text.update(cx, |state, cx| state.set_value("", window, cx));
        self.memory_form.new_tags.update(cx, |state, cx| state.set_value("", window, cx));
        self.settings_focus.focus(window, cx);
        cx.notify();
    }

    /// 「来源」：跳到记下这条记忆的那个会话的那一轮（会话可能不在内存里，按 ID 恢复）。
    fn reveal_memory_source(&mut self, entry: &Entry, cx: &mut Context<Self>) {
        let Some(source) = entry.source.clone() else {
            return;
        };
        let hit = super::search::HistoryHitState {
            session: source.session.clone(),
            // `turn` 从 1 数，界面上定位用的是从 0 数的下标
            turn: source.turn.saturating_sub(1) as usize,
            from_user: true,
            snippet: entry.text.clone(),
            title: entry.text.clone(),
        };
        self.settings_open = false;
        self.reveal_history_hit(&hit, cx);
    }

    /// 用资源管理器打开记忆文件（没建过就打开它所在的文件夹）。
    fn reveal_memory(&self, scope: Scope, cx: &mut Context<Self>) {
        let Some(path) = self.memory_path_of(scope) else {
            return;
        };
        let target =
            if path.exists() { path } else { path.parent().map(PathBuf::from).unwrap_or(path) };
        cx.reveal_path(&target);
    }

    pub(super) fn render_memory(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let form = &self.memory_form;
        let scope = form.scope;
        // 两栏切换 + 搜索框 + 新增
        let add_label = if form.adding { "取消" } else { "新增" };
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
                    this.memory_form.adding = false;
                    this.memory_form.editing = None;
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
            )
            .child(Self::text_button("memory-add", add_label, false).small().on_click(
                cx.listener(|this, _, window, cx| {
                    this.memory_form.adding = !this.memory_form.adding;
                    this.memory_form.editing = None;
                    if !this.memory_form.adding {
                        this.memory_form
                            .new_text
                            .update(cx, |state, cx| state.set_value("", window, cx));
                        this.memory_form
                            .new_tags
                            .update(cx, |state, cx| state.set_value("", window, cx));
                    }
                    cx.notify();
                }),
            ));
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
                d.child(note(IconAll::Info, "跟着仓库进 git；私人偏好记到「全局」"))
            })
            .child(path_line)
            .when(total >= TIDY_HINT, |d| {
                d.child(note(
                    IconAll::Lightbulb,
                    format!(
                        "已经 {total} 条了：构建、规范这类该进 AGENTS.md（用 edit_file 写进去，再在这里删掉）",
                    ),
                ))
            });
        let warnings = form.warnings.clone();
        let error = form.error.clone();
        let adding = form.adding;
        let actions = div()
            .h_flex()
            .gap_1p5()
            .child(
                Self::text_button("memory-reveal", "打开文件位置", false)
                    .small()
                    .on_click(cx.listener(move |this, _, _, cx| this.reveal_memory(scope, cx))),
            )
            .into_any_element();
        let add_panel = adding.then(|| {
            let new_text = self.memory_form.new_text.clone();
            let new_tags = self.memory_form.new_tags.clone();
            div()
                .v_flex()
                .gap_2()
                .p_3()
                .rounded_xl()
                .border_1()
                .border_color(rgb(LINE))
                .bg(rgb(CARD))
                .child(div().child(Input::new(&new_text).cleanable(true)))
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .child(div().flex_1().min_w_0().child(Input::new(&new_tags)))
                        .child(
                            Self::text_button("memory-add-save", "记下", true).small().on_click(
                                cx.listener(move |this, _, window, cx| {
                                    this.add_memory(scope, window, cx);
                                }),
                            ),
                        ),
                )
        });
        section(
            "记住的事",
            "跨会话记住的偏好和约定：每轮自动带上相关的，置顶的永远带上",
            Some(actions),
            div()
                .v_flex()
                .gap_4()
                .children(self.render_memory_candidates(cx))
                .child(bar)
                .when_some(add_panel, |d, panel| d.child(panel))
                .child(body)
                .child(footer)
                .when_some(error, |d, err| {
                    d.child(
                        div()
                            .text_size(TEXT_SMALL)
                            .line_height(LEADING)
                            .text_color(rgb(RED))
                            .child(err),
                    )
                })
                .children(warnings.into_iter().map(|w| {
                    div().text_size(TEXT_SMALL).line_height(LEADING).text_color(rgb(RED)).child(w)
                })),
        )
    }

    /// 条目卡片：一句话结论 + 灰色标签 +（置顶 / 编辑 / 来源 / 删除）。
    fn memory_list(&self, entries: &[&Entry], cx: &mut Context<Self>) -> Div {
        let scope = self.memory_form.scope;
        let mut card = div().v_flex().rounded_xl().border_1().border_color(rgb(LINE)).bg(rgb(CARD));
        for (i, entry) in entries.iter().enumerate() {
            let target = (*entry).clone();
            let tags = entry.tags.clone();
            let pinned = entry.pinned;
            let source = entry.source.clone();
            let editing = self.memory_form.editing.as_deref() == Some(entry.id.as_str());
            let row = div()
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
                        .when(editing, |d| {
                            let text = self.memory_form.edit_text.clone();
                            let tags = self.memory_form.edit_tags.clone();
                            d.child(
                                div()
                                    .v_flex()
                                    .gap_2()
                                    .child(Textarea::new(&text).appearance(false))
                                    .child(
                                        div()
                                            .h_flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .child(Input::new(&tags).cleanable(true)),
                                            )
                                            .child(
                                                Self::text_button("memory-edit-save", "保存", true)
                                                    .small()
                                                    .on_click(cx.listener(
                                                        |this, _, window, cx| {
                                                            this.save_edit_memory(
                                                                scope, window, cx,
                                                            );
                                                        },
                                                    )),
                                            )
                                            .child(
                                                Self::text_button(
                                                    "memory-edit-cancel",
                                                    "取消",
                                                    false,
                                                )
                                                .small()
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.memory_form.editing = None;
                                                    this.settings_focus.focus(window, cx);
                                                    cx.notify();
                                                })),
                                            ),
                                    ),
                            )
                        })
                        .when(!editing, |d| {
                            d.child(
                                div()
                                    .h_flex()
                                    .items_start()
                                    .gap_1p5()
                                    .when(pinned, |d| {
                                        d.child(
                                            Icon::new(IconAll::Pin)
                                                .size_3p5()
                                                .mt_1()
                                                .flex_shrink_0()
                                                .text_color(rgb(CORAL)),
                                        )
                                    })
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_size(TEXT_BODY)
                                            .line_height(LEADING)
                                            .text_color(rgb(INK))
                                            .child(entry.text.clone()),
                                    ),
                            )
                        })
                        .when(!editing && !tags.is_empty(), |d| {
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
                        })
                        .when(!editing && source.is_some(), |d| {
                            let label = source
                                .as_ref()
                                .map(|s| format!("来源：第 {} 轮", s.turn))
                                .unwrap_or_default();
                            d.child(
                                div()
                                    .id(("memory-source", i))
                                    .h_flex()
                                    .items_center()
                                    .gap_1()
                                    .cursor_pointer()
                                    .text_size(TEXT_META)
                                    .text_color(rgb(INK_SOFT))
                                    .hover(|d| d.text_color(rgb(INK)))
                                    .on_click(cx.listener({
                                        let entry = target.clone();
                                        move |this, _, _window, cx| {
                                            this.reveal_memory_source(&entry, cx)
                                        }
                                    }))
                                    .child(Icon::new(IconAll::CornerDownRight).size_3())
                                    .child(label),
                            )
                        }),
                )
                .when(!editing, |d| {
                    d.child(
                        Self::quiet_button(
                            ("memory-pin", i),
                            if pinned { IconAll::PinOff } else { IconAll::Pin },
                            Surface::Paper,
                            cx,
                        )
                        .small()
                        .tooltip(if pinned {
                            "取消置顶：按最近用过的挑着带"
                        } else {
                            "置顶：永远带进对话，也不会被淘汰"
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.pin_memory(scope, target.clone(), !pinned, cx)
                        })),
                    )
                    .child(
                        Self::quiet_button(("memory-edit", i), IconAll::Pencil, Surface::Paper, cx)
                            .small()
                            .tooltip("改这句话或它的标签")
                            .on_click(cx.listener({
                                let entry = target.clone();
                                move |this, _, window, cx| {
                                    this.begin_edit_memory(entry.clone(), window, cx)
                                }
                            })),
                    )
                    .child(
                        Self::quiet_button(("memory-remove", i), IconAll::X, Surface::Paper, cx)
                            .small()
                            .tooltip("删掉这条：不再带进对话")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_memory(scope, target.clone(), cx)
                            })),
                    )
                });
            card = card.child(row);
        }
        card
    }

    /// 空状态：怎么让它记起来。
    fn memory_empty(&self, scope: Scope) -> Div {
        match scope {
            Scope::Project => how_to(
                "这个项目还没有记住的事",
                &[
                    "跟它说「记一下：……」，它就存进这个项目",
                    "约定、踩过的坑，它觉得有用也会自己记",
                    "点 × 删掉，下一轮就不再带上",
                ],
            ),
            Scope::Global => how_to(
                "还没有全局的记忆",
                &[
                    "在所有项目里生效，比如「回复一律用中文」",
                    "只跟某个仓库有关的，记在「这个项目」",
                    "删掉立即失效",
                ],
            ),
        }
    }
}

/// 一条待确认的候选：提炼出来的那句话 + 它是哪个会话、哪一轮提炼出来的。
///
/// 来源跟着条目走（而不是整批一个）：待确认区可能同时挂着几个会话的候选，
/// 收下早先那条时不能把来源记成最后来的那个会话。
#[derive(Debug, Clone)]
pub(super) struct PendingCandidate {
    pub(super) text: String,
    pub(super) tags: Vec<String>,
    /// 同一个键再来一次就不重复挂
    pub(super) key: String,
    /// 来源会话（`SessionMeta::id`）；空 = 不知道（这时不记来源）
    session: String,
    /// 来源里的第几轮
    turn: u64,
}

impl From<(MemoryCandidateView, String, u64)> for PendingCandidate {
    fn from((view, session, turn): (MemoryCandidateView, String, u64)) -> Self {
        Self { text: view.text, tags: view.tags, key: view.key, session, turn }
    }
}

/// 待确认的候选（第 3 期）：内核一轮结束后提炼出来的，收下才落盘。
#[derive(Debug, Clone, Default)]
pub(super) struct MemoryCandidates {
    /// 按来的顺序，新的在前；`key` 相同的只留一条
    items: Vec<PendingCandidate>,
}

impl MemoryCandidates {
    /// 收下一批候选（内核发来 `MemorySuggested` 时调）：和待确认区里已有的去重。
    pub(super) fn push(&mut self, candidates: Vec<MemoryCandidateView>, session: &str, turn: u64) {
        for candidate in candidates {
            if candidate.key.is_empty() {
                continue;
            }
            if self.items.iter().any(|c| c.key == candidate.key) {
                continue;
            }
            self.items.insert(0, (candidate, session.to_string(), turn).into());
        }
        self.items.truncate(MAX_PENDING);
    }

    /// 第 `index` 条（界面上按顺序点的「收下 / 不要」）。
    pub(super) fn get(&self, index: usize) -> Option<&PendingCandidate> {
        self.items.get(index)
    }

    /// 待确认区里的那些（界面上逐条画）。
    pub(super) fn all(&self) -> &[PendingCandidate] {
        &self.items
    }

    /// 删掉 key 对得上的那条。
    pub(super) fn remove(&mut self, key: &str) {
        self.items.retain(|c| c.key != key);
    }

    /// 删掉第 `index` 条。
    pub(super) fn remove_at(&mut self, index: usize) {
        if index < self.items.len() {
            self.items.remove(index);
        }
    }
}

/// 待确认区最多挂几条（再多就把最早的挤掉：它们本来也只是候选）
pub(super) const MAX_PENDING: usize = 20;

impl ChatApp {
    /// 收下一条候选：写进**项目**记忆（候选多半是项目里的观察；要全局就在设置页改到全局）。
    /// 写盘走和内核同一条路（`memory::update` + `upsert`），来源记成提炼它的那个会话那一轮。
    pub(super) fn accept_candidate(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(candidate) = self.memory_candidates.get(index).cloned() else {
            return;
        };
        let scope = Scope::Project;
        // 写进提出它的那个会话的项目：待确认区跨会话共用，收下时设置页显示的可能是别的项目。
        // 来源会话已经关掉了才退回当前显示的那份
        let source = self
            .sessions
            .iter()
            .position(|s| s.acp_id.as_deref() == Some(candidate.session.as_str()));
        let (path, idx) = match source {
            Some(i) => (memory::project_file_or(&self.sessions[i].cwd), i),
            None => (self.memory_form.project_path.clone(), self.active),
        };
        self.edit_memory_at(scope, path, idx, cx, |entries| {
            let mut entry = Entry::new(candidate.text.clone(), memory::now_ms());
            entry.tags = candidate.tags.clone();
            if !candidate.session.is_empty() {
                entry.source = Some(memory::Source {
                    session: candidate.session.clone(),
                    turn: candidate.turn,
                });
            }
            memory::upsert(entries, entry);
            memory::enforce_limits(entries, scope.max_entries());
        });
        self.memory_candidates.remove(&candidate.key);
        cx.notify();
    }

    /// 不要这条候选。
    pub(super) fn dismiss_candidate(&mut self, index: usize, cx: &mut Context<Self>) {
        self.memory_candidates.remove_at(index);
        cx.notify();
    }

    /// 待确认区（设置页「记忆」顶部）：一条候选一行，「收下 / 不要」。
    pub(super) fn render_memory_candidates(&self, cx: &mut Context<Self>) -> Option<Div> {
        if self.memory_candidates.all().is_empty() {
            return None;
        }
        let mut card = div().v_flex().rounded_xl().border_1().border_color(rgb(LINE)).bg(rgb(CARD));
        for (i, candidate) in self.memory_candidates.all().iter().enumerate() {
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
                        Icon::new(IconAll::Sparkles)
                            .size_3p5()
                            .mt_1()
                            .flex_shrink_0()
                            .text_color(rgb(CORAL)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(TEXT_BODY)
                            .line_height(LEADING)
                            .text_color(rgb(INK))
                            .child(candidate.text.clone()),
                    )
                    .child(
                        Self::text_button(("memory-accept", i), "收下", false).small().on_click(
                            cx.listener(move |this, _, _, cx| this.accept_candidate(i, cx)),
                        ),
                    )
                    .child(
                        Self::quiet_button(("memory-dismiss", i), IconAll::X, Surface::Paper, cx)
                            .small()
                            .tooltip("不要这条")
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.dismiss_candidate(i, cx)),
                            ),
                    ),
            );
        }
        Some(section(
            "待确认",
            "对话里看着值得记下的话；收下才写进记忆，不要的直接丢掉",
            None,
            div().v_flex().gap_2().child(card).child(
                div().text_size(TEXT_META).text_color(rgb(INK_SOFT)).child(
                    "收下写进「这个项目」（要全局就收下后改到全局）。不想每次都被问，\
                         在「内核与模型」里关掉「回合结束后提炼记忆」",
                ),
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(text: &str) -> MemoryCandidateView {
        MemoryCandidateView { text: text.into(), tags: Vec::new(), key: text.into() }
    }

    /// 来源跟着条目走：两个会话的候选挂在同一张表上，收下早先那条时不能记成后来的会话
    #[core::prelude::v1::test]
    fn candidates_keep_their_own_source() {
        let mut list = MemoryCandidates::default();
        list.push(vec![view("甲")], "会话一", 1);
        list.push(vec![view("乙")], "会话二", 3);
        // 新的在前
        assert_eq!(list.all()[0].text, "乙");
        let first = list.get(1).unwrap();
        assert_eq!((first.session.as_str(), first.turn), ("会话一", 1));
        let second = list.get(0).unwrap();
        assert_eq!((second.session.as_str(), second.turn), ("会话二", 3));
        // 同一个 key 再来一次：不重复挂
        list.push(vec![view("甲")], "会话三", 9);
        assert_eq!(list.all().len(), 2);
        assert_eq!(list.get(1).unwrap().session, "会话一");
        // 收下 / 不要都是按 key、按下标删
        list.remove("甲");
        assert_eq!(list.all().len(), 1);
        assert_eq!(list.all()[0].text, "乙");
        list.remove_at(0);
        assert!(list.all().is_empty());
    }

    /// 标签拆分：中英文逗号、顿号、空格都算分隔，截到上限
    #[core::prelude::v1::test]
    fn tags_split_on_any_separator() {
        assert_eq!(split_tags("构建, 测试、格式"), ["构建", "测试", "格式"]);
        assert!(split_tags("  ").is_empty());
        let many: String = (0..memory::MAX_TAGS + 3).map(|i| format!("t{i} ")).collect();
        assert_eq!(split_tags(&many).len(), memory::MAX_TAGS);
    }
}
