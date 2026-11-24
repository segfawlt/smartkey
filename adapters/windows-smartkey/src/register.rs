use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use windows::core::{implement, GUID, HRESULT, Interface, PCWSTR};
use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, E_NOINTERFACE, HINSTANCE, HMODULE, S_FALSE, S_OK, WIN32_ERROR,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IClassFactory, IClassFactory_Impl,
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::LibraryLoader::{DisableThreadLibraryCalls, GetModuleFileNameW};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE,
    REG_SZ,
};
use windows::Win32::System::SystemServices::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH};
use windows::Win32::UI::Input::KeyboardAndMouse::HKL;
use windows::Win32::UI::TextServices::{
    CLSID_TF_CategoryMgr, CLSID_TF_InputProcessorProfiles, ITfCategoryMgr,
    ITfInputProcessorProfileMgr, ITfInputProcessorProfiles, GUID_TFCAT_TIPCAP_COMLESS,
    GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT, GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT,
    GUID_TFCAT_TIPCAP_SECUREMODE, GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
    GUID_TFCAT_TIPCAP_UIELEMENTENABLED, GUID_TFCAT_TIP_KEYBOARD, TF_IPPMF_DISABLEPROFILE,
    TF_IPPMF_ENABLEPROFILE, TF_IPPMF_FORSESSION, TF_PROFILETYPE_INPUTPROCESSOR,
};

use crate::guids::{
    CLSID_SMARTKEY_TEXT_SERVICE, CLSID_SMARTKEY_TEXT_SERVICE_STR, GUID_SMARTKEY_PROFILE_EN_US,
    LANGID_EN_US, SMARTKEY_DISPLAY_NAME,
};
use crate::tsf::SmartKeyTextService;

static DLL_MODULE: AtomicUsize = AtomicUsize::new(0);
static DLL_REF_COUNT: AtomicUsize = AtomicUsize::new(0);

const SELFREG_E_CLASS: HRESULT = HRESULT(0x8004_0201_u32 as i32);

pub fn increment_dll_ref() {
    DLL_REF_COUNT.fetch_add(1, Ordering::SeqCst);
}

pub fn decrement_dll_ref() {
    DLL_REF_COUNT.fetch_sub(1, Ordering::SeqCst);
}

fn to_wide_nul(s: &str) -> Vec<u16> {
    let mut out: Vec<u16> = s.encode_utf16().collect();
    out.push(0);
    out
}

fn current_dll_path() -> Result<PathBuf, String> {
    let hmod = DLL_MODULE.load(Ordering::SeqCst);
    let mut buf = vec![0u16; 1024];
    let len = unsafe {
        GetModuleFileNameW(
            if hmod == 0 {
                None
            } else {
                Some(HMODULE(hmod as *mut c_void))
            },
            &mut buf,
        )
    };
    if len == 0 {
        return Err("GetModuleFileNameW failed".to_string());
    }
    let path_str = String::from_utf16_lossy(&buf[..len as usize]);
    Ok(PathBuf::from(path_str))
}

fn clsid_subkey() -> String {
    format!("Software\\Classes\\CLSID\\{CLSID_SMARTKEY_TEXT_SERVICE_STR}")
}

fn clsid_inproc_subkey() -> String {
    format!("{}\\InprocServer32", clsid_subkey())
}

fn reg_set_sz(hkey: HKEY, name: Option<&str>, value: &str) -> Result<(), String> {
    let name_w = name.map(to_wide_nul);
    let val_w = to_wide_nul(value);
    let bytes = unsafe {
        std::slice::from_raw_parts(val_w.as_ptr() as *const u8, val_w.len() * 2)
    };
    let name_ptr = match &name_w {
        Some(v) => PCWSTR(v.as_ptr()),
        None => PCWSTR::null(),
    };
    let err = unsafe { RegSetValueExW(hkey, name_ptr, None, REG_SZ, Some(bytes)) };
    if err != WIN32_ERROR(0) {
        return Err(format!("RegSetValueExW failed: {}", err.0));
    }
    Ok(())
}

fn write_clsid_registry(root: HKEY, dll_path: &Path) -> Result<(), String> {
    let subkey = to_wide_nul(&clsid_inproc_subkey());
    let mut hkey = HKEY::default();
    let err = unsafe {
        RegCreateKeyExW(
            root,
            PCWSTR(subkey.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut hkey,
            None,
        )
    };
    if err != WIN32_ERROR(0) {
        return Err(format!(
            "RegCreateKeyExW({}) failed: error {}",
            clsid_inproc_subkey(),
            err.0
        ));
    }
    let res_dll = reg_set_sz(hkey, None, &dll_path.to_string_lossy());
    let res_tm = reg_set_sz(hkey, Some("ThreadingModel"), "Apartment");
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    res_dll?;
    res_tm?;

    let clsid_key_w = to_wide_nul(&clsid_subkey());
    let mut root_clsid = HKEY::default();
    if unsafe {
        RegCreateKeyExW(
            root,
            PCWSTR(clsid_key_w.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut root_clsid,
            None,
        )
    } == WIN32_ERROR(0)
    {
        let _ = reg_set_sz(root_clsid, None, SMARTKEY_DISPLAY_NAME);
        unsafe {
            let _ = RegCloseKey(root_clsid);
        }
    }
    Ok(())
}

fn delete_clsid_registry(root: HKEY) {
    let key = to_wide_nul(&clsid_subkey());
    unsafe {
        let _ = RegDeleteTreeW(root, PCWSTR(key.as_ptr()));
    }
}

fn query_clsid_inproc(root: HKEY) -> Option<PathBuf> {
    let key = to_wide_nul(&clsid_inproc_subkey());
    let mut hkey = HKEY::default();
    if unsafe { RegOpenKeyExW(root, PCWSTR(key.as_ptr()), None, KEY_READ, &mut hkey) }
        != WIN32_ERROR(0)
    {
        return None;
    }
    let mut bytes_len: u32 = 0;
    let mut reg_type = REG_SZ;
    let status = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR::null(),
            None,
            Some(&mut reg_type),
            None,
            Some(&mut bytes_len),
        )
    };
    let mut out = None;
    if status == WIN32_ERROR(0) && bytes_len >= 2 {
        let mut buf = vec![0u16; (bytes_len as usize).div_ceil(2)];
        let mut len_inout = (buf.len() * 2) as u32;
        if unsafe {
            RegQueryValueExW(
                hkey,
                PCWSTR::null(),
                None,
                Some(&mut reg_type),
                Some(buf.as_mut_ptr() as *mut u8),
                Some(&mut len_inout),
            )
        } == WIN32_ERROR(0)
        {
            let wlen = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            let s = String::from_utf16_lossy(&buf[..wlen]);
            if !s.trim().is_empty() {
                out = Some(PathBuf::from(s));
            }
        }
    }
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    out
}

/// Checks `HKLM` then `HKCU` for SmartKey's registered TSF `InprocServer32` DLL path.
pub fn is_clsid_registered() -> Option<PathBuf> {
    query_clsid_inproc(HKEY_LOCAL_MACHINE).or_else(|| query_clsid_inproc(HKEY_CURRENT_USER))
}

struct ComApartmentGuard(bool);

impl ComApartmentGuard {
    fn enter() -> Self {
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        Self(hr == S_OK || hr == S_FALSE)
    }
}

impl Drop for ComApartmentGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

const CATEGORIES: &[GUID] = &[
    GUID_TFCAT_TIP_KEYBOARD,
    GUID_TFCAT_TIPCAP_SECUREMODE,
    GUID_TFCAT_TIPCAP_UIELEMENTENABLED,
    GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT,
    GUID_TFCAT_TIPCAP_COMLESS,
    GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
    GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
];

/// Registers SmartKey's COM CLSID, TSF profile (`en-US`), and capabilities, and activates
/// the profile for the current Windows session.
pub fn register_server(dll_path: &Path, per_user_clsid: bool) -> Result<(), String> {
    let root = if per_user_clsid {
        HKEY_CURRENT_USER
    } else {
        HKEY_LOCAL_MACHINE
    };
    write_clsid_registry(root, dll_path)?;

    let _com = ComApartmentGuard::enter();

    let profiles: ITfInputProcessorProfiles =
        unsafe { CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| format!("CoCreateInstance(ITfInputProcessorProfiles) failed: {e}"))?;

    unsafe { profiles.Register(&CLSID_SMARTKEY_TEXT_SERVICE) }
        .map_err(|e| format!("ITfInputProcessorProfiles::Register failed: {e}"))?;

    let profile_mgr: ITfInputProcessorProfileMgr = profiles
        .cast()
        .map_err(|e| format!("QI ITfInputProcessorProfileMgr failed: {e}"))?;

    let desc_w = to_wide_nul(SMARTKEY_DISPLAY_NAME);
    let icon_w = to_wide_nul(&dll_path.to_string_lossy());
    unsafe {
        profile_mgr.RegisterProfile(
            &CLSID_SMARTKEY_TEXT_SERVICE,
            LANGID_EN_US,
            &GUID_SMARTKEY_PROFILE_EN_US,
            &desc_w[..desc_w.len() - 1],
            &icon_w[..icon_w.len() - 1],
            0,
            HKL::default(),
            0,
            true,
            0,
        )
    }
    .map_err(|e| format!("RegisterProfile failed: {e}"))?;

    let _ = unsafe {
        profile_mgr.ActivateProfile(
            TF_PROFILETYPE_INPUTPROCESSOR,
            LANGID_EN_US,
            &CLSID_SMARTKEY_TEXT_SERVICE,
            &GUID_SMARTKEY_PROFILE_EN_US,
            HKL::default(),
            TF_IPPMF_ENABLEPROFILE | TF_IPPMF_FORSESSION,
        )
    };

    let cat_mgr: ITfCategoryMgr =
        unsafe { CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| format!("CoCreateInstance(ITfCategoryMgr) failed: {e}"))?;
    for cat in CATEGORIES {
        unsafe {
            cat_mgr.RegisterCategory(
                &CLSID_SMARTKEY_TEXT_SERVICE,
                cat,
                &CLSID_SMARTKEY_TEXT_SERVICE,
            )
        }
        .map_err(|e| format!("RegisterCategory failed: {e}"))?;
    }

    Ok(())
}

/// Unregisters SmartKey's TSF categories, `en-US` profile, and COM CLSID.
pub fn unregister_server(per_user_clsid: bool) -> Result<(), String> {
    let _com = ComApartmentGuard::enter();

    if let Ok(cat_mgr) =
        unsafe { CoCreateInstance::<_, ITfCategoryMgr>(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER) }
    {
        for cat in CATEGORIES {
            let _ = unsafe {
                cat_mgr.UnregisterCategory(
                    &CLSID_SMARTKEY_TEXT_SERVICE,
                    cat,
                    &CLSID_SMARTKEY_TEXT_SERVICE,
                )
            };
        }
    }

    if let Ok(profiles) = unsafe {
        CoCreateInstance::<_, ITfInputProcessorProfiles>(
            &CLSID_TF_InputProcessorProfiles,
            None,
            CLSCTX_INPROC_SERVER,
        )
    } {
        if let Ok(profile_mgr) = profiles.cast::<ITfInputProcessorProfileMgr>() {
            let _ = unsafe {
                profile_mgr.ActivateProfile(
                    TF_PROFILETYPE_INPUTPROCESSOR,
                    LANGID_EN_US,
                    &CLSID_SMARTKEY_TEXT_SERVICE,
                    &GUID_SMARTKEY_PROFILE_EN_US,
                    HKL::default(),
                    TF_IPPMF_DISABLEPROFILE | TF_IPPMF_FORSESSION,
                )
            };
            let _ = unsafe {
                profile_mgr.UnregisterProfile(
                    &CLSID_SMARTKEY_TEXT_SERVICE,
                    LANGID_EN_US,
                    &GUID_SMARTKEY_PROFILE_EN_US,
                    0,
                )
            };
        }
        let _ = unsafe { profiles.Unregister(&CLSID_SMARTKEY_TEXT_SERVICE) };
    }

    if per_user_clsid {
        delete_clsid_registry(HKEY_CURRENT_USER);
    } else {
        delete_clsid_registry(HKEY_LOCAL_MACHINE);
        delete_clsid_registry(HKEY_CURRENT_USER);
    }
    Ok(())
}

#[implement(IClassFactory)]
struct SmartKeyClassFactory;

impl IClassFactory_Impl for SmartKeyClassFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: windows_core::Ref<'_, windows_core::IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut c_void,
    ) -> windows_core::Result<()> {
        if ppvobject.is_null() || riid.is_null() {
            return Err(E_NOINTERFACE.into());
        }
        unsafe { *ppvobject = std::ptr::null_mut() };
        if !punkouter.is_null() {
            return Err(windows::Win32::Foundation::CLASS_E_NOAGGREGATION.into());
        }
        let service: windows_core::IUnknown = SmartKeyTextService::new().into();
        unsafe { service.query(riid, ppvobject).ok() }
    }

    fn LockServer(&self, flock: windows_core::BOOL) -> windows_core::Result<()> {
        if flock.as_bool() {
            increment_dll_ref();
        } else {
            decrement_dll_ref();
        }
        Ok(())
    }
}

/// # Safety
///
/// Invoked by the Windows loader according to `DllMain` ABI rules; `hinst` must be a valid module handle.
#[no_mangle]
pub unsafe extern "system" fn DllMain(
    hinst: HINSTANCE,
    reason: u32,
    _reserved: *mut c_void,
) -> i32 {
    match reason {
        DLL_PROCESS_ATTACH => {
            DLL_MODULE.store(hinst.0 as usize, Ordering::SeqCst);
            let _ = DisableThreadLibraryCalls(HMODULE(hinst.0));
        }
        DLL_PROCESS_DETACH => {}
        _ => {}
    }
    1
}

/// # Safety
///
/// Invoked by COM; any non-null `rclsid`, `riid`, and `ppv` pointers must be valid for read/write.
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if ppv.is_null() || rclsid.is_null() || riid.is_null() {
        return E_NOINTERFACE;
    }
    *ppv = std::ptr::null_mut();
    if *rclsid != CLSID_SMARTKEY_TEXT_SERVICE {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    let factory: windows_core::IUnknown = SmartKeyClassFactory.into();
    factory.query(riid, ppv)
}

/// # Safety
///
/// Invoked by COM to query DLL unload eligibility via atomic `DLL_REF_COUNT`.
#[no_mangle]
pub unsafe extern "system" fn DllCanUnloadNow() -> HRESULT {
    if DLL_REF_COUNT.load(Ordering::SeqCst) == 0 {
        S_OK
    } else {
        S_FALSE
    }
}

/// # Safety
///
/// Invoked by `regsvr32` or the SmartKey CLI with valid COM DLL module state.
#[no_mangle]
pub unsafe extern "system" fn DllRegisterServer() -> HRESULT {
    let Ok(dll_path) = current_dll_path() else {
        return SELFREG_E_CLASS;
    };
    if register_server(&dll_path, false).is_ok() {
        return S_OK;
    }
    if register_server(&dll_path, true).is_ok() {
        return S_OK;
    }
    SELFREG_E_CLASS
}

/// # Safety
///
/// Invoked by `regsvr32 /u` or the SmartKey CLI with valid COM DLL module state.
#[no_mangle]
pub unsafe extern "system" fn DllUnregisterServer() -> HRESULT {
    let _ = unregister_server(false);
    S_OK
}
