// slint! marks its generated code allow(unsafe_code), which forbid rejects;
// this crate's own code still has none.
#![deny(unsafe_code)]
use std::{cell::RefCell, rc::Rc, time::Duration};
use vst_audio_gui_example::Model;

slint::slint! {
    import { Button, Palette } from "std-widgets.slint";
    export component AppWindow inherits Window {
        title: "vst-audio · slint";
        preferred-width: 520px;
        preferred-height: 300px;
        in property <string> plugin-name;
        in property <string> status;
        in property <string> detail;
        in property <bool> can-open;
        in property <bool> can-close;
        callback open-editor();
        callback close-editor();
        callback quit();
        init => {
            Palette.color-scheme = ColorScheme.dark;
        }
        VerticalLayout {
            padding: 24px;
            spacing: 16px;
            alignment: start;
            Text {
                text: "Plugin editor";
                font-size: 26px;
            }
            Text {
                text: root.plugin-name;
                font-size: 16px;
            }
            Text {
                text: root.status;
                font-size: 16px;
            }
            HorizontalLayout {
                spacing: 12px;
                alignment: start;
                Button {
                    text: "Open editor";
                    enabled: root.can-open;
                    clicked => { root.open-editor(); }
                }
                Button {
                    text: "Close editor";
                    enabled: root.can-close;
                    clicked => { root.close-editor(); }
                }
            }
            if root.detail != "": Text {
                text: root.detail;
                wrap: word-wrap;
            }
            HorizontalLayout {
                alignment: start;
                Button {
                    text: "Quit";
                    clicked => { root.quit(); }
                }
            }
        }
    }
}

fn show(window: &AppWindow, model: &Model) {
    window.set_plugin_name(model.plugin_name.as_str().into());
    window.set_status(model.status.into());
    window.set_detail(model.detail.as_deref().unwrap_or_default().into());
    window.set_can_open(model.ready() && !model.editor_open);
    window.set_can_close(model.editor_open);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = Model::from_args()?;
    let outcome = model.outcome.clone();
    let smoke = model.is_smoke();
    let model = Rc::new(RefCell::new(model));
    let window = AppWindow::new()?;
    show(&window, &model.borrow());
    window.on_open_editor({
        let model = model.clone();
        move || model.borrow_mut().open()
    });
    window.on_close_editor({
        let model = model.clone();
        move || model.borrow_mut().close()
    });
    window.on_quit(|| {
        let _ = slint::quit_event_loop();
    });
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(16), {
        let model = model.clone();
        let window = window.as_weak();
        move || {
            let mut model = model.borrow_mut();
            let finished = model.tick();
            if let Some(window) = window.upgrade() {
                show(&window, &model);
            }
            if finished {
                let _ = slint::quit_event_loop();
            }
        }
    });
    window.run()?;
    Model::verify_outcome(&outcome, smoke)?;
    Ok(())
}
