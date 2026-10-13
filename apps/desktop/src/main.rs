//! M0 冒烟窗口：验证 gpui-kit 工具链 + Windows IME。
//! 下一步将加入文本输入框做中文输入双测（微软拼音 + 搜狗）。

use gpui_kit::component::button::*;
use gpui_kit::component::*;
use gpui_kit::*;

pub struct HelloWorld;

impl Render for HelloWorld {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .v_flex()
            .gap_2()
            .size_full()
            .items_center()
            .justify_center()
            .child("你好，Claude Desktop Harness（M0 冒烟窗口）")
            .child(
                Button::new("ok")
                    .primary()
                    .label("Let's Go!")
                    .on_click(|_, _, _| println!("Clicked!")),
            )
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            // 必须先于任何 gpui-kit 组件特性调用。
            gpui_kit::init(cx);

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1100.), px(700.)),
                    cx,
                ))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Claude Desktop Harness · M0".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };

            gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| HelloWorld))
                .expect("Failed to open window");
        });
}
