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
            DELETE, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
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
            grant(exe, sid.0, false)?;
            // Sibling DLLs and plugin bundle resources need the same read/execute access.
            if let Some(directory) = exe.parent() {
                grant(directory, sid.0, false)?;
            }
            grant(&launch.plugin, sid.0, false)?;
            if let Some(directory) = launch.plugin.parent() {
                grant(directory, sid.0, false)?;
            }
            for path in
                std::iter::once(temporary).chain(policy.writable.iter().map(|path| path.as_path()))
            {
                grant(path, sid.0, true)?;
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
            security.token = Some(restricted_token()?);
        }
        Ok(security)
    }
}

fn grant(path: &Path, sid: PSID, writable: bool) -> windows::core::Result<()> {
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
    if !writable {
        let mut packages = [0u32; 17];
        let mut size = size_of_val(&packages) as u32;
        let package_sid = PSID(packages.as_mut_ptr().cast());
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
fn restricted_token() -> windows::core::Result<OwnedHandle> {
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
            DISABLE_MAX_PRIVILEGE,
            Some(&disable),
            None,
            None,
            &mut token,
        )
    }?;
    let token = own(token);
    let mut low = [0u32; 17];
    let mut bytes = size_of_val(&low) as u32;
    let low_sid = PSID(low.as_mut_ptr().cast());
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
            raw(&token),
            TokenIntegrityLevel,
            (&label as *const TOKEN_MANDATORY_LABEL).cast(),
            size_of_val(&label) as u32 + bytes,
        )
    }?;
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
