#![forbid(unsafe_code)]
//! The same application model is driven by two unrelated GUI event loops.
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};
use vst_audio::{
    Command, Configuration, Host, HostEvent, Instance, LoadTask, PluginDescriptor, PluginFormat,
    RequestId,
};

pub struct Model {
    load: Option<LoadTask>,
    instance: Option<Instance>,
    open_request: Option<RequestId>,
    open_at: Option<Instant>,
    started: Instant,
    smoke: bool,
    smoke_closed: bool,
    pub plugin_name: String,
    pub status: &'static str,
    pub detail: Option<String>,
    pub editor_open: bool,
    pub outcome: Arc<AtomicU8>,
}
impl Model {
    pub fn from_args() -> Result<Self, String> {
        let mut args = std::env::args_os().skip(1);
        let mut worker = None;
        let mut plugin = None;
        let mut smoke = false;
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--worker") => worker = args.next().map(PathBuf::from),
                Some("--plugin") => plugin = args.next().map(PathBuf::from),
                Some("--smoke") => smoke = true,
                _ => return Err("usage: --worker <path> --plugin <path> [--smoke]".into()),
            }
        }
        let worker = worker.ok_or("missing --worker")?;
        let plugin = plugin.ok_or("missing --plugin")?;
        let format = if plugin
            .extension()
            .is_some_and(|s| s.eq_ignore_ascii_case("vst3"))
        {
            PluginFormat::Vst3
        } else {
            PluginFormat::Vst2
        };
        let plugin_name = plugin
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let load = Host::new(worker).spawn_async(
            PluginDescriptor::new(plugin, format),
            Configuration::default(),
            None,
        );
        Ok(Self {
            load: Some(load),
            instance: None,
            open_request: None,
            open_at: None,
            started: Instant::now(),
            smoke,
            smoke_closed: false,
            plugin_name,
            status: "Opening plugin…",
            detail: None,
            editor_open: false,
            outcome: Arc::new(AtomicU8::new(0)),
        })
    }
    pub fn ready(&self) -> bool {
        self.instance.is_some() && self.detail.is_none()
    }
    fn fail(&mut self, reason: String) {
        eprintln!("{reason}");
        self.status = "The plugin could not be opened. Check its path and restart.";
        self.detail = Some(reason);
        if self.smoke {
            self.outcome.store(2, Ordering::Release);
        }
    }
    pub fn open(&mut self) {
        if let Some(instance) = &self.instance {
            match instance.controller().request(Command::OpenEditor) {
                Ok(request) => {
                    self.open_request = Some(request);
                    self.status = "Opening editor…";
                }
                Err(error) => self.fail(error.to_string()),
            }
        }
    }
    pub fn close(&mut self) {
        if let Some(instance) = &self.instance {
            match instance.controller().request(Command::CloseEditor) {
                Ok(_) => {
                    self.status = "Closing editor…";
                    self.smoke_closed = true;
                }
                Err(error) => self.fail(error.to_string()),
            }
        }
    }
    /// Called by the owning GUI event loop. Returns true when a smoke run ends.
    pub fn tick(&mut self) -> bool {
        if let Some(task) = &self.load {
            match task.try_complete() {
                Ok(Some(instance)) => {
                    self.instance = Some(instance);
                    self.load = None;
                    self.status = "Ready";
                    if self.smoke {
                        self.open();
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    self.load = None;
                    self.fail(error.to_string());
                }
            }
        }
        for _ in 0..32 {
            let Some(event) = self.instance.as_ref().and_then(Instance::try_next_event) else {
                break;
            };
            match event {
                HostEvent::Completed { request } if Some(request) == self.open_request => {
                    self.editor_open = true;
                    self.status = "Editor open";
                    self.open_request = None;
                    self.open_at = Some(Instant::now());
                }
                HostEvent::EditorClosed => {
                    self.editor_open = false;
                    self.status = "Editor closed";
                    if self.smoke && self.smoke_closed {
                        self.outcome.store(1, Ordering::Release);
                    }
                }
                HostEvent::Failed { reason } | HostEvent::RequestFailed { reason, .. } => {
                    self.fail(reason)
                }
                _ => {}
            }
        }
        if self.smoke
            && self.editor_open
            && !self.smoke_closed
            && self
                .open_at
                .is_some_and(|t| t.elapsed() >= Duration::from_millis(350))
        {
            self.close();
        }
        if self.smoke && self.started.elapsed() > Duration::from_secs(20) {
            self.fail("GUI smoke deadline exceeded".into());
        }
        self.smoke && self.outcome.load(Ordering::Acquire) != 0
    }
    pub fn verify_outcome(outcome: &AtomicU8, smoke: bool) -> Result<(), String> {
        if smoke && outcome.load(Ordering::Acquire) != 1 {
            Err("editor smoke did not complete".into())
        } else {
            Ok(())
        }
    }
    pub fn is_smoke(&self) -> bool {
        self.smoke
    }
}
