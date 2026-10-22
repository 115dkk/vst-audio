//! Worker sandbox policy and platform-independent profile construction.
use std::{
    io,
    path::{Path, PathBuf},
};

/// What a sandboxed worker may do beyond running its plugin. `None` in the
/// options means no sandbox (the default).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SandboxPolicy {
    /// Network access (licence checks, online content). Default: denied.
    pub network: bool,
    /// Starting other programs. Default: denied.
    pub processes: bool,
    /// Additional writable directories. Default: none.
    pub writable: Vec<PathBuf>,
}

/// Restrictions successfully installed before any plugin code was loaded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SandboxReport {
    pub mechanism: String,
    pub network_denied: bool,
    pub processes_denied: bool,
    pub writes_restricted: bool,
}
impl SandboxReport {
    pub fn applied(policy: &SandboxPolicy, mechanism: String) -> Self {
        Self {
            mechanism,
            network_denied: !policy.network,
            processes_denied: !policy.processes,
            writes_restricted: true,
        }
    }
}

/// Owned by the parent until the child has terminated, including startup errors.
/// Without a policy nothing is created and the worker inherits the parent's
/// environment unchanged.
#[derive(Debug)]
pub struct SandboxLaunch {
    pub policy: Option<SandboxPolicy>,
    pub temporary: Option<tempfile::TempDir>,
    pub plugin: PathBuf,
}
impl SandboxLaunch {
    pub fn new(policy: Option<&SandboxPolicy>, plugin: &Path) -> io::Result<Self> {
        let Some(policy) = policy else {
            return Ok(Self {
                policy: None,
                temporary: None,
                plugin: plugin.into(),
            });
        };
        let mut builder = tempfile::Builder::new();
        builder.prefix("vst-audio-worker-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let temporary = builder.tempdir_in(std::env::temp_dir().canonicalize()?)?;
        let mut policy = policy.clone();
        policy.writable = policy
            .writable
            .iter()
            .map(|path| {
                let path = path.canonicalize()?;
                if !path.is_dir() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "sandbox writable path is not a directory",
                    ));
                }
                Ok(path)
            })
            .collect::<io::Result<_>>()?;
        Ok(Self {
            policy: Some(policy),
            temporary: Some(temporary),
            plugin: plugin.canonicalize()?,
        })
    }
    /// The private temporary directory a sandboxed worker receives through
    /// `TMPDIR`, `TMP` and `TEMP`.
    pub fn temporary(&self) -> Option<&Path> {
        self.temporary.as_ref().map(tempfile::TempDir::path)
    }
    pub fn arguments(&self, args: &mut Vec<std::ffi::OsString>) {
        if let Some(policy) = &self.policy {
            args.push("--sandbox".into());
            if policy.network {
                args.push("--sandbox-network".into());
            }
            if policy.processes {
                args.push("--sandbox-processes".into());
            }
            for path in &policy.writable {
                args.push("--sandbox-writable".into());
                args.push(path.as_os_str().into());
            }
        }
    }
}

/// SBPL strings escape both delimiters and backslashes; parentheses inside a
/// quoted string are data, never profile forms. Reject control characters.
fn sbpl_string(path: &Path) -> io::Result<String> {
    let path = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "SBPL path is not UTF-8"))?;
    if path.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SBPL path contains a control character",
        ));
    }
    Ok(format!(
        "\"{}\"",
        path.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

pub fn seatbelt_profile(policy: &SandboxPolicy, temporary: &Path) -> io::Result<String> {
    let mut result = String::from("(version 1)\n(allow default)\n");
    if !policy.network {
        result.push_str("(deny network-outbound (remote ip))\n(deny network-bind (local ip))\n");
    }
    if !policy.processes {
        result.push_str("(deny process-exec)\n(deny process-fork)\n");
    }
    result.push_str("(deny file-write* (require-all");
    for path in std::iter::once(temporary).chain(policy.writable.iter().map(PathBuf::as_path)) {
        result.push_str(&format!(" (require-not (subpath {}))", sbpl_string(path)?));
    }
    // Existing IPC mappings remain usable; /dev/null is an explicit device exception.
    result.push_str(" (require-not (literal \"/dev/null\"))))\n");
    Ok(result)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LandlockRule {
    pub path: PathBuf,
    pub writable: bool,
}
pub fn landlock_rules(policy: &SandboxPolicy, temporary: &Path) -> Vec<LandlockRule> {
    let mut rules = vec![LandlockRule {
        path: "/".into(),
        writable: false,
    }];
    rules.extend(
        std::iter::once(temporary)
            .chain(policy.writable.iter().map(PathBuf::as_path))
            .chain([Path::new("/dev/null"), Path::new("/dev/shm")])
            .map(|path| LandlockRule {
                path: path.into(),
                writable: true,
            }),
    );
    rules
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SeccompRule {
    DenyInternetSockets,
    DenyExec,
}
pub fn seccomp_rules(policy: &SandboxPolicy) -> Vec<SeccompRule> {
    let mut rules = Vec::new();
    if !policy.network {
        rules.push(SeccompRule::DenyInternetSockets);
    }
    if !policy.processes {
        rules.push(SeccompRule::DenyExec);
    }
    rules
}

#[cfg(target_os = "linux")]
#[path = "sandbox_linux.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "sandbox_macos.rs"]
mod platform;
#[cfg(windows)]
#[path = "sandbox_windows.rs"]
pub(crate) mod platform;

/// Must be called on the worker's initial thread before creating other threads
/// or loading foreign code. Restrictions are irreversible for this process.
pub fn apply_worker(policy: &SandboxPolicy, temporary: &Path) -> io::Result<SandboxReport> {
    platform::apply_worker(policy, temporary)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policies_build_rules() {
        let policy = SandboxPolicy::default();
        assert_eq!(
            seccomp_rules(&policy),
            [SeccompRule::DenyInternetSockets, SeccompRule::DenyExec]
        );
        let rules = landlock_rules(&policy, Path::new("/private/tmp"));
        assert_eq!(
            rules[0],
            LandlockRule {
                path: "/".into(),
                writable: false
            }
        );
        assert!(rules[1..].iter().all(|rule| rule.writable));
        let enabled = SandboxPolicy {
            network: true,
            processes: true,
            writable: vec!["/allowed".into()],
        };
        assert!(seccomp_rules(&enabled).is_empty());
        assert!(
            landlock_rules(&enabled, Path::new("/tmp/private"))
                .iter()
                .any(|rule| rule.path == Path::new("/allowed") && rule.writable)
        );
    }
    #[test]
    fn sbpl_escapes_paths_and_flags() {
        let policy = SandboxPolicy {
            writable: vec!["/a\"(b)\\c".into()],
            ..Default::default()
        };
        let profile = seatbelt_profile(&policy, Path::new("/private/tmp")).unwrap();
        assert!(profile.contains("(subpath \"/a\\\"(b)\\\\c\")"));
        assert!(profile.contains("(deny process-exec)"));
        assert!(profile.contains("(deny network-outbound (remote ip))"));
        assert!(seatbelt_profile(&policy, Path::new("/bad\npath")).is_err());
        let profile = seatbelt_profile(
            &SandboxPolicy {
                network: true,
                processes: true,
                ..policy
            },
            Path::new("/tmp"),
        )
        .unwrap();
        assert!(!profile.contains("deny network"));
        assert!(!profile.contains("deny process"));
        assert!(profile.contains("deny file-write*"));
    }
}
