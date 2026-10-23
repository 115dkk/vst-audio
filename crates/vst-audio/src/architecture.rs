//! Bounded binary-header inspection; plugin code is never loaded here.
use crate::{PluginFormat, StartError};
use std::{fs::File, io::Read, path::Path};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Architecture {
    X86,
    X86_64,
    Aarch64,
}

impl Architecture {
    /// The architecture this process was built for.
    pub fn host() -> Self {
        #[cfg(target_arch = "x86")]
        {
            Self::X86
        }
        #[cfg(target_arch = "x86_64")]
        {
            Self::X86_64
        }
        #[cfg(target_arch = "aarch64")]
        {
            Self::Aarch64
        }
    }

    /// Whether this architecture can run on the host platform. Optional OS
    /// support (multilib, Windows emulation or Rosetta) must also be installed.
    pub fn runs_on_host(self) -> bool {
        self == Self::host()
            || matches!((Self::host(), self), (Self::X86_64, Self::X86))
                && cfg!(any(target_os = "windows", target_os = "linux"))
            || matches!((Self::host(), self), (Self::Aarch64, Self::X86_64))
                && cfg!(any(target_os = "windows", target_os = "macos"))
            || matches!((Self::host(), self), (Self::Aarch64, Self::X86))
                && cfg!(target_os = "windows")
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::X86 => "x86",
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
        }
    }
}

/// The architectures a plugin binary is built for, including universal Mach-O
/// slices and every binary present in this OS's VST3 bundle folders.
/// Reads at most the first 4096 bytes of each binary, without loading it.
pub fn plugin_architectures(
    path: &Path,
    format: PluginFormat,
) -> Result<Vec<Architecture>, StartError> {
    if !path.is_dir() {
        return read_binary(path);
    }
    let name = path.file_name().ok_or_else(invalid)?;
    let mut architectures = Vec::new();
    if format == PluginFormat::Vst3 {
        #[cfg(target_os = "windows")]
        let folders = [
            "x86-win",
            "x86_64-win",
            "arm64-win",
            "arm64ec-win",
            "arm64x-win",
        ];
        #[cfg(target_os = "linux")]
        let folders = ["i386-linux", "x86_64-linux", "aarch64-linux"];
        #[cfg(target_os = "macos")]
        let folders = ["MacOS"];
        #[cfg(target_os = "windows")]
        let binary = Path::new(name).to_path_buf();
        #[cfg(target_os = "linux")]
        let binary = Path::new(name).with_extension("so");
        #[cfg(target_os = "macos")]
        let binary = Path::new(name).with_extension("");
        for folder in folders {
            let candidate = path.join("Contents").join(folder).join(&binary);
            if candidate.is_file() {
                for architecture in read_binary(&candidate)? {
                    insert(&mut architectures, architecture);
                }
            }
        }
    } else {
        #[cfg(target_os = "macos")]
        if path.extension().is_some_and(|extension| extension == "vst")
            && let Some(binary) = macos_executable(path)
        {
            return read_binary(&binary);
        }
    }
    #[cfg(target_os = "macos")]
    if architectures.is_empty()
        && let Some(binary) = macos_executable(path)
    {
        for architecture in read_binary(&binary)? {
            insert(&mut architectures, architecture);
        }
    }
    if architectures.is_empty() {
        return Err(StartError(
            "plugin bundle has no binary for this platform".into(),
        ));
    }
    Ok(architectures)
}

/// A macOS bundle's executable: `Contents/MacOS/<bundle name>`, or the only
/// file there when the executable is named otherwise (`CFBundleExecutable`).
#[cfg(target_os = "macos")]
fn macos_executable(bundle: &Path) -> Option<std::path::PathBuf> {
    let folder = bundle.join("Contents/MacOS");
    let named = folder.join(Path::new(bundle.file_name()?).with_extension(""));
    if named.is_file() {
        return Some(named);
    }
    let mut files = std::fs::read_dir(&folder)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file());
    let only = files.next()?;
    files.next().is_none().then_some(only)
}

fn invalid() -> StartError {
    StartError("unsupported or truncated plugin binary header".into())
}

fn insert(architectures: &mut Vec<Architecture>, architecture: Architecture) {
    if !architectures.contains(&architecture) {
        architectures.push(architecture);
    }
}

fn read_binary(path: &Path) -> Result<Vec<Architecture>, StartError> {
    let mut file = File::open(path).map_err(|e| StartError(e.to_string()))?;
    let mut header = [0u8; 4096];
    let mut length = 0;
    while length < header.len() {
        match file.read(&mut header[length..]) {
            Ok(0) => break,
            Ok(count) => length += count,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(StartError(e.to_string())),
        }
    }
    parse(&header[..length])
}

fn bytes<const N: usize>(header: &[u8], offset: usize) -> Result<[u8; N], StartError> {
    header
        .get(offset..offset.checked_add(N).ok_or_else(invalid)?)
        .and_then(|part| part.try_into().ok())
        .ok_or_else(invalid)
}

fn mach_cpu(cpu: u32) -> Result<Architecture, StartError> {
    match cpu {
        7 => Ok(Architecture::X86),
        0x01000007 => Ok(Architecture::X86_64),
        0x0100000c => Ok(Architecture::Aarch64),
        _ => Err(invalid()),
    }
}

fn parse(header: &[u8]) -> Result<Vec<Architecture>, StartError> {
    let magic = bytes::<4>(header, 0)?;
    let architecture = if magic[..2] == *b"MZ" {
        let offset =
            usize::try_from(u32::from_le_bytes(bytes(header, 0x3c)?)).map_err(|_| invalid())?;
        if bytes::<4>(header, offset)? != *b"PE\0\0" {
            return Err(invalid());
        }
        match u16::from_le_bytes(bytes(header, offset.checked_add(4).ok_or_else(invalid)?)?) {
            0x014c => Architecture::X86,
            0x8664 => Architecture::X86_64,
            0xaa64 => Architecture::Aarch64,
            _ => return Err(invalid()),
        }
    } else if magic == *b"\x7fELF" {
        if header.get(5) != Some(&1) {
            return Err(invalid());
        }
        match u16::from_le_bytes(bytes(header, 18)?) {
            3 => Architecture::X86,
            62 => Architecture::X86_64,
            183 => Architecture::Aarch64,
            _ => return Err(invalid()),
        }
    } else {
        match u32::from_le_bytes(magic) {
            0xfeedface | 0xfeedfacf => {
                let architecture = mach_cpu(u32::from_le_bytes(bytes(header, 4)?))?;
                if (u32::from_le_bytes(magic) == 0xfeedface) != (architecture == Architecture::X86)
                {
                    return Err(invalid());
                }
                architecture
            }
            _ => {
                let entry_size = match u32::from_be_bytes(magic) {
                    0xcafebabe => 20,
                    0xcafebabf => 32,
                    _ => return Err(invalid()),
                };
                let count = u32::from_be_bytes(bytes(header, 4)?);
                if !(1..=32).contains(&count) {
                    return Err(invalid());
                }
                let count = count as usize;
                // Validate the whole bounded table, not just each entry's CPU field.
                if header.len() < 8 + count * entry_size {
                    return Err(invalid());
                }
                let mut architectures = Vec::new();
                for index in 0..count {
                    insert(
                        &mut architectures,
                        mach_cpu(u32::from_be_bytes(bytes(header, 8 + index * entry_size)?))?,
                    );
                }
                return Ok(architectures);
            }
        }
    };
    Ok(vec![architecture])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    pub(crate) struct Directory(PathBuf);
    impl Directory {
        pub(crate) fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "vst-audio-arch-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    pub(crate) fn elf(architecture: Architecture) -> Vec<u8> {
        let mut header = vec![0; 64];
        header[..4].copy_from_slice(b"\x7fELF");
        header[4] = if architecture == Architecture::X86 {
            1
        } else {
            2
        };
        header[5] = 1;
        let machine: u16 = match architecture {
            Architecture::X86 => 3,
            Architecture::X86_64 => 62,
            Architecture::Aarch64 => 183,
        };
        header[18..20].copy_from_slice(&machine.to_le_bytes());
        header
    }

    fn fat(magic: u32, cpus: &[u32]) -> Vec<u8> {
        let size = if magic == 0xcafebabe { 20 } else { 32 };
        let mut header = vec![0; 8 + cpus.len() * size];
        header[..4].copy_from_slice(&magic.to_be_bytes());
        header[4..8].copy_from_slice(&(cpus.len() as u32).to_be_bytes());
        for (index, cpu) in cpus.iter().enumerate() {
            header[8 + index * size..12 + index * size].copy_from_slice(&cpu.to_be_bytes());
        }
        header
    }

    #[test]
    fn worker_selection_and_registration() {
        use crate::{Host, PluginDescriptor};
        let directory = Directory::new();
        let path = directory.path().join("plugin");
        let plugin = PluginDescriptor::new(&path, PluginFormat::Vst2);
        let host = Host::new("native");
        assert_eq!(host.worker_for(&plugin).unwrap(), Path::new("native"));
        fs::write(&path, b"unreadable header").unwrap();
        assert_eq!(host.worker_for(&plugin).unwrap(), Path::new("native"));
        for architecture in [
            Architecture::X86,
            Architecture::X86_64,
            Architecture::Aarch64,
        ] {
            fs::write(&path, elf(architecture)).unwrap();
            if architecture == Architecture::host() {
                assert_eq!(host.worker_for(&plugin).unwrap(), Path::new("native"));
                assert_eq!(
                    host.clone()
                        .with_worker(architecture, "replacement")
                        .worker_for(&plugin)
                        .unwrap(),
                    Path::new("replacement")
                );
            } else {
                let error = host.worker_for(&plugin).unwrap_err();
                assert!(error.0.contains(architecture.name()));
                let registered = host
                    .clone()
                    .with_worker(architecture, "old")
                    .with_worker(architecture, "foreign");
                if architecture.runs_on_host() {
                    assert_eq!(
                        registered.worker_for(&plugin).unwrap(),
                        Path::new("foreign")
                    );
                } else {
                    assert!(registered.worker_for(&plugin).is_err());
                }
            }
        }
        // A universal binary always prefers the native worker, irrespective of
        // slice or registration order.
        fs::write(&path, fat(0xcafebabe, &[7, 0x01000007, 0x0100000c])).unwrap();
        let host = host
            .with_worker(Architecture::X86, "x86")
            .with_worker(Architecture::X86_64, "x64")
            .with_worker(Architecture::Aarch64, "arm");
        assert_eq!(host.worker_for(&plugin).unwrap(), host.worker_path());
    }

    #[test]
    fn pe_architectures_and_bounds() {
        let mut header = [0; 128];
        header[..2].copy_from_slice(b"MZ");
        header[0x3c..0x40].copy_from_slice(&64u32.to_le_bytes());
        header[64..68].copy_from_slice(b"PE\0\0");
        for (machine, architecture) in [
            (0x014cu16, Architecture::X86),
            (0x8664, Architecture::X86_64),
            (0xaa64, Architecture::Aarch64),
        ] {
            header[68..70].copy_from_slice(&machine.to_le_bytes());
            assert_eq!(parse(&header).unwrap(), [architecture]);
        }
        assert!(parse(&header[..69]).is_err());
        header[68..70].copy_from_slice(&0xffffu16.to_le_bytes());
        assert!(parse(&header).is_err());
        header[0x3c..0x40].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse(&header).is_err());
        header[0x3c..0x40].copy_from_slice(&4096u32.to_le_bytes());
        let directory = Directory::new();
        let path = directory.path().join("large.dll");
        let mut file = header.to_vec();
        file.resize(4102, 0);
        file[4096..4100].copy_from_slice(b"PE\0\0");
        file[4100..4102].copy_from_slice(&0x8664u16.to_le_bytes());
        fs::write(&path, file).unwrap();
        assert!(plugin_architectures(&path, PluginFormat::Vst2).is_err());
    }

    #[test]
    fn elf_architectures_and_invalid_headers() {
        for architecture in [
            Architecture::X86,
            Architecture::X86_64,
            Architecture::Aarch64,
        ] {
            let mut header = elf(architecture);
            assert_eq!(parse(&header).unwrap(), [architecture]);
            assert!(parse(&header[..19]).is_err());
            header[5] = 2;
            assert!(parse(&header).is_err());
            header[5] = 1;
            header[18..20].copy_from_slice(&0xffffu16.to_le_bytes());
            assert!(parse(&header).is_err());
        }
        for length in 0..4 {
            assert!(parse(&[0; 4][..length]).is_err());
        }
        assert!(parse(b"not a binary").is_err());
    }

    #[test]
    fn mach_o_thin_and_universal() {
        for (magic, cpu, architecture) in [
            (0xfeedfaceu32, 7u32, Architecture::X86),
            (0xfeedfacf, 0x01000007, Architecture::X86_64),
            (0xfeedfacf, 0x0100000c, Architecture::Aarch64),
        ] {
            let header = [magic.to_le_bytes(), cpu.to_le_bytes()].concat();
            assert_eq!(parse(&header).unwrap(), [architecture]);
            assert!(parse(&header[..7]).is_err());
        }
        for magic in [0xcafebabe, 0xcafebabf] {
            let mut header = fat(magic, &[0x01000007, 0x0100000c, 0x01000007, 7]);
            assert_eq!(
                parse(&header).unwrap(),
                [
                    Architecture::X86_64,
                    Architecture::Aarch64,
                    Architecture::X86
                ]
            );
            assert!(parse(&header[..header.len() - 1]).is_err());
            header[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
            assert!(parse(&header).is_err());
            assert!(parse(&fat(magic, &[])).is_err());
            assert!(parse(&fat(magic, &[123])).is_err());
        }
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    fn vst3_bundle_merges_platform_binaries() {
        let directory = Directory::new();
        let bundle = directory.path().join("Matrix.vst3");
        #[cfg(target_os = "linux")]
        let (folders, name) = (["i386-linux", "x86_64-linux", "aarch64-linux"], "Matrix.so");
        #[cfg(target_os = "windows")]
        let (folders, name) = (["x86-win", "x86_64-win", "arm64-win"], "Matrix.vst3");
        let expected = [
            Architecture::X86,
            Architecture::X86_64,
            Architecture::Aarch64,
        ];
        fs::create_dir(&bundle).unwrap();
        assert!(plugin_architectures(&bundle, PluginFormat::Vst3).is_err());
        for (folder, architecture) in folders.into_iter().zip(expected) {
            let path = bundle.join("Contents").join(folder).join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, elf(architecture)).unwrap();
        }
        assert_eq!(
            plugin_architectures(&bundle, PluginFormat::Vst3).unwrap(),
            expected
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_bundles_resolve_executable() {
        let directory = Directory::new();
        for (extension, format) in [("vst", PluginFormat::Vst2), ("vst3", PluginFormat::Vst3)] {
            let bundle = directory.path().join(format!("Matrix.{extension}"));
            let path = bundle.join("Contents/MacOS/Matrix");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, fat(0xcafebabe, &[0x01000007, 0x0100000c])).unwrap();
            assert_eq!(
                plugin_architectures(&bundle, format).unwrap(),
                [Architecture::X86_64, Architecture::Aarch64]
            );
        }
    }
}
