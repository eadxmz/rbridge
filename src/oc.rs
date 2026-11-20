use std::borrow::Cow;
use std::ffi::{c_char, c_int, c_void};
use std::io;
use std::mem::{size_of, transmute, zeroed};
use std::ptr::null_mut;
use winreg::enums::*;
use winreg::{RegKey, RegValue};

const RBRIDGE_REG: &str = r"SOFTWARE\RBridge";
const DISPLAY_CLASS: &str =
    r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
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

type FnCreateDxgiFactory1 = unsafe extern "system" fn(*const [u8; 16], *mut *mut c_void) -> i32;
type FnD3d11CreateDevice = unsafe extern "system" fn(
    *mut c_void,
    u32,
    *mut c_void,
    u32,
    *const u32,
    u32,
    u32,
    *mut *mut c_void,
    *mut u32,
    *mut *mut c_void,
) -> i32;

#[repr(C)]
struct DxgiAdapterDesc1 {
    description: [u16; 128],
    vendor_id: u32,
    device_id: u32,
    sub_sys_id: u32,
    revision: u32,
    dedicated_video_memory: usize,
    dedicated_system_memory: usize,
    shared_system_memory: usize,
    adapter_luid: [u32; 2],
    flags: u32,
}

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
    pub levels_summary: String,
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

struct DgpuWakeGuard {
    dxgi_lib: *mut c_void,
    d3d_lib: *mut c_void,
    factory: *mut c_void,
    adapter: *mut c_void,
    device: *mut c_void,
    ctx: *mut c_void,
}

impl DgpuWakeGuard {
    fn wake_amd() -> Self {
        let mut guard = Self {
            dxgi_lib: null_mut(),
            d3d_lib: null_mut(),
            factory: null_mut(),
            adapter: null_mut(),
            device: null_mut(),
            ctx: null_mut(),
        };
        unsafe {
            guard.dxgi_lib = LoadLibraryA(b"dxgi.dll\0".as_ptr() as *const c_char);
            guard.d3d_lib = LoadLibraryA(b"d3d11.dll\0".as_ptr() as *const c_char);
            if guard.dxgi_lib.is_null() || guard.d3d_lib.is_null() {
                return guard;
            }
            let Some(create_fac) = load_fn!(guard.dxgi_lib, b"CreateDXGIFactory1\0", FnCreateDxgiFactory1) else {
                return guard;
            };
            let Some(create_dev) = load_fn!(guard.d3d_lib, b"D3D11CreateDevice\0", FnD3d11CreateDevice) else {
                return guard;
            };

            let iid_factory1: [u8; 16] = [
                0x78, 0xae, 0x0a, 0x77, 0x6f, 0xf2, 0xba, 0x4d,
                0xa8, 0x29, 0x25, 0x3c, 0x83, 0xd1, 0xb3, 0x87,
            ];
            if create_fac(&iid_factory1, &mut guard.factory) != 0 || guard.factory.is_null() {
                return guard;
            }

            let fac_vtbl = *(guard.factory as *const *const usize);
            let enum_adapters1: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32 =
                transmute(*fac_vtbl.add(12));

            let mut i = 0u32;
            loop {
                let mut adp: *mut c_void = null_mut();
                if enum_adapters1(guard.factory, i, &mut adp) != 0 || adp.is_null() {
                    break;
                }
                let adp_vtbl = *(adp as *const *const usize);
                let adp_release: unsafe extern "system" fn(*mut c_void) -> u32 = transmute(*adp_vtbl.add(2));
                let get_desc1: unsafe extern "system" fn(*mut c_void, *mut DxgiAdapterDesc1) -> i32 =
                    transmute(*adp_vtbl.add(10));

                let mut desc: DxgiAdapterDesc1 = zeroed();
                if get_desc1(adp, &mut desc) == 0 && desc.vendor_id == 0x1002 {
                    guard.adapter = adp;
                    let fl = 0xb000u32;
                    let _ = create_dev(
                        adp,
                        0,
                        null_mut(),
                        0,
                        &fl,
                        1,
                        7,
                        &mut guard.device,
                        null_mut(),
                        &mut guard.ctx,
                    );
                    break;
                }
                adp_release(adp);
                i += 1;
            }
        }
        guard
    }
}

impl Drop for DgpuWakeGuard {
    fn drop(&mut self) {
        unsafe {
            for ptr in [self.ctx, self.device, self.adapter, self.factory] {
                if !ptr.is_null() {
                    let vtbl = *(ptr as *const *const usize);
                    let rel: unsafe extern "system" fn(*mut c_void) -> u32 = transmute(*vtbl.add(2));
                    rel(ptr);
                }
            }
            if !self.d3d_lib.is_null() {
                FreeLibrary(self.d3d_lib);
            }
            if !self.dxgi_lib.is_null() {
                FreeLibrary(self.dxgi_lib);
            }
        }
    }
}

struct AdlSession {
    _wake: DgpuWakeGuard,
    lib: *mut c_void,
    destroy: FnMainControlDestroy,
}

impl AdlSession {
    fn open() -> Result<Self, String> {
        let wake = DgpuWakeGuard::wake_amd();
        unsafe {
            let mut lib = LoadLibraryA(b"atiadlxx.dll\0".as_ptr() as *const c_char);
            if lib.is_null() {
                lib = LoadLibraryA(b"atiadlxy.dll\0".as_ptr() as *const c_char);
            }
            if lib.is_null() {
                return Err("Could not load atiadlxx.dll.".into());
            }

            let Some(create) = load_fn!(lib, b"ADL_Main_Control_Create\0", FnMainControlCreate) else {
                FreeLibrary(lib);
                return Err("Missing ADL_Main_Control_Create.".into());
            };
            let Some(destroy) = load_fn!(lib, b"ADL_Main_Control_Destroy\0", FnMainControlDestroy) else {
                FreeLibrary(lib);
                return Err("Missing ADL_Main_Control_Destroy.".into());
            };

            let rc = create(adl_malloc, 0);
            if rc != ADL_OK {
                FreeLibrary(lib);
                return Err(format!("ADL_Main_Control_Create failed (code {}).", rc));
            }
            Ok(Self {
                _wake: wake,
                lib,
                destroy,
            })
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

            fallback.ok_or_else(|| "No AMD adapter found in ADL.".into())
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
                    let mut parts = Vec::new();
                    for i in 0..n {
                        let off = 2 + i * 3;
                        parts.push(format!(
                            "L{}: {}/{} MHz ({} mV)",
                            i,
                            cur[off] / 100,
                            cur[off + 1] / 100,
                            cur[off + 2]
                        ));
                    }
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
                        levels_summary: parts.join(" | "),
                    });
                }
            }
        }

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
                        levels_summary: "OD6 Performance State".into(),
                    });
                }
            }
        }
    }

    Err("OverDrive API unavailable on this adapter.".into())
}

fn try_od5_set(
    session: &AdlSession,
    idx: c_int,
    engine_mhz: u32,
    memory_mhz: u32,
) -> Result<(u32, u32, &'static str), c_int> {
    unsafe {
        let (Some(params_fn), Some(levels_get), Some(levels_set)) = (
            load_fn!(session.lib, b"ADL_Overdrive5_ODParameters_Get\0", FnOd5ParamsGet),
            load_fn!(session.lib, b"ADL_Overdrive5_ODPerformanceLevels_Get\0", FnOd5LevelsGet),
            load_fn!(session.lib, b"ADL_Overdrive5_ODPerformanceLevels_Set\0", FnOd5LevelsSet),
        ) else {
            return Err(-99);
        };

        let mut params: AdlOdParameters = zeroed();
        params.i_size = size_of::<AdlOdParameters>() as c_int;
        if params_fn(idx, &mut params) != ADL_OK || params.i_number_of_performance_levels <= 0 {
            return Err(-98);
        }

        let n = (params.i_number_of_performance_levels as usize).min(16);
        let buf_bytes = (8 + n * 12) as c_int;
        let mut base = vec![0i32; 2 + n * 3];
        base[0] = buf_bytes;

        if levels_get(idx, 0, base.as_mut_ptr() as *mut c_void) != ADL_OK {
            return Err(-97);
        }

        let last = 2 + (n - 1) * 3;
        let orig_e = base[last];
        let orig_m = base[last + 1];

        let target_e = if engine_mhz > 0 {
            (engine_mhz as i32 * 100).clamp(params.s_engine_clock.i_min, params.s_engine_clock.i_max)
        } else {
            orig_e
        };
        let target_m = if memory_mhz > 0 {
            (memory_mhz as i32 * 100).clamp(params.s_memory_clock.i_min, params.s_memory_clock.i_max)
        } else {
            orig_m
        };

        let mut buf = base.clone();
        buf[last] = target_e;
        buf[last + 1] = target_m;
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

        if target_e != orig_e && target_m != orig_m {
            let mut core_only = base;
            core_only[last] = target_e;
            for i in 0..(n - 1) {
                let off = 2 + i * 3;
                if core_only[off] > target_e {
                    core_only[off] = target_e;
                }
            }
            if levels_set(idx, core_only.as_mut_ptr() as *mut c_void) == ADL_OK {
                return Ok((
                    (target_e / 100) as u32,
                    (orig_m / 100) as u32,
                    "Overdrive5 (Core-only; OEM VBIOS locked MCLK)",
                ));
            }
        }

        Err(rc)
    }
}

fn try_od6_set(
    session: &AdlSession,
    idx: c_int,
    engine_mhz: u32,
    memory_mhz: u32,
) -> Result<(u32, u32, &'static str), c_int> {
    unsafe {
        let (Some(caps_fn), Some(state_get), Some(state_set)) = (
            load_fn!(session.lib, b"ADL_Overdrive6_Capabilities_Get\0", FnOd6CapsGet),
            load_fn!(session.lib, b"ADL_Overdrive6_StateInfo_Get\0", FnOd6StateInfoGet),
            load_fn!(session.lib, b"ADL_Overdrive6_State_Set\0", FnOd6StateSet),
        ) else {
            return Err(-99);
        };

        let mut caps: AdlOd6Capabilities = zeroed();
        if caps_fn(idx, &mut caps) != ADL_OK {
            return Err(-98);
        }

        let mut state = [0i32; 7];
        state[0] = 2;
        if state_get(
            idx,
            ADL_OD6_GETSTATEINFO_CUSTOM_PERFORMANCE,
            state.as_mut_ptr() as *mut c_void,
        ) != ADL_OK
        {
            return Err(-97);
        }

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
            Ok(((target_e / 100) as u32, (target_m / 100) as u32, "Overdrive6"))
        } else {
            Err(rc)
        }
    }
}

fn le24_10khz(mhz: u32) -> [u8; 3] {
    let val = mhz * 100;
    let b = val.to_le_bytes();
    [b[0], b[1], b[2]]
}

// GCN 1.0 SoftPowerPlayTable bypass: patches PP_PhmSoftPowerPlayTable directly when OEM VBIOS locks ADL
pub fn apply_soft_powerplay_mod(
    stock_engine_mhz: u32,
    stock_memory_mhz: u32,
    new_engine_mhz: u32,
    new_memory_mhz: u32,
) -> Result<usize, String> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let class_key = hklm
        .open_subkey_with_flags(DISPLAY_CLASS, KEY_READ)
        .map_err(|e| format!("Cannot open Display Class: {}", e))?;

    let mut needle = [0u8; 6];
    needle[..3].copy_from_slice(&le24_10khz(stock_engine_mhz));
    needle[3..].copy_from_slice(&le24_10khz(stock_memory_mhz));

    let mut replacement = [0u8; 6];
    replacement[..3].copy_from_slice(&le24_10khz(new_engine_mhz));
    replacement[3..].copy_from_slice(&le24_10khz(new_memory_mhz));

    for sub in class_key.enum_keys().filter_map(|k| k.ok()) {
        if sub.len() != 4 || !sub.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let full_path = format!(r"{}\{}", DISPLAY_CLASS, sub);
        let Ok(gpu_key) = hklm.open_subkey_with_flags(&full_path, KEY_READ | KEY_WRITE) else {
            continue;
        };
        let matching_id = gpu_key
            .get_value::<String, _>("MatchingDeviceId")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !matching_id.contains("ven_1002") {
            continue;
        }

        // Read clean backup if available, else create it from stock PP_PhmSoftPowerPlayTable
        let mut table = if let Ok(bak) = gpu_key.get_raw_value("PP_PhmSoftPowerPlayTable_BAK") {
            bak.bytes.into_owned()
        } else if let Ok(cur) = gpu_key.get_raw_value("PP_PhmSoftPowerPlayTable") {
            let bytes = cur.bytes.into_owned();
            let _ = gpu_key.set_raw_value(
                "PP_PhmSoftPowerPlayTable_BAK",
                &RegValue {
                    bytes: Cow::Borrowed(&bytes),
                    vtype: REG_BINARY,
                },
            );
            bytes
        } else {
            return Err("PP_PhmSoftPowerPlayTable not present in AMD driver registry key.".into());
        };

        let mut hits = 0usize;
        let mut i = 0usize;
        while i + 6 <= table.len() {
            if table[i..i + 6] == needle {
                table[i..i + 6].copy_from_slice(&replacement);
                hits += 1;
                i += 6;
            } else {
                i += 1;
            }
        }

        if hits == 0 {
            return Err(format!(
                "Could not locate stock {}/{} MHz DPM signature in PP_PhmSoftPowerPlayTable.",
                stock_engine_mhz, stock_memory_mhz
            ));
        }

        gpu_key
            .set_raw_value(
                "PP_PhmSoftPowerPlayTable",
                &RegValue {
                    bytes: Cow::Borrowed(&table),
                    vtype: REG_BINARY,
                },
            )
            .map_err(|e| format!("Failed writing PP_PhmSoftPowerPlayTable: {}", e))?;

        return Ok(hits);
    }

    Err("AMD VEN_1002 registry key not found.".into())
}

pub fn restore_soft_powerplay_table() -> bool {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let Ok(class_key) = hklm.open_subkey_with_flags(DISPLAY_CLASS, KEY_READ) else {
        return false;
    };
    for sub in class_key.enum_keys().filter_map(|k| k.ok()) {
        if sub.len() != 4 || !sub.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let full_path = format!(r"{}\{}", DISPLAY_CLASS, sub);
        let Ok(gpu_key) = hklm.open_subkey_with_flags(&full_path, KEY_READ | KEY_WRITE) else {
            continue;
        };
        if let Ok(bak) = gpu_key.get_raw_value("PP_PhmSoftPowerPlayTable_BAK") {
            let _ = gpu_key.set_raw_value("PP_PhmSoftPowerPlayTable", &bak);
            let _ = gpu_key.delete_value("PP_PhmSoftPowerPlayTable_BAK");
            return true;
        }
    }
    false
}

pub fn apply_oc_clocks(engine_mhz: u32, memory_mhz: u32) -> Result<(u32, u32, &'static str), String> {
    let session = AdlSession::open()?;
    let (idx, _) = session.find_amd_adapter()?;

    match try_od5_set(&session, idx, engine_mhz, memory_mhz) {
        Ok(res) => Ok(res),
        Err(od5_rc) => match try_od6_set(&session, idx, engine_mhz, memory_mhz) {
            Ok(res) => Ok(res),
            Err(od6_rc) => Err(format!(
                "ADL locked by OEM VBIOS (OD5 code {}, OD6 code {})",
                od5_rc, od6_rc
            )),
        },
    }
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
