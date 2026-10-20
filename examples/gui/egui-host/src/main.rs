#![forbid(unsafe_code)]
use eframe::egui;
use std::time::Duration;
use vst_audio_gui_example::Model;

struct App {
    model: Model,
}
impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        if self.model.tick() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_millis(16));
    }
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        egui::Frame::new().inner_margin(24).show(ui, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(12.0, 16.0);
            ui.heading("Plugin editor");
            ui.label(egui::RichText::new(&self.model.plugin_name).size(16.0));
            ui.label(egui::RichText::new(self.model.status).size(15.0));
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        self.model.ready() && !self.model.editor_open,
                        egui::Button::new("Open editor").min_size(egui::vec2(144.0, 40.0)),
                    )
                    .clicked()
                {
                    self.model.open();
                }
                if ui
                    .add_enabled(
                        self.model.editor_open,
                        egui::Button::new("Close editor").min_size(egui::vec2(144.0, 40.0)),
                    )
                    .clicked()
                {
                    self.model.close();
                }
            });
            if let Some(detail) = &self.model.detail {
                ui.collapsing("Error details", |ui| {
                    ui.label(detail);
                });
            }
            if ui.button("Quit").clicked() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = Model::from_args()?;
    let outcome = model.outcome.clone();
    let smoke = model.is_smoke();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([520.0, 280.0]),
        ..Default::default()
    };
    eframe::run_native(
        "vst-audio · egui",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(App { model }))
        }),
    )?;
    Model::verify_outcome(&outcome, smoke)?;
    Ok(())
}
