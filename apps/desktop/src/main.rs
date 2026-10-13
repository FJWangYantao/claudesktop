//! M0 冒烟窗口：验证 gpui-kit 工具链 + Windows IME + 中文渲染。
//!
//! 人工测试点（微软拼音 / 搜狗各测一遍）：
//! 1. 候选框是否跟随光标位置
//! 2. 组词过程中按 Enter / Backspace 是否正确交给 IME（而非被窗口截获）
//! 3. 中文是否正常渲染（无豆腐块）
//! 4. 每次输入触发 Change -> echo 标签实时更新（状态 + 重渲染链路）

use gpui_kit::component::button::*;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::label::*;
use gpui_kit::component::*;
use gpui_kit::*;

pub struct SmokeWindow {
    input: Entity<InputState>,
    echo: SharedString,
}

impl SmokeWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("在这里输入中文，测试 IME（微软拼音 / 搜狗）…")
        });

        cx.subscribe(&input, |this, entity, event: &InputEvent, cx| match event {
            InputEvent::Change => {
                let value = entity.read(cx).value();
                let count = value.chars().count();
                this.echo = if count == 0 {
                    "（等待输入）".into()
                } else {
                    format!("已输入 {count} 字：{value}").into()
                };
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {
                let value = entity.read(cx).value();
                this.echo = format!("已提交：{value}").into();
                cx.notify();
            }
            _ => {}
        })
        .detach();

        Self {
            input,
            echo: "（等待输入）".into(),
        }
    }
}

impl Render for SmokeWindow {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let send_input = self.input.clone();
        div()
            .v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .child(
                div()
                    .v_flex()
                    .gap_4()
                    .w(px(620.))
                    .child(Label::new("Claude Desktop Harness · M0 冒烟窗口"))
                    .child(Label::new(
                        "测试点：候选框跟随光标 / 组词时 Enter·Backspace / 中文渲染 / 输入回显",
                    ))
                    .child(Input::new(&self.input))
                    .child(Label::new(self.echo.clone()))
                    .child(
                        Button::new("send")
                            .primary()
                            .label("发送")
                            .on_click(move |_, _, cx| {
                                let value = send_input.read(cx).value();
                                println!("[send] {value}");
                            }),
                    ),
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
                    size(px(1100.), px(720.)),
                    cx,
                ))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Claude Desktop Harness · M0".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };

            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| SmokeWindow::new(window, cx))
            })
            .expect("Failed to open window");
        });
}
