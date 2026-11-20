use std::ffi::{c_char, c_int, c_void};
use std::io;
use std::mem::{size_of, transmute, zeroed};
use std::ptr::null_mut;
use winreg::enums::*;
use winreg::RegKey;

const RBRIDGE_REG: &str = r"SOFTWARE\RBridge";
const ADL_MAX_PATH: usize = 256;
const ADL_OK: c_int = 0;
const ADL_OD6_GETSTATEINFO_DEFAULT_PERFORMANCE: c_int = 1;
const ADL_OD6_GETSTATEINFO_CUSTOM_PERFORMANCE: c_int = 2;
const ADL_OD6_SETSTATE_PERFORMANCE: c_int = 1;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryA(lp_lib_file_name: *const c_char) -> *mut c_void;
    fn GetProcAddress(h_module: *mut c_void, lp_proc_name: *const c_char) -> *mut c_void;
    fn FreeLibrary(h_lib_module: *mut c_void) -> c_int;
}

extern "C" {
    fn malloc(size: usize) -> *mut c_void;
}

unsafe extern "C" fn adl_malloc(size: c_int) -> *mut c_void {
    if size <= 0 {
        null_mut()
    } else {
        malloc(size as usize)
    }
}

type AdlMallocCb = unsafe extern "C" fn(c_int) -> *mut c_void;
type FnMainControlCreate = unsafe extern "C" fn(AdlMallocCb, c_int) -> c_int;
type FnMainControlDestroy = unsafe extern "C" fn() -> c_int;
type FnAdapterNumGet = unsafe extern "C" fn(*mut c_int) -> c_int;
type FnAdapterInfoGet = unsafe extern "C" fn(*mut AdapterInfo, c_int) -> c_int;
type FnOd5ParamsGet = unsafe extern "C" fn(c_int, *mut AdlOdParameters) -> c_int;
type FnOd5LevelsGet = unsafe extern "C" fn(c_int, c_int, *mut c_void) -> c_int;
type FnOd5LevelsSet = unsafe extern "C" fn(c_int, *mut c_void) -> c_int;
type FnOd6CapsGet = unsafe extern "C" fn(c_int, *mut AdlOd6Capabilities) -> c_int;
type FnOd6StateInfoGet = unsafe extern "C" fn(c_int, c_int, *mut c_void) -> c_int;
type FnOd6StateSet = unsafe extern "C" fn(c_int, c_int, *mut c_void) -> c_int;

#[repr(C)]
#[derive(Clone, Copy)]
struct AdapterInfo {
    i_size: c_int,
    i_adapter_index: c_int,
    str_udid: [u8; ADL_MAX_PATH],
    i_bus_number: c_int,
    i_device_number: c_int,
    i_function_number: c_int,
    i_vendor_id: c_int,
    str_adapter_name: [u8; ADL_MAX_PATH],
    str_display_name: [u8; ADL_MAX_PATH],
    i_present: c_int,
    i_exist: c_int,
    str_driver_path: [u8; ADL_MAX_PATH],
    str_driver_path_ext: [u8; ADL_MAX_PATH],
    str_pnp_string: [u8; ADL_MAX_PATH],
    i_os_display_index: c_int,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AdlOdParameterRange {
    i_min: c_int,
    i_max: c_int,
    i_step: c_int,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AdlOdParameters {
    i_size: c_int,
    i_number_of_performance_levels: c_int,
    i_activity_reporting_supported: c_int,
    i_discrete_performance_levels: c_int,
    i_reserved: c_int,
    s_engine_clock: AdlOdParameterRange,
    s_memory_clock: AdlOdParameterRange,
    s_vddc: AdlOdParameterRange,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AdlOd6Capabilities {
    i_capabilities: c_int,
    i_supported_states: c_int,
    s_engine_clock_range: AdlOdParameterRange,
    s_memory_clock_range: AdlOdParameterRange,
    i_ext_value: c_int,
    i_ext_mask: c_int,
}

pub struct OcStatus {
    pub adapter_name: String,
    pub backend: &'static str,
    pub engine_mhz: u32,
    pub memory_mhz: u32,
    pub default_engine_mhz: u32,
    pub default_memory_mhz: u32,
    pub min_engine_mhz: u32,
    pub max_engine_mhz: u32,
    pub min_memory_mhz: u32,
    pub max_memory_mhz: u32,
}

pub struct SavedOc {
    pub enabled: bool,
    pub engine_mhz: u32,
    pub memory_mhz: u32,
}

macro_rules! load_fn {
    ($lib:expr, $name:literal, $ty:ty) => {{
        let ptr = GetProcAddress($lib, $name.as_ptr() as *const c_char);
        if ptr.is_null() {
            None
        } else {
            Some(transmute::<*mut c_void, $ty>(ptr))
        }
    }};
}

fn cstr_from_buf(buf: &[u8]) -> String {
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..len]).into_owned()
}

struct AdlSession {
    lib: *mut c_void,
    destroy: FnMainControlDestroy,
}

impl AdlSession {
    fn open() -> Result<Self, String> {
        unsafe {
            let mut lib = LoadLibraryA(b"atiadlxx.dll\0".as_ptr() as *const c_char);
            if lib.is_null() {
                lib = LoadLibraryA(b"atiadlxy.dll\0".as_ptr() as *const c_char);
            }
            if lib.is_null() {
                return Err("Could not load atiadlxx.dll (ensure AMD driver is installed).".into());
            }

            let Some(create) = load_fn!(lib, b"ADL_Main_Control_Create\0", FnMainControlCreate) else {
                FreeLibrary(lib);
                return Err("Missing ADL_Main_Control_Create in atiadlxx.dll.".into());
            };
            let Some(destroy) = load_fn!(lib, b"ADL_Main_Control_Destroy\0", FnMainControlDestroy) else {
                FreeLibrary(lib);
                return Err("Missing ADL_Main_Control_Destroy in atiadlxx.dll.".into());
            };

            // Pass 0 so headless Enduro dGPUs (no directly attached monitor) are enumerated
            let rc = create(adl_malloc, 0);
            if rc != ADL_OK {
                FreeLibrary(lib);
                return Err(format!("ADL_Main_Control_Create failed (code {}).", rc));
            }
            Ok(Self { lib, destroy })
        }
    }

    fn find_amd_adapter(&self) -> Result<(c_int, String), String> {
        unsafe {
            let num_fn = load_fn!(self.lib, b"ADL_Adapter_NumberOfAdapters_Get\0", FnAdapterNumGet)
                .ok_or("Missing ADL_Adapter_NumberOfAdapters_Get")?;
            let info_fn = load_fn!(self.lib, b"ADL_Adapter_AdapterInfo_Get\0", FnAdapterInfoGet)
                .ok_or("Missing ADL_Adapter_AdapterInfo_Get")?;

            let mut count: c_int = 0;
            if num_fn(&mut count) != ADL_OK || count <= 0 {
                return Err("ADL reported 0 adapters.".into());
            }

            let mut infos: Vec<AdapterInfo> = vec![zeroed(); count as usize];
            for item in &mut infos {
                item.i_size = size_of::<AdapterInfo>() as c_int;
            }
            let total_bytes = (infos.len() * size_of::<AdapterInfo>()) as c_int;
            if info_fn(infos.as_mut_ptr(), total_bytes) != ADL_OK {
                return Err("ADL_Adapter_AdapterInfo_Get failed.".into());
            }

            // Prefer exact Oland 8670M / 8600M / 8700M (DEV_6660 / DEV_6600)
            let mut fallback: Option<(c_int, String)> = None;
            for info in &infos {
                let pnp = cstr_from_buf(&info.str_pnp_string).to_ascii_lowercase();
                let name = cstr_from_buf(&info.str_adapter_name);
                let is_amd = info.i_vendor_id == 1002
                    || info.i_vendor_id == 0x1002
                    || pnp.contains("ven_1002");
                if !is_amd {
                    continue;
                }
                if pnp.contains("dev_6660") || pnp.contains("dev_6600") {
                    return Ok((info.i_adapter_index, name));
                }
                if fallback.is_none() {
                    fallback = Some((info.i_adapter_index, name));
                }
            }

            fallback.ok_or_else(|| "No AMD adapter found in ADL enumeration.".into())
        }
    }
}

impl Drop for AdlSession {
    fn drop(&mut self) {
        unsafe {
            let _ = (self.destroy)();
            FreeLibrary(self.lib);
        }
    }
}

pub fn query_oc_status() -> Result<OcStatus, String> {
    let session = AdlSession::open()?;
    let (idx, name) = session.find_amd_adapter()?;

    unsafe {
        // Try Overdrive5 first (native GCN 1.0 Oland)
        if let (Some(params_fn), Some(levels_get)) = (
            load_fn!(session.lib, b"ADL_Overdrive5_ODParameters_Get\0", FnOd5ParamsGet),
            load_fn!(session.lib, b"ADL_Overdrive5_ODPerformanceLevels_Get\0", FnOd5LevelsGet),
        ) {
            let mut params: AdlOdParameters = zeroed();
            params.i_size = size_of::<AdlOdParameters>() as c_int;
            if params_fn(idx, &mut params) == ADL_OK && params.i_number_of_performance_levels > 0 {
                let n = (params.i_number_of_performance_levels as usize).min(16);
                let buf_bytes = (8 + n * 12) as c_int;

                let mut cur = vec![0i32; 2 + n * 3];
                cur[0] = buf_bytes;
                let mut def = vec![0i32; 2 + n * 3];
                def[0] = buf_bytes;

                if levels_get(idx, 0, cur.as_mut_ptr() as *mut c_void) == ADL_OK {
                    let _ = levels_get(idx, 1, def.as_mut_ptr() as *mut c_void);
                    let last = 2 + (n - 1) * 3;
                    return Ok(OcStatus {
                        adapter_name: name,
                        backend: "Overdrive5",
                        engine_mhz: (cur[last].max(0) / 100) as u32,
                        memory_mhz: (cur[last + 1].max(0) / 100) as u32,
                        default_engine_mhz: (def[last].max(0) / 100) as u32,
                        default_memory_mhz: (def[last + 1].max(0) / 100) as u32,
                        min_engine_mhz: (params.s_engine_clock.i_min.max(0) / 100) as u32,
                        max_engine_mhz: (params.s_engine_clock.i_max.max(0) / 100) as u32,
                        min_memory_mhz: (params.s_memory_clock.i_min.max(0) / 100) as u32,
                        max_memory_mhz: (params.s_memory_clock.i_max.max(0) / 100) as u32,
                    });
                }
            }
        }

        // Fallback: Overdrive6
        if let (Some(caps_fn), Some(state_get)) = (
            load_fn!(session.lib, b"ADL_Overdrive6_Capabilities_Get\0", FnOd6CapsGet),
            load_fn!(session.lib, b"ADL_Overdrive6_StateInfo_Get\0", FnOd6StateInfoGet),
        ) {
            let mut caps: AdlOd6Capabilities = zeroed();
            if caps_fn(idx, &mut caps) == ADL_OK {
                let mut cur = [0i32; 7];
                let mut def = [0i32; 7];
                cur[0] = 2;
                def[0] = 2;
                if state_get(
                    idx,
                    ADL_OD6_GETSTATEINFO_CUSTOM_PERFORMANCE,
                    cur.as_mut_ptr() as *mut c_void,
                ) == ADL_OK
                {
                    let _ = state_get(
                        idx,
                        ADL_OD6_GETSTATEINFO_DEFAULT_PERFORMANCE,
                        def.as_mut_ptr() as *mut c_void,
                    );
                    let lvl_off = if cur[0] >= 2 { 5 } else { 3 };
                    return Ok(OcStatus {
                        adapter_name: name,
                        backend: "Overdrive6",
                        engine_mhz: (cur[lvl_off].max(0) / 100) as u32,
                        memory_mhz: (cur[lvl_off + 1].max(0) / 100) as u32,
                        default_engine_mhz: (def[lvl_off].max(0) / 100) as u32,
                        default_memory_mhz: (def[lvl_off + 1].max(0) / 100) as u32,
                        min_engine_mhz: (caps.s_engine_clock_range.i_min.max(0) / 100) as u32,
                        max_engine_mhz: (caps.s_engine_clock_range.i_max.max(0) / 100) as u32,
                        min_memory_mhz: (caps.s_memory_clock_range.i_min.max(0) / 100) as u32,
                        max_memory_mhz: (caps.s_memory_clock_range.i_max.max(0) / 100) as u32,
                    });
                }
            }
        }
    }

    Err("OverDrive API unavailable on this adapter (verify PP_ODNFeatureEnable=1 and reboot).".into())
}

pub fn apply_oc_clocks(engine_mhz: u32, memory_mhz: u32) -> Result<(u32, u32, &'static str), String> {
    let session = AdlSession::open()?;
    let (idx, _) = session.find_amd_adapter()?;

    unsafe {
        // Overdrive5 path
        if let (Some(params_fn), Some(levels_get), Some(levels_set)) = (
            load_fn!(session.lib, b"ADL_Overdrive5_ODParameters_Get\0", FnOd5ParamsGet),
            load_fn!(session.lib, b"ADL_Overdrive5_ODPerformanceLevels_Get\0", FnOd5LevelsGet),
            load_fn!(session.lib, b"ADL_Overdrive5_ODPerformanceLevels_Set\0", FnOd5LevelsSet),
        ) {
            let mut params: AdlOdParameters = zeroed();
            params.i_size = size_of::<AdlOdParameters>() as c_int;
            if params_fn(idx, &mut params) == ADL_OK && params.i_number_of_performance_levels > 0 {
                let n = (params.i_number_of_performance_levels as usize).min(16);
                let buf_bytes = (8 + n * 12) as c_int;
                let mut buf = vec![0i32; 2 + n * 3];
                buf[0] = buf_bytes;

                if levels_get(idx, 0, buf.as_mut_ptr() as *mut c_void) == ADL_OK {
                    let last = 2 + (n - 1) * 3;
                    let target_e = if engine_mhz > 0 {
                        (engine_mhz as i32 * 100)
                            .clamp(params.s_engine_clock.i_min, params.s_engine_clock.i_max)
                    } else {
                        buf[last]
                    };
                    let target_m = if memory_mhz > 0 {
                        (memory_mhz as i32 * 100)
                            .clamp(params.s_memory_clock.i_min, params.s_memory_clock.i_max)
                    } else {
                        buf[last + 1]
                    };

                    buf[last] = target_e;
                    buf[last + 1] = target_m;

                    // Keep lower performance states <= highest state so ADL never rejects
                    for i in 0..(n - 1) {
                        let off = 2 + i * 3;
                        if buf[off] > target_e {
                            buf[off] = target_e;
                        }
                        if buf[off + 1] > target_m {
                            buf[off + 1] = target_m;
                        }
                    }

                    let rc = levels_set(idx, buf.as_mut_ptr() as *mut c_void);
                    if rc == ADL_OK {
                        return Ok(((target_e / 100) as u32, (target_m / 100) as u32, "Overdrive5"));
                    }
                    return Err(format!("ADL_Overdrive5_ODPerformanceLevels_Set failed (code {}).", rc));
                }
            }
        }

        // Overdrive6 fallback path
        if let (Some(caps_fn), Some(state_get), Some(state_set)) = (
            load_fn!(session.lib, b"ADL_Overdrive6_Capabilities_Get\0", FnOd6CapsGet),
            load_fn!(session.lib, b"ADL_Overdrive6_StateInfo_Get\0", FnOd6StateInfoGet),
            load_fn!(session.lib, b"ADL_Overdrive6_State_Set\0", FnOd6StateSet),
        ) {
            let mut caps: AdlOd6Capabilities = zeroed();
            if caps_fn(idx, &mut caps) == ADL_OK {
                let mut state = [0i32; 7];
                state[0] = 2;
                if state_get(
                    idx,
                    ADL_OD6_GETSTATEINFO_CUSTOM_PERFORMANCE,
                    state.as_mut_ptr() as *mut c_void,
                ) == ADL_OK
                {
                    let lvl_off = if state[0] >= 2 { 5 } else { 3 };
                    let target_e = if engine_mhz > 0 {
                        (engine_mhz as i32 * 100)
                            .clamp(caps.s_engine_clock_range.i_min, caps.s_engine_clock_range.i_max)
                    } else {
                        state[lvl_off]
                    };
                    let target_m = if memory_mhz > 0 {
                        (memory_mhz as i32 * 100)
                            .clamp(caps.s_memory_clock_range.i_min, caps.s_memory_clock_range.i_max)
                    } else {
                        state[lvl_off + 1]
                    };
                    state[lvl_off] = target_e;
                    state[lvl_off + 1] = target_m;

                    let rc = state_set(
                        idx,
                        ADL_OD6_SETSTATE_PERFORMANCE,
                        state.as_mut_ptr() as *mut c_void,
                    );
                    if rc == ADL_OK {
                        return Ok(((target_e / 100) as u32, (target_m / 100) as u32, "Overdrive6"));
                    }
                    return Err(format!("ADL_Overdrive6_State_Set failed (code {}).", rc));
                }
            }
        }
    }

    Err("Failed to apply OverDrive clocks via ADL.".into())
}

pub fn save_oc_profile(enabled: bool, engine_mhz: u32, memory_mhz: u32) -> io::Result<()> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let (key, _) = hklm.create_subkey(RBRIDGE_REG)?;
    key.set_value("OcEnabled", &(if enabled { 1u32 } else { 0u32 }))?;
    key.set_value("OcEngineMhz", &engine_mhz)?;
    key.set_value("OcMemoryMhz", &memory_mhz)?;
    Ok(())
}

pub fn read_saved_oc() -> SavedOc {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let Ok(key) = hklm.open_subkey_with_flags(RBRIDGE_REG, KEY_READ) else {
        return SavedOc {
            enabled: false,
            engine_mhz: 0,
            memory_mhz: 0,
        };
    };
    SavedOc {
        enabled: key.get_value::<u32, _>("OcEnabled").unwrap_or(0) != 0,
        engine_mhz: key.get_value::<u32, _>("OcEngineMhz").unwrap_or(0),
        memory_mhz: key.get_value::<u32, _>("OcMemoryMhz").unwrap_or(0),
    }
}

pub fn apply_saved_oc() {
    let cfg = read_saved_oc();
    if cfg.enabled && (cfg.engine_mhz > 0 || cfg.memory_mhz > 0) {
        let _ = apply_oc_clocks(cfg.engine_mhz, cfg.memory_mhz);
    }
}
