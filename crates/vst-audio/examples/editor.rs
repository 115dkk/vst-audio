//! No GUI framework: the worker owns and displays the native plugin window.
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use vst_audio::{Command, Configuration, Host, HostEvent, PluginDescriptor, PluginFormat};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: editor <worker executable> <plugin file or bundle>".into());
    }
    let plugin = PathBuf::from(&args[1]);
    let format = if plugin
        .extension()
        .is_some_and(|s| s.eq_ignore_ascii_case("vst3"))
    {
        PluginFormat::Vst3
    } else {
        PluginFormat::Vst2
    };
    let instance = Host::new(PathBuf::from(&args[0])).spawn(
        &PluginDescriptor::new(plugin, format),
        &Configuration::default(),
        None,
    )?;
    instance.controller().request(Command::OpenEditor)?;
    let deadline = Instant::now() + Duration::from_secs(300);
    while Instant::now() < deadline {
        if let Some(event) = instance.try_next_event() {
            println!("{event:?}");
            if matches!(event, HostEvent::EditorClosed | HostEvent::Failed { .. }) {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
