#![forbid(unsafe_code)]
use serde::Serialize;
use std::sync::{Mutex, MutexGuard};
use tauri::{AppHandle, Manager, RunEvent, State};
use vst_audio_gui_example::Model;

/// Tauri runs synchronous commands on the event loop thread, so the model is
/// driven from the same thread as in the other examples. The page polls
/// `tick`; a smoke run therefore also needs the webview, its script and the
/// IPC bridge to work.
struct App(Mutex<Option<Model>>);
impl App {
    fn model(&self) -> MutexGuard<'_, Option<Model>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct View {
    plugin_name: String,
    status: &'static str,
    detail: Option<String>,
    can_open: bool,
    can_close: bool,
}
impl View {
    fn of(model: &Model) -> Self {
        Self {
            plugin_name: model.plugin_name.clone(),
            status: model.status,
            detail: model.detail.clone(),
            can_open: model.ready() && !model.editor_open,
            can_close: model.editor_open,
        }
    }
}

fn with_model<R>(app: &App, f: impl FnOnce(&mut Model) -> R) -> Option<R> {
    app.model().as_mut().map(f)
}

#[tauri::command]
fn tick(handle: AppHandle, app: State<'_, App>) -> Option<View> {
    let (finished, view) = with_model(&app, |model| (model.tick(), View::of(model)))?;
    if finished {
        handle.exit(0);
    }
    Some(view)
}
#[tauri::command]
fn open_editor(app: State<'_, App>) -> Option<View> {
    with_model(&app, |model| {
        model.open();
        View::of(model)
    })
}
#[tauri::command]
fn close_editor(app: State<'_, App>) -> Option<View> {
    with_model(&app, |model| {
        model.close();
        View::of(model)
    })
}
#[tauri::command]
fn quit(handle: AppHandle) {
    handle.exit(0);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = Model::from_args()?;
    let outcome = model.outcome.clone();
    let smoke = model.is_smoke();
    let app = tauri::Builder::default()
        .manage(App(Mutex::new(Some(model))))
        .invoke_handler(tauri::generate_handler![
            tick,
            open_editor,
            close_editor,
            quit
        ])
        .build(tauri::generate_context!())?;
    app.run_return(|handle, event| {
        if let RunEvent::Exit = event {
            // Managed state outlives the event loop; end the worker here, on
            // the event loop thread, as the other examples do when they return.
            handle.state::<App>().model().take();
        }
    });
    Model::verify_outcome(&outcome, smoke)?;
    Ok(())
}
