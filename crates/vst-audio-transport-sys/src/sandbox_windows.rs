#![allow(unsafe_code)]
use super::{SandboxLaunch, SandboxPolicy, SandboxReport};
use std::{
    io,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
};
use windows::{
    Win32::{
        Foundation::*,
        Security::{Authorization::*, Isolation::*, *},
        Storage::FileSystem::{
            DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ,
            FILE_GENERIC_WRITE, GetFinalPathNameByHandleW, VOLUME_NAME_NT,
        },
        System::{
            SystemServices::{SE_GROUP_INTEGRITY, SECURITY_MANDATORY_LOW_RID},
            Threading::*,
        },
    },
    core::{PCWSTR, PWSTR, w},
};

struct Sid(PSID);
impl Drop for Sid {
    fn drop(&mut self) {
        // SAFETY: Create/DeriveAppContainerSid returned this allocation; freed once.
        unsafe { FreeSid(self.0) };
    }
}
struct Local(HLOCAL);
impl Drop for Local {
    fn drop(&mut self) {
        // SAFETY: this allocation was returned by an API documented to use LocalFree.
        unsafe { LocalFree(Some(self.0)) };
    }
}
fn wide(path: &Path) -> windows::core::Result<Vec<u16>> {
    let mut text: Vec<_> = path.as_os_str().encode_wide().collect();
    if text.contains(&0) {
        return Err(windows::core::Error::from_hresult(E_INVALIDARG));
    }
    text.push(0);
    Ok(text)
}
fn own(handle: HANDLE) -> OwnedHandle {
    // SAFETY: private helper receives only newly created, uniquely owned token handles.
    unsafe { OwnedHandle::from_raw_handle(handle.0) }
}
fn raw(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}

pub(crate) struct LaunchSecurity {
    _sid: Option<Sid>,
    pub capabilities: Option<SECURITY_CAPABILITIES>,
    pub token: Option<OwnedHandle>,
}
impl LaunchSecurity {
    pub fn new(exe: &Path, launch: &SandboxLaunch) -> windows::core::Result<Self> {
        let mut security = Self {
            _sid: None,
            capabilities: None,
            token: None,
        };
        let (Some(policy), Some(temporary)) = (&launch.policy, launch.temporary()) else {
            return Ok(security);
        };
        if !policy.network {
            // SAFETY: constant terminated strings, empty capability array, API-owned result.
            let sid = match unsafe {
                CreateAppContainerProfile(
                    w!("vst-audio.worker"),
                    w!("vst-audio worker"),
                    w!("Isolated audio plugin worker"),
                    None,
                )
            } {
                Ok(sid) => sid,
                Err(error)
                    if error.code()
                        == windows::core::HRESULT::from_win32(ERROR_ALREADY_EXISTS.0) =>
                {
                    // SAFETY: same constant name as the existing profile; API returns an owned SID.
                    unsafe { DeriveAppContainerSidFromAppContainerName(w!("vst-audio.worker")) }?
                }
                Err(error) => return Err(error),
            };
            let sid = Sid(sid);
            // Sibling DLLs and plugin bundle resources need the same read/execute access.
            for path in executables(exe, &launch.plugin) {
                grant(path, sid.0, false, true)?;
            }
            for path in
                std::iter::once(temporary).chain(policy.writable.iter().map(|path| path.as_path()))
            {
                grant(path, sid.0, true, false)?;
                low_label(path)?;
            }
            security.capabilities = Some(SECURITY_CAPABILITIES {
                AppContainerSid: sid.0,
                ..Default::default()
            });
            security._sid = Some(sid);
        } else {
            for path in
                std::iter::once(temporary).chain(policy.writable.iter().map(|path| path.as_path()))
            {
                low_label(path)?;
            }
            let token = restricted_token()?;
            // An elevated application's files may be readable only through
            // Administrators, which this token denies. Grant the user read/execute
            // only where opening as the worker actually fails.
            let mut user = [0u64; 16];
            let mut bytes = 0;
            // SAFETY: aligned storage exceeds TOKEN_USER plus maximum SID size.
            unsafe {
                GetTokenInformation(
                    raw(&token),
                    TokenUser,
                    Some(user.as_mut_ptr().cast()),
                    size_of_val(&user) as u32,
                    &mut bytes,
                )
            }?;
            // SAFETY: successful TokenUser query initialized a TOKEN_USER in aligned storage.
            let user = unsafe { &*user.as_ptr().cast::<TOKEN_USER>() }.User.Sid;
            for path in executables(exe, &launch.plugin) {
                if !openable(&token, path)? {
                    grant(path, user, false, false)?;
                    if !openable(&token, path)? {
                        return Err(windows::core::Error::new(
                            E_ACCESSDENIED,
                            format!(
                                "the sandboxed worker cannot read {} ({})",
                                path.display(),
                                describe(path, user)
                            ),
                        ));
                    }
                }
            }
            security.token = Some(token);
        }
        Ok(security)
    }
}

/// Temporary: which token or path form can open a file the worker cannot.
fn describe(path: &Path, _user: PSID) -> String {
    use std::os::windows::io::AsRawHandle;
    let probe = |token: windows::core::Result<OwnedHandle>, path: &Path| {
        token.and_then(|token| openable(&token, path)).map_or_else(
            |error| format!("{:?}", error.code()),
            |open| open.to_string(),
        )
    };
    let duplicate = || -> windows::core::Result<OwnedHandle> {
        // SAFETY: pseudo handle only; not owned or closed.
        let process = unsafe { GetCurrentProcess() };
        let mut original = HANDLE::default();
        // SAFETY: current process and writable token handle output.
        unsafe { OpenProcessToken(process, TOKEN_ALL_ACCESS, &mut original) }?;
        let original = own(original);
        let mut token = HANDLE::default();
        // SAFETY: live source token; output uniquely owned.
        unsafe {
            DuplicateTokenEx(
                raw(&original),
                TOKEN_ACCESS_MASK(0),
                None,
                SecurityImpersonation,
                TokenPrimary,
                &mut token,
            )
        }?;
        let token = own(token);
        lower(&token)?;
        Ok(token)
    };
    let global = std::fs::File::open(path).ok().and_then(|file| {
        let mut buffer = vec![0u16; 1024];
        // SAFETY: live file handle and writable buffer.
        let length = unsafe {
            GetFinalPathNameByHandleW(HANDLE(file.as_raw_handle()), &mut buffer, VOLUME_NAME_NT)
        } as usize;
        (length > 0 && length < buffer.len()).then(|| {
            std::path::PathBuf::from(format!(
                r"\\?\GLOBALROOT{}",
                String::from_utf16_lossy(&buffer[..length])
            ))
        })
    });
    format!(
        "open as: low={} dup-low={} medium-no-admin={} low-globalroot={} ({})",
        probe(token_variant(false, false, true), path),
        probe(duplicate(), path),
        probe(token_variant(true, false, false), path),
        global.as_deref().map_or_else(
            || "none".into(),
            |global| probe(token_variant(false, false, true), global)
        ),
        global
            .as_deref()
            .map_or_else(String::new, |g| g.display().to_string()),
    )
}

fn executables<'a>(exe: &'a Path, plugin: &'a Path) -> impl Iterator<Item = &'a Path> {
    [Some(exe), exe.parent(), Some(plugin), plugin.parent()]
        .into_iter()
        .flatten()
}

/// Opens `path` for read/execute while impersonating the worker token: the
/// file system's own decision, including deny-only groups and integrity.
fn openable(token: &OwnedHandle, path: &Path) -> windows::core::Result<bool> {
    use std::os::windows::fs::OpenOptionsExt;
    let mut impersonation = HANDLE::default();
    // SAFETY: the restricted token was created with duplicate access; output uniquely owned.
    unsafe { DuplicateToken(raw(token), SecurityImpersonation, &mut impersonation) }?;
    let impersonation = own(impersonation);
    // SAFETY: impersonates on this thread only; reverted before returning.
    unsafe { SetThreadToken(None, Some(raw(&impersonation))) }?;
    let opened = std::fs::OpenOptions::new()
        .access_mode(GENERIC_READ.0 | GENERIC_EXECUTE.0)
        // Directories need backup semantics to be opened at all; without the
        // privilege this is an ordinary access check.
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(path);
    // SAFETY: ends this thread's impersonation.
    unsafe { RevertToSelf() }?;
    match opened {
        Ok(_) => Ok(true),
        Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED.0 as i32) => Ok(false),
        Err(error) => Err(windows::core::Error::new(E_FAIL, error.to_string())),
    }
}

/// `packages`: an existing ALL APPLICATION PACKAGES grant already covers an
/// AppContainer `sid`, so no entry is added.
fn grant(path: &Path, sid: PSID, writable: bool, packages: bool) -> windows::core::Result<()> {
    let name = wide(path)?;
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let mut acl = std::ptr::null_mut();
    // SAFETY: terminated path and writable output pointers; returned descriptor owns acl.
    unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(name.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut acl),
            None,
            &mut descriptor,
        )
    }
    .ok()?;
    let _descriptor = Local(HLOCAL(descriptor.0));
    let mask = FILE_GENERIC_READ.0
        | FILE_GENERIC_EXECUTE.0
        | if writable {
            FILE_GENERIC_WRITE.0 | DELETE.0
        } else {
            0
        };
    if packages {
        let mut storage = [0u32; 17];
        let mut size = size_of_val(&storage) as u32;
        let package_sid = PSID(storage.as_mut_ptr().cast());
        // SAFETY: aligned SID_MAX_SUB_AUTHORITIES-sized buffer and writable size.
        unsafe { CreateWellKnownSid(WinBuiltinAnyPackageSid, None, Some(package_sid), &mut size) }?;
        let trustee = TRUSTEE_W {
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
            ptstrName: PWSTR(package_sid.0.cast()),
            ..Default::default()
        };
        let mut rights = 0;
        // SAFETY: acl is live within descriptor and trustee points to a valid SID.
        unsafe { GetEffectiveRightsFromAclW(acl, &trustee, &mut rights) }.ok()?;
        if rights & mask == mask {
            return Ok(());
        }
    }
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: mask,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        Trustee: TRUSTEE_W {
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: PWSTR(sid.0.cast()),
            ..Default::default()
        },
    };
    let mut replacement = std::ptr::null_mut();
    // SAFETY: entry and its SID remain live; acl is from GetNamedSecurityInfoW.
    unsafe { SetEntriesInAclW(Some(&[entry]), Some(acl), &mut replacement) }.ok()?;
    let _replacement = Local(HLOCAL(replacement.cast()));
    // SAFETY: replacement is a valid ACL; only the DACL is changed, with inheritance preserved.
    unsafe {
        SetNamedSecurityInfoW(
            PCWSTR(name.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(replacement),
            None,
        )
    }
    .ok()
}
fn low_label(path: &Path) -> windows::core::Result<()> {
    let name = wide(path)?;
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: constant SDDL grants no-write-up Low mandatory label to this directory and children.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            w!("S:(ML;OICI;NW;;;LW)"),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }?;
    let _descriptor = Local(HLOCAL(descriptor.0));
    let mut sacl = std::ptr::null_mut();
    let mut present = windows::core::BOOL::default();
    let mut defaulted = windows::core::BOOL::default();
    // SAFETY: descriptor is valid and all outputs point to initialized writable storage.
    unsafe { GetSecurityDescriptorSacl(descriptor, &mut present, &mut sacl, &mut defaulted) }?;
    // SAFETY: sacl belongs to the live descriptor; LABEL changes only the mandatory label.
    unsafe {
        SetNamedSecurityInfoW(
            PCWSTR(name.as_ptr()),
            SE_FILE_OBJECT,
            LABEL_SECURITY_INFORMATION,
            None,
            None,
            None,
            Some(sacl),
        )
    }
    .ok()
}
/// Temporary: a token with each restriction chosen separately, to find which one
/// denies the sandboxed worker's reads.
fn token_variant(
    disable_admin: bool,
    drop_privileges: bool,
    low: bool,
) -> windows::core::Result<OwnedHandle> {
    // SAFETY: pseudo handle only; not owned or closed.
    let process = unsafe { GetCurrentProcess() };
    let mut original = HANDLE::default();
    // SAFETY: current process and writable token handle output.
    unsafe { OpenProcessToken(process, TOKEN_ALL_ACCESS, &mut original) }?;
    let original = own(original);
    let mut admin = [0u32; 17];
    let mut bytes = size_of_val(&admin) as u32;
    let admin_sid = PSID(admin.as_mut_ptr().cast());
    // SAFETY: aligned maximum-sized SID storage and writable size.
    unsafe {
        CreateWellKnownSid(
            WinBuiltinAdministratorsSid,
            None,
            Some(admin_sid),
            &mut bytes,
        )
    }?;
    let disable = [SID_AND_ATTRIBUTES {
        Sid: admin_sid,
        Attributes: 0,
    }];
    let mut token = HANDLE::default();
    // SAFETY: original token and disabled SID live through the call; output uniquely owned.
    unsafe {
        CreateRestrictedToken(
            raw(&original),
            if drop_privileges {
                DISABLE_MAX_PRIVILEGE
            } else {
                CREATE_RESTRICTED_TOKEN_FLAGS(0)
            },
            disable_admin.then_some(&disable[..]),
            None,
            None,
            &mut token,
        )
    }?;
    let token = own(token);
    if low {
        lower(&token)?;
    }
    Ok(token)
}

fn lower(token: &OwnedHandle) -> windows::core::Result<()> {
    let mut storage = [0u32; 17];
    let mut bytes = size_of_val(&storage) as u32;
    let low_sid = PSID(storage.as_mut_ptr().cast());
    // SAFETY: aligned maximum-sized SID storage and writable size.
    unsafe { CreateWellKnownSid(WinLowLabelSid, None, Some(low_sid), &mut bytes) }?;
    let label = TOKEN_MANDATORY_LABEL {
        Label: SID_AND_ATTRIBUTES {
            Sid: low_sid,
            Attributes: SE_GROUP_INTEGRITY as u32,
        },
    };
    // SAFETY: label and referenced SID are live; token has TOKEN_ADJUST_DEFAULT rights.
    unsafe {
        SetTokenInformation(
            raw(token),
            TokenIntegrityLevel,
            (&label as *const TOKEN_MANDATORY_LABEL).cast(),
            size_of_val(&label) as u32 + bytes,
        )
    }?;
    Ok(())
}

fn restricted_token() -> windows::core::Result<OwnedHandle> {
    let token = token_variant(true, true, true)?;
    // An elevated token gives new objects (the worker process and its threads)
    // to Administrators and SYSTEM. With Administrators deny-only the worker could
    // not open its own threads, which the loader does for DLLs with static TLS.
    let mut user = [0u64; 16];
    let mut bytes = 0;
    // SAFETY: aligned storage exceeds TOKEN_USER plus maximum SID size.
    unsafe {
        GetTokenInformation(
            raw(&token),
            TokenUser,
            Some(user.as_mut_ptr().cast()),
            size_of_val(&user) as u32,
            &mut bytes,
        )
    }?;
    // SAFETY: successful TokenUser query initialized a TOKEN_USER in aligned storage.
    let user_sid = unsafe { &*user.as_ptr().cast::<TOKEN_USER>() }.User.Sid;
    let mut system = [0u32; 17];
    let mut bytes = size_of_val(&system) as u32;
    let system_sid = PSID(system.as_mut_ptr().cast());
    // SAFETY: aligned maximum-sized SID storage and writable size.
    unsafe { CreateWellKnownSid(WinLocalSystemSid, None, Some(system_sid), &mut bytes) }?;
    let entries = [user_sid, system_sid].map(|sid| EXPLICIT_ACCESS_W {
        grfAccessPermissions: GENERIC_ALL.0,
        grfAccessMode: SET_ACCESS,
        grfInheritance: NO_INHERITANCE,
        Trustee: TRUSTEE_W {
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: PWSTR(sid.0.cast()),
            ..Default::default()
        },
    });
    let mut acl = std::ptr::null_mut();
    // SAFETY: entries and their SIDs are live; the new ACL is LocalFree'd below.
    unsafe { SetEntriesInAclW(Some(&entries), None, &mut acl) }.ok()?;
    let _acl = Local(HLOCAL(acl.cast()));
    let dacl = TOKEN_DEFAULT_DACL { DefaultDacl: acl };
    // SAFETY: the ACL outlives the call, which copies it into the token.
    unsafe {
        SetTokenInformation(
            raw(&token),
            TokenDefaultDacl,
            (&dacl as *const TOKEN_DEFAULT_DACL).cast(),
            size_of_val(&dacl) as u32,
        )
    }?;
    Ok(token)
}

pub fn apply_worker(policy: &SandboxPolicy, _: &Path) -> io::Result<SandboxReport> {
    // Token setup occurs before the first instruction in the Windows worker.
    // Independently verify the actual token rather than trusting a CLI flag.
    // SAFETY: obtains a pseudo handle only.
    let process = unsafe { GetCurrentProcess() };
    let mut token = HANDLE::default();
    // SAFETY: current process and writable output; query-only token handle.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.map_err(io::Error::other)?;
    let token = own(token);
    if !policy.network {
        let mut enabled = 0u32;
        let mut bytes = 0;
        // SAFETY: output buffer matches TokenIsAppContainer's DWORD result.
        unsafe {
            GetTokenInformation(
                raw(&token),
                TokenIsAppContainer,
                Some((&mut enabled as *mut u32).cast()),
                size_of_val(&enabled) as u32,
                &mut bytes,
            )
        }
        .map_err(io::Error::other)?;
        if enabled == 0 {
            return Err(io::Error::other("worker is not in an AppContainer"));
        }
    } else {
        let mut buffer = [0usize; 32];
        let mut bytes = 0;
        // SAFETY: aligned storage exceeds TOKEN_MANDATORY_LABEL plus maximum SID size.
        unsafe {
            GetTokenInformation(
                raw(&token),
                TokenIntegrityLevel,
                Some(buffer.as_mut_ptr().cast()),
                size_of_val(&buffer) as u32,
                &mut bytes,
            )
        }
        .map_err(io::Error::other)?;
        // SAFETY: successful TokenIntegrityLevel query initialized a label in aligned storage.
        let label = unsafe { &*buffer.as_ptr().cast::<TOKEN_MANDATORY_LABEL>() };
        // SAFETY: query returned a valid SID containing the mandatory integrity RID.
        let rid = unsafe { GetSidSubAuthority(label.Label.Sid, 0) };
        // SAFETY: the returned subauthority lies in the live query buffer.
        let rid = unsafe { *rid };
        if rid > SECURITY_MANDATORY_LOW_RID as u32 {
            return Err(io::Error::other("worker integrity is above Low"));
        }
    }
    Ok(SandboxReport::applied(
        policy,
        if policy.network {
            "restricted-token-low"
        } else {
            "appcontainer"
        }
        .into(),
    ))
}
