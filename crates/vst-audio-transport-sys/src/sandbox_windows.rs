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
        System::{SystemServices::SE_GROUP_ENABLED, Threading::*},
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

/// Network capabilities a network-allowed worker receives.
const NETWORK: [WELL_KNOWN_SID_TYPE; 3] = [
    WinCapabilityInternetClientSid,
    WinCapabilityInternetClientServerSid,
    WinCapabilityPrivateNetworkClientServerSid,
];
fn well_known(kind: WELL_KNOWN_SID_TYPE) -> windows::core::Result<[u32; 17]> {
    let mut storage = [0u32; 17];
    let mut size = size_of_val(&storage) as u32;
    // SAFETY: aligned SID_MAX_SUB_AUTHORITIES-sized buffer and writable size.
    unsafe {
        CreateWellKnownSid(
            kind,
            None,
            Some(PSID(storage.as_mut_ptr().cast())),
            &mut size,
        )
    }?;
    Ok(storage)
}

pub(crate) struct LaunchSecurity {
    _sid: Option<Sid>,
    // Heap storage referenced by `capabilities` until the child is created.
    _capability_sids: Box<[[u32; 17]]>,
    _capability_entries: Box<[SID_AND_ATTRIBUTES]>,
    pub capabilities: Option<SECURITY_CAPABILITIES>,
}
impl LaunchSecurity {
    pub fn new(exe: &Path, launch: &SandboxLaunch) -> windows::core::Result<Self> {
        let mut security = Self {
            _sid: None,
            _capability_sids: Box::new([]),
            _capability_entries: Box::new([]),
            capabilities: None,
        };
        let (Some(policy), Some(temporary)) = (&launch.policy, launch.temporary()) else {
            return Ok(security);
        };
        // Network-allowed workers use their own profile, so an administrator's
        // loopback exemption for it never reaches network-denied workers.
        let name = if policy.network {
            w!("vst-audio.worker.network")
        } else {
            w!("vst-audio.worker")
        };
        // SAFETY: constant terminated strings, empty capability array, API-owned result.
        let sid = match unsafe {
            CreateAppContainerProfile(
                name,
                w!("vst-audio worker"),
                w!("Isolated audio plugin worker"),
                None,
            )
        } {
            Ok(sid) => sid,
            Err(error)
                if error.code() == windows::core::HRESULT::from_win32(ERROR_ALREADY_EXISTS.0) =>
            {
                // SAFETY: same constant name as the existing profile; API returns an owned SID.
                unsafe { DeriveAppContainerSidFromAppContainerName(name) }?
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
        if policy.network {
            security._capability_sids = NETWORK
                .into_iter()
                .map(well_known)
                .collect::<windows::core::Result<_>>()?;
            security._capability_entries = security
                ._capability_sids
                .iter_mut()
                .map(|storage| SID_AND_ATTRIBUTES {
                    Sid: PSID(storage.as_mut_ptr().cast()),
                    Attributes: SE_GROUP_ENABLED as u32,
                })
                .collect();
        }
        security.capabilities = Some(SECURITY_CAPABILITIES {
            AppContainerSid: sid.0,
            Capabilities: if security._capability_entries.is_empty() {
                std::ptr::null_mut()
            } else {
                security._capability_entries.as_mut_ptr()
            },
            CapabilityCount: security._capability_entries.len() as u32,
            ..Default::default()
        });
        security._sid = Some(sid);
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
pub fn apply_worker(policy: &SandboxPolicy, _: &Path) -> io::Result<SandboxReport> {
    // Token setup occurs before the first instruction in the Windows worker.
    // Independently verify the actual token rather than trusting a CLI flag.
    // SAFETY: obtains a pseudo handle only.
    let process = unsafe { GetCurrentProcess() };
    let mut token = HANDLE::default();
    // SAFETY: current process and writable output; query-only token handle.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.map_err(io::Error::other)?;
    let token = own(token);
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
    let mut buffer = [0u64; 128];
    // SAFETY: aligned storage for TOKEN_GROUPS and its SIDs; reports the needed size.
    unsafe {
        GetTokenInformation(
            raw(&token),
            TokenCapabilities,
            Some(buffer.as_mut_ptr().cast()),
            size_of_val(&buffer) as u32,
            &mut bytes,
        )
    }
    .map_err(io::Error::other)?;
    // SAFETY: successful TokenCapabilities query initialized TOKEN_GROUPS in aligned storage.
    let groups = unsafe { &*buffer.as_ptr().cast::<TOKEN_GROUPS>() };
    // SAFETY: GroupCount entries follow the header inside the live query buffer.
    let groups =
        unsafe { std::slice::from_raw_parts(groups.Groups.as_ptr(), groups.GroupCount as usize) };
    let mut internet = well_known(WinCapabilityInternetClientSid).map_err(io::Error::other)?;
    let internet = PSID(internet.as_mut_ptr().cast());
    // SAFETY: both SIDs are valid for the duration of the comparison.
    let network = groups
        .iter()
        .any(|group| unsafe { EqualSid(group.Sid, internet) }.is_ok());
    if network != policy.network {
        return Err(io::Error::other(
            "worker network capability does not match the sandbox policy",
        ));
    }
    Ok(SandboxReport::applied(policy, "appcontainer".into()))
}
