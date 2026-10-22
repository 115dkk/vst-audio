//! Test-only attempts, invoked synchronously by restoring mode 6.
pub fn run() -> [u8; 3] {
    let network = std::env::var("VST_AUDIO_PROBE_PORT")
        .ok()
        .and_then(|port| port.parse::<u16>().ok())
        .is_some_and(|port| {
            std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
                std::time::Duration::from_secs(2),
            )
            .is_ok()
        });
    let file = std::env::var_os("VST_AUDIO_PROBE_FILE")
        .is_some_and(|path| std::fs::write(path, b"sandbox probe").is_ok());
    #[cfg(unix)]
    let process = std::process::Command::new("/bin/true").status();
    #[cfg(windows)]
    let process = std::process::Command::new("cmd.exe")
        .args(["/c", "exit", "0"])
        .status();
    [
        u8::from(network),
        u8::from(file),
        u8::from(process.is_ok_and(|status| status.success())),
    ]
}
