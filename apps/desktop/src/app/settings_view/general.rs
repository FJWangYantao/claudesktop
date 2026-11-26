//! 「通用」一栏的绘制：外观、字体、动效、设置文件入口。

use super::*;

impl ChatApp {
    /// 「通用」分类：外观、字号、字体、侧栏、通知（都即时生效，不走「保存」），以及设置文件在哪。
    pub(super) fn render_general(&self, cx: &mut Context<Self>) -> Div {
        let motion = self.settings.motion;
        let motion_control = seg_control(
            "motion",
            MotionPref::ALL.map(|m| (m, m.label())),
            motion,
            |this, m, _, cx| this.set_motion_pref(m, cx),
            cx,
        );
        let motion_row = setting_row("动画", motion_control);
        let appearance = setting_group([
            setting_row(
                "明暗",
                seg_control(
                    "appearance",
                    Appearance::ALL.map(|a| (a, a.label())),
                    self.settings.appearance,
                    |this, a, window, cx| this.set_appearance(a, window, cx),
                    cx,
                ),
            ),
            self.theme_row(cx),
            self.code_theme_row(cx),
            setting_row(
                "主区对齐",
                seg_control(
                    "content-align",
                    ContentAlign::ALL.map(|a| (a, a.label())),
                    self.settings.content_align,
                    |this, align, _, cx| {
                        this.save_ui_pref(move |s| s.content_align = align, cx);
                        cx.notify();
                    },
                    cx,
                ),
            ),
            setting_row(
                "宽阅读列",
                Switch::new("wide-reading")
                    .checked(self.settings.wide_reading)
                    .color(rgb(CORAL))
                    .on_change(cx.listener(|this, checked, _, cx| {
                        let wide = *checked;
                        this.save_ui_pref(move |s| s.wide_reading = wide, cx);
                        cx.notify();
                    })),
            ),
            setting_row(
                "对话视图",
                seg_control(
                    "view-mode",
                    ViewMode::ALL.map(|m| (m, m.label())),
                    self.settings.view_mode,
                    |this, mode, _, cx| this.set_view_mode(mode, cx),
                    cx,
                ),
            ),
            motion_row,
            self.auto_load_images_row(cx),
            self.font_scale_row(),
            self.font_family_row(cx),
        ]);
        let sidebar = setting_group([
            setting_row(
                "会话管理",
                seg_control(
                    "sidebar-grouping",
                    SidebarGrouping::ALL.map(|g| (g, g.label())),
                    self.settings.sidebar_grouping(),
                    |this, grouping, _, cx| this.set_sidebar_grouping(grouping, cx),
                    cx,
                ),
            ),
            setting_row(
                "显示内核状态",
                Switch::new("sidebar-kernel-status")
                    .checked(self.settings.sidebar_kernel_status)
                    .color(rgb(CORAL))
                    .on_change(cx.listener(|this, checked, _, cx| {
                        this.set_sidebar_kernel_status(*checked, cx)
                    })),
            ),
        ]);
        let reminders = setting_group([setting_row(
            "系统通知",
            Switch::new("notifications")
                .checked(self.settings.notifications)
                .color(rgb(CORAL))
                .on_change(cx.listener(|this, checked, _, cx| {
                    let on = *checked;
                    this.save_ui_pref(move |s| s.notifications = on, cx);
                    cx.notify();
                })),
        )]);
        div()
            .v_flex()
            .gap_5()
            .child(section("外观", None, appearance))
            .child(section("侧栏", None, sidebar))
            .child(section("提醒", None, reminders))
            .child(section("数据", None, setting_group([self.settings_file_row(cx)])))
    }

    /// 设置文件在哪：路径 + 「打开文件夹」。
    pub(super) fn settings_file_row(&self, cx: &mut Context<Self>) -> Div {
        let path = self.settings_path.clone();
        let location = match &path {
            Some(p) => div().font_family(MONO).text_size(TEXT_META).child(p.display().to_string()),
            None => div().text_color(rgb(RED)).child("数据目录不可用，设置不会保存"),
        };
        setting_row_with(
            "设置文件",
            location,
            div().when_some(path, |d, path| {
                d.child(
                    Self::text_button("settings-reveal", "打开文件夹", false)
                        .small()
                        .on_click(cx.listener(move |_, _, _, cx| cx.reveal_path(&path))),
                )
            }),
        )
    }

    /// 「自动加载回复里的网络图片」：默认关，关着时远程图片先摆占位卡等用户点「加载」。
    pub(super) fn auto_load_images_row(&self, cx: &mut Context<Self>) -> Div {
        setting_row(
            "自动加载回复里的网络图片",
            Switch::new("auto-load-images")
                .checked(self.settings.auto_load_images)
                .color(rgb(CORAL))
                .on_change(cx.listener(|this, checked, _, cx| {
                    let on = *checked;
                    crate::app::remote_image::set_auto_load(on);
                    this.save_ui_pref(move |s| s.auto_load_images = on, cx);
                    cx.notify();
                })),
        )
    }

    /// 字号滑杆：右边带当前百分比。事件订阅在 `ChatApp::new`（`SliderState` 自己 emit），
    /// 拖动与松手都即时生效。
    pub(super) fn font_scale_row(&self) -> Div {
        let percent = (self.settings.font_scale * 100.).round();
        let slider = div()
            .h_flex()
            .items_center()
            .gap_3()
            .w(rems(15.))
            .child(div().flex_1().min_w_0().child(Slider::new(&self.font_slider)))
            .child(
                // 宽度用 rem：字号放大后「140%」也不会折成两行
                div()
                    .w(rems(3.))
                    .flex_shrink_0()
                    .whitespace_nowrap()
                    .text_right()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK))
                    .font_features(tabular_nums())
                    .child(format!("{percent:.0}%")),
            );
        setting_row("界面字号", slider)
    }

    /// 配色下拉：十套色板，每项下面一行色块预览。换配色即时生效。
    pub(super) fn theme_row(&self, cx: &mut Context<Self>) -> Div {
        let app = cx.entity();
        setting_row(
            "配色",
            menu_popover(
                "theme-menu",
                Anchor::BottomRight,
                rems(17.5),
                &app,
                &self.menu_focus,
                MenuChip::new(IconAll::Palette, self.settings.theme.label()),
                theme_menu_items,
            ),
        )
    }

    /// 代码块语法高亮配色下拉：跟随界面配色 + 多套独立配色，每项带语法色块预览。
    pub(super) fn code_theme_row(&self, cx: &mut Context<Self>) -> Div {
        let app = cx.entity();
        setting_row(
            "代码配色",
            menu_popover(
                "code-theme-menu",
                Anchor::BottomRight,
                rems(18.5),
                &app,
                &self.menu_focus,
                MenuChip::new(IconAll::CodeXml, self.settings.code_theme.label()),
                code_theme_menu_items,
            ),
        )
    }

    /// 字体族下拉：内嵌的 Inter + 系统已装字体（下拉里可搜索）。
    pub(super) fn font_family_row(&self, cx: &mut Context<Self>) -> Div {
        let app = cx.entity();
        let label: SharedString =
            self.settings.font_family_or_default().unwrap_or_else(|| "Inter（内嵌）".into());
        setting_row(
            "界面字体",
            menu_popover(
                "font-family-menu",
                Anchor::BottomRight,
                rems(17.5),
                &app,
                &self.menu_focus,
                MenuChip::new(IconAll::ALargeSmall, label),
                font_family_menu_items,
            ),
        )
    }
}
/// 配色下拉的条目：名称 + 灰色说明，下一行是这套配色在当前明暗下的色块，当前项打勾。
pub(super) fn theme_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> MenuList {
    let current = app.settings.theme;
    // 九套色板 × 三行（名称 / 说明 / 色块）比原来高，限高滚动，矮窗口里不至于顶出屏幕
    let mut menu = MenuList::new("theme-list", handle, popover).scroll(rems(26.));
    for t in ThemeChoice::ALL {
        menu = menu.pick(
            theme_menu_item(
                ("theme-item", t.index()),
                t.label(),
                t.desc(),
                t.index(),
                t == current,
            ),
            move |this, cx| this.set_theme(t, cx),
        );
    }
    menu
}

/// 代码配色下拉的条目：名称 + 说明 + 5 个语法色块，当前项打勾。
pub(super) fn code_theme_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> MenuList {
    let current = app.settings.code_theme;
    let ui_palette = app.settings.theme.index();
    let dark = crate::theme::is_dark();
    let mut menu = MenuList::new("code-theme-list", handle, popover).scroll(rems(26.));
    for c in CodeThemeChoice::ALL {
        menu = menu.pick(
            code_theme_menu_item(
                ("code-theme-item", c.index()),
                c.label(),
                c.desc(),
                crate::syntax::swatches(c, ui_palette, dark),
                c == current,
            ),
            move |this, cx| this.set_code_theme(c, cx),
        );
    }
    menu
}

/// 界面字体下拉的条目：第一项是内嵌的 Inter，后面跟系统已装字体（可滚动，最多列一批）。
/// 每项用该字体本身排一行预览（`font_menu_item`），当前项打勾；
/// 列不完的靠「还有 N 款」提示，需要的话在设置文件里手写字体名。
pub(super) fn font_family_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> MenuList {
    const MAX_SYSTEM_FONTS: usize = 60;
    let current = app.settings.font_family_or_default();
    let mut menu = MenuList::new("font-family-list", handle, popover).scroll(rems(22.5));

    // 内嵌的 Inter：字体族为空即表示用它
    menu = menu.pick(
        font_menu_item(
            "font-embedded",
            "Inter（内嵌）",
            DEFAULT_SANS,
            FONT_PREVIEW,
            current.is_none(),
        ),
        |this, cx| this.set_font_family(None, cx),
    );

    let fonts = app.system_fonts();
    for (i, name) in fonts.iter().take(MAX_SYSTEM_FONTS).enumerate() {
        let active = current.as_deref() == Some(name.as_str());
        let picked: SharedString = name.clone().into();
        menu = menu.pick(
            font_menu_item(("font-item", i), name.clone(), name.clone(), FONT_PREVIEW, active),
            move |this, cx| this.set_font_family(Some(picked.clone()), cx),
        );
    }
    if fonts.len() > MAX_SYSTEM_FONTS {
        menu = menu.child(
            div()
                .px_2p5()
                .py_1p5()
                .text_size(TEXT_SMALL)
                .text_color(rgb(INK_SOFT))
                .child(format!("还有 {} 款未列出", fonts.len() - MAX_SYSTEM_FONTS)),
        );
    }
    menu
}
