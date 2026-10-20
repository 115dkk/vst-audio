#![forbid(unsafe_code)]
use iced::{
    Element, Subscription, Task, Theme,
    widget::{button, column, container, row, text},
};
use std::time::Duration;
use vst_audio_gui_example::Model;

#[derive(Debug, Clone)]
enum Message {
    Tick,
    Open,
    Close,
    Quit,
}
struct App {
    model: Model,
}
impl App {
    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick => {
                if self.model.tick() {
                    return iced::exit();
                }
            }
            Message::Open => self.model.open(),
            Message::Close => self.model.close(),
            Message::Quit => return iced::exit(),
        }
        Task::none()
    }
    fn view(&self) -> Element<'_, Message> {
        let mut open = button(text("Open editor").size(16)).padding([10, 20]);
        let mut close = button(text("Close editor").size(16)).padding([10, 20]);
        if self.model.ready() && !self.model.editor_open {
            open = open.on_press(Message::Open);
        }
        if self.model.editor_open {
            close = close.on_press(Message::Close);
        }
        container(
            column![
                text("Plugin editor").size(26),
                text(&self.model.plugin_name).size(16),
                text(self.model.status).size(16),
                row![open, close].spacing(12),
                button(text("Quit").size(16))
                    .on_press(Message::Quit)
                    .padding([10, 20]),
            ]
            .spacing(16),
        )
        .padding(24)
        .into()
    }
    fn subscription(&self) -> Subscription<Message> {
        iced::time::every(Duration::from_millis(16)).map(|_| Message::Tick)
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = Model::from_args()?;
    let outcome = model.outcome.clone();
    let smoke = model.is_smoke();
    iced::application("vst-audio · iced", App::update, App::view)
        .theme(|_| Theme::Dark)
        .subscription(App::subscription)
        .window_size((520.0, 300.0))
        .run_with(|| (App { model }, Task::none()))?;
    Model::verify_outcome(&outcome, smoke)?;
    Ok(())
}
