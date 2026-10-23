#![allow(unsafe_code)]
use super::{SandboxPolicy, SandboxReport, SeccompRule, landlock_rules, seccomp_rules};
use std::{
    ffi::CString,
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::Path,
};

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
}
#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}
const READ: u64 = (1 << 0) | (1 << 2) | (1 << 3);
const WRITE: u64 = (1 << 1) | (0x3ff << 4) | (1 << 14);

fn owned_fd(result: libc::c_long) -> io::Result<OwnedFd> {
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: callers pass only successful fd-producing syscalls, transferring ownership once.
    Ok(unsafe { OwnedFd::from_raw_fd(result as i32) })
}

pub fn apply_worker(policy: &SandboxPolicy, temporary: &Path) -> io::Result<SandboxReport> {
    // SAFETY: version query has no pointer payload and does not create a ruleset.
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<u8>(),
            0usize,
            1u32,
        )
    };
    if abi < 3 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "sandbox requires Landlock ABI >= 3 (including truncate), query returned {abi}: {}",
                io::Error::last_os_error()
            ),
        ));
    }
    // Handle device ioctl rights when the kernel can express them. Permit those
    // only for the explicit writable device exception, not every readable file.
    let write = WRITE | if abi >= 5 { 1 << 15 } else { 0 };
    let attr = RulesetAttr {
        handled_access_fs: READ | write,
    };
    // SAFETY: attr has the UAPI layout and size; no newer trailing fields are supplied.
    let ruleset = owned_fd(unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &attr,
            size_of_val(&attr),
            0u32,
        )
    })?;
    for rule in landlock_rules(policy, temporary) {
        let path = CString::new(rule.path.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "sandbox path contains NUL")
        })?;
        // SAFETY: path is NUL-terminated; open returns a new descriptor or an error.
        let fd = owned_fd(
            unsafe { libc::open(path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) } as libc::c_long,
        )?;
        let is_directory = std::fs::metadata(&rule.path)?.is_dir();
        let allowed = if rule.writable { READ | write } else { READ };
        let allowed = if is_directory {
            allowed
        } else {
            allowed & ((1 << 0) | (1 << 1) | (1 << 2) | (1 << 14) | (1 << 15))
        };
        let attr = PathBeneathAttr {
            allowed_access: allowed,
            parent_fd: fd.as_raw_fd(),
        };
        // SAFETY: both descriptors remain owned; packed attr matches landlock_path_beneath_attr.
        if unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                ruleset.as_raw_fd(),
                1u32,
                &attr,
                0u32,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    // SAFETY: process-local irreversible privilege restriction, no pointer arguments.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: ruleset is initialized and owned; only the current initial thread exists.
    if unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset.as_raw_fd(), 0u32) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let instructions = filter(policy);
    let program = libc::sock_fprog {
        len: instructions.len() as u16,
        filter: instructions.as_ptr().cast_mut(),
    };
    // SAFETY: program and its initialized BPF array remain live for the syscall.
    // Kernel copies/validates them; descendants inherit the filter.
    if unsafe { libc::prctl(libc::PR_SET_SECCOMP, 2, &program, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(SandboxReport::applied(
        policy,
        format!("landlock-abi-{abi}+seccomp"),
    ))
}

fn instruction(code: u16, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}
fn filter(policy: &SandboxPolicy) -> Vec<libc::sock_filter> {
    const LOAD: u16 = 0x20;
    const EQUAL: u16 = 0x15;
    const RET: u16 = 0x06;
    const DENY: u32 = 0x0005_0000 | libc::EPERM as u32;
    #[cfg(target_arch = "x86_64")]
    const ARCH: u32 = 0xc000_003e;
    #[cfg(target_arch = "x86")]
    const ARCH: u32 = 0x4000_0003;
    #[cfg(target_arch = "aarch64")]
    const ARCH: u32 = 0xc000_00b7;
    let mut code = vec![
        instruction(LOAD, 0, 0, 4),
        instruction(EQUAL, 1, 0, ARCH),
        instruction(RET, 0, 0, 0x8000_0000),
        instruction(LOAD, 0, 0, 0),
    ];
    #[cfg(target_arch = "x86_64")]
    {
        // x32 shares AUDIT_ARCH_X86_64 but has different syscall numbers.
        code.push(instruction(0x45, 0, 1, 0x4000_0000));
        code.push(instruction(RET, 0, 0, 0x8000_0000));
    }
    // io_uring can create/connect sockets without passing through socket(2).
    code.push(instruction(EQUAL, 0, 1, libc::SYS_io_uring_setup as u32));
    code.push(instruction(RET, 0, 0, DENY));
    for rule in seccomp_rules(policy) {
        match rule {
            SeccompRule::DenyExec => {
                for number in [libc::SYS_execve, libc::SYS_execveat] {
                    code.push(instruction(EQUAL, 0, 1, number as u32));
                    code.push(instruction(RET, 0, 0, DENY));
                }
            }
            SeccompRule::DenyInternetSockets => {
                #[cfg(target_arch = "x86")]
                {
                    // Classic BPF cannot dereference socketcall's argument array.
                    // Deny legacy socket/socketpair creation, but allow operations
                    // on existing sockets (X11 uses legacy connect on i686).
                    code.push(instruction(EQUAL, 0, 5, libc::SYS_socketcall as u32));
                    code.push(instruction(LOAD, 0, 0, 16));
                    code.push(instruction(EQUAL, 0, 1, 1)); // SYS_SOCKET
                    code.push(instruction(RET, 0, 0, DENY));
                    code.push(instruction(EQUAL, 0, 1, 8)); // SYS_SOCKETPAIR
                    code.push(instruction(RET, 0, 0, DENY));
                    code.push(instruction(LOAD, 0, 0, 0));
                }
                code.push(instruction(EQUAL, 0, 7, libc::SYS_socket as u32));
                code.push(instruction(LOAD, 0, 0, 16));
                for family in [libc::AF_INET, libc::AF_INET6, libc::AF_PACKET] {
                    code.push(instruction(EQUAL, 0, 1, family as u32));
                    code.push(instruction(RET, 0, 0, DENY));
                }
                code.push(instruction(LOAD, 0, 0, 0));
            }
        }
    }
    code.push(instruction(RET, 0, 0, 0x7fff_0000));
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    fn evaluate(policy: &SandboxPolicy, arch: u32, syscall: u32, family: u32) -> u32 {
        let code = filter(policy);
        let mut accumulator = 0;
        let mut pc = 0;
        loop {
            let instruction = code[pc];
            pc += 1;
            match instruction.code {
                0x20 => {
                    accumulator = match instruction.k {
                        0 => syscall,
                        4 => arch,
                        16 => family,
                        _ => panic!("unexpected BPF load"),
                    }
                }
                0x15 | 0x45 => {
                    let matched = if instruction.code == 0x15 {
                        accumulator == instruction.k
                    } else {
                        accumulator & instruction.k != 0
                    };
                    pc += usize::from(if matched {
                        instruction.jt
                    } else {
                        instruction.jf
                    });
                }
                0x06 => return instruction.k,
                _ => panic!("unexpected BPF opcode"),
            }
        }
    }
    #[test]
    fn bpf_checks_architecture_and_policy_branches() {
        #[cfg(target_arch = "x86_64")]
        let arch = 0xc000_003e;
        #[cfg(target_arch = "x86")]
        let arch = 0x4000_0003;
        #[cfg(target_arch = "aarch64")]
        let arch = 0xc000_00b7;
        for network in [false, true] {
            for processes in [false, true] {
                let policy = SandboxPolicy {
                    network,
                    processes,
                    ..Default::default()
                };
                let allow = 0x7fff_0000;
                let deny = 0x0005_0000 | libc::EPERM as u32;
                assert_eq!(
                    evaluate(&policy, 0, libc::SYS_getpid as u32, 0),
                    0x8000_0000
                );
                assert_eq!(evaluate(&policy, arch, libc::SYS_getpid as u32, 0), allow);
                assert_eq!(
                    evaluate(&policy, arch, libc::SYS_socket as u32, libc::AF_UNIX as u32),
                    allow
                );
                for family in [libc::AF_INET, libc::AF_INET6, libc::AF_PACKET] {
                    assert_eq!(
                        evaluate(&policy, arch, libc::SYS_socket as u32, family as u32),
                        if network { allow } else { deny }
                    );
                }
                for syscall in [libc::SYS_execve, libc::SYS_execveat] {
                    assert_eq!(
                        evaluate(&policy, arch, syscall as u32, 0),
                        if processes { allow } else { deny }
                    );
                }
                assert_eq!(
                    evaluate(&policy, arch, libc::SYS_io_uring_setup as u32, 0),
                    deny
                );
                #[cfg(target_arch = "x86_64")]
                assert_eq!(
                    evaluate(&policy, arch, 0x4000_0000 | libc::SYS_socket as u32, 0),
                    0x8000_0000
                );
                #[cfg(target_arch = "x86")]
                assert_eq!(
                    evaluate(&policy, arch, libc::SYS_socketcall as u32, 1),
                    if network { allow } else { deny }
                );
            }
        }
    }
}
