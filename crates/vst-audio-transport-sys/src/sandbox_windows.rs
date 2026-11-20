use super::{SandboxLaunch, SandboxPolicy, SandboxReport};
use std::{io, path::Path};
use win_custody::{AppContainerAccess, AppContainerProfile, Process, ProcessAccess};

pub(crate) fn launch_profile(
    exe: &Path,
    launch: &SandboxLaunch,
) -> io::Result<Option<AppContainerProfile>> {
    let Some(policy) = &launch.policy else {
        return Ok(None);
    };
    // Network-allowed workers use their own profile, so an administrator's
    // loopback exemption for it never reaches network-denied workers.
    let (name, capabilities): (&str, &[&str]) = if policy.network {
        (
            "vst-audio.worker.network",
            &[
                "internetClient",
                "internetClientServer",
                "privateNetworkClientServer",
            ],
        )
    } else {
        ("vst-audio.worker", &[])
    };
    let profile = AppContainerProfile::create_or_open_with_capabilities(
        name,
        "vst-audio worker",
        "Isolated audio plugin worker",
        capabilities,
    )?;
    profile.ensure_path_access(exe, AppContainerAccess::ReadExecute)?;
    if let Some(directory) = exe.parent() {
        profile.ensure_path_access(directory, AppContainerAccess::ReadExecute)?;
    }
    profile.ensure_path_access(&launch.plugin, AppContainerAccess::ReadExecute)?;
    if let Some(directory) = launch.plugin.parent() {
        profile.ensure_path_access(directory, AppContainerAccess::ReadExecute)?;
    }
    if let Some(temporary) = launch.temporary() {
        profile.ensure_path_access(temporary, AppContainerAccess::ReadWriteExecute)?;
    }
    for path in &policy.writable {
        profile.ensure_path_access(path, AppContainerAccess::ReadWriteExecute)?;
    }
    Ok(Some(profile))
}

pub fn apply_worker(policy: &SandboxPolicy, _: &Path) -> io::Result<SandboxReport> {
    // Token setup occurs before the first instruction in the Windows worker.
    // Independently verify the actual token rather than trusting a CLI flag.
    let process = Process::open(std::process::id(), ProcessAccess::QueryLimited)?;
    if !process.is_app_container()? {
        return Err(io::Error::other("worker is not in an AppContainer"));
    }
    if process.has_capability("internetClient")? != policy.network {
        return Err(io::Error::other(
            "worker network capability does not match the sandbox policy",
        ));
    }
    Ok(SandboxReport::applied(policy, "appcontainer".into()))
}
