//! 渲染探针 2：逐步逼近 app 中不渲染的结构。
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::*;
use gpui_kit::*;

const CORAL: u32 = 0xD97757;
const INK: u32 = 0x3D3929;
const INK_SOFT: u32 = 0x87847C;
const LINE: u32 = 0xE5E1D5;
const CARD: u32 = 0xFFFFFF;
const BLUE: u32 = 0x5B6FA8;
const BLUE_TINT: u32 = 0xE9EEF7;
const SAND: u32 = 0xF0EEE6;

struct Probe;

impl Probe {
    fn top_bar() -> Div {
        div()
            .h_flex()
            .items_center()
            .gap_2_5()
            .px_5()
            .py_2_5()
            .border_b_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .size(px(18.))
                            .rounded_md()
                            .bg(rgb(0xC2694E))
                            .flex()
                            .justify_center()
                            .items_center()
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(rgb(0xFFFFFF))
                                    .child("甲"),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(13.5))
                            .text_color(rgb(INK))
                            .child("帮我配置一个有图形界面的动态壁纸软件"),
                    ),
            )
            .child(div().flex_1())
            .child(
                div()
                    .h_flex()
                    .gap_1_5()
                    .px_2_5()
                    .py_1()
                    .rounded_full()
                    .bg(rgb(BLUE_TINT))
                    .child(
                        Icon::new(IconAll::Folder)
                            .size_3_5()
                            .text_color(rgb(BLUE)),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(rgb(BLUE))
                            .child("claudesktop"),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .gap_1()
                    .px_2_5()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(CARD))
                    .child(
                        Icon::new(IconAll::Bot)
                            .size_3_5()
                            .text_color(rgb(INK_SOFT)),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(INK))
                            .child("GLM-5.3"),
                    )
                    .child(
                        Icon::new(IconAll::ChevronDown)
                            .size_3()
                            .text_color(rgb(INK_SOFT)),
                    ),
            )
    }
}

impl Render for Probe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .v_flex()
            .size_full()
            .bg(rgb(0xFAF9F5))
            .text_color(rgb(INK))
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(div().text_size(px(13.)).child("顶栏文字"))
                    .child(div().flex_1())
                    .child(
                        div()
                            .px_2_5()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(LINE))
                            .bg(rgb(CARD))
                            .child(div().text_size(px(12.)).child("GLM-5.3")),
                    ),
            )
            .child(
                div()
                    .p_8()
                    .v_flex()
                    .gap_4()
                    .child(div().size(px(60.)).rounded_full().bg(rgb(CORAL)))
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().size(px(9.)).rounded_full().bg(rgb(CORAL)))
                            .child(div().text_size(px(12.5)).child("claudesktop")),
                    )
                    .child(
                        Icon::new(IconAll::Sparkles)
                            .size(px(48.))
                            .text_color(rgb(CORAL)),
                    )
                    .child(
                        Icon::new(IconAll::Folder)
                            .size(px(48.))
                            .text_color(rgb(INK)),
                    ),
            )
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            let mut options = WindowOptions::default();
            options.window_bounds = Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(900.), px(500.)),
                cx,
            )));
            gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| Probe))
                .expect("open");
            cx.activate(true);
        });
}
