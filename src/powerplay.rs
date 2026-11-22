#![allow(dead_code)]

use std::ffi::{c_char, c_int, c_void};
use std::fs;
use std::mem::{size_of, transmute, zeroed};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::ptr::null_mut;
use winreg::enums::*;
use winreg::{RegKey, RegValue};

use crate::tweaks::{find_amd_class_subkey, RBRIDGE_REG};

const CREATE_NO_WINDOW: u32 = 0x08000000;
const ADL_MAX_PATH: usize = 256;
const ADL_OK: c_int = 0;

// Official AMD adl_defines.h constants for Overdrive6
const ADL_OD6_GETSTATEINFO_DEFAULT_PERFORMANCE: c_int = 0x00000001;
const ADL_OD6_GETSTATEINFO_CUSTOM_PERFORMANCE: c_int = 0x00000004;
const ADL_OD6_SETSTATE_PERFORMANCE: c_int = 0x00000001;

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
type FnAdl2MainControlCreate = unsafe extern "C" fn(AdlMallocCb, c_int, *mut *mut c_void) -> c_int;
type FnAdl2MainControlDestroy = unsafe extern "C" fn(*mut c_void) -> c_int;
type FnAdapterNumGet = unsafe extern "C" fn(*mut c_int) -> c_int;
type FnAdapterInfoGet = unsafe extern "C" fn(*mut AdapterInfo, c_int) -> c_int;
type FnOverdriveCaps = unsafe extern "C" fn(c_int, *mut c_int, *mut c_int, *mut c_int) -> c_int;

type FnOd5ParamsGet = unsafe extern "C" fn(c_int, *mut AdlOdParameters) -> c_int;
type FnOd5LevelsGet = unsafe extern "C" fn(c_int, c_int, *mut c_void) -> c_int;
type FnOd5LevelsSet = unsafe extern "C" fn(c_int, *mut c_void) -> c_int;
type FnAdl2Od5LevelsSet = unsafe extern "C" fn(*mut c_void, c_int, *mut c_void) -> c_int;
type FnOd5ActivityGet = unsafe extern "C" fn(c_int, *mut AdlPmActivity) -> c_int;
type FnOd5TempGet = unsafe extern "C" fn(c_int, c_int, *mut AdlTemperature) -> c_int;
type FnOd5PowerControlSet = unsafe extern "C" fn(c_int, c_int) -> c_int;

type FnOd6CapsGet = unsafe extern "C" fn(c_int, *mut AdlOd6Capabilities) -> c_int;
type FnOd6StateInfoGet = unsafe extern "C" fn(c_int, c_int, *mut c_void) -> c_int;
type FnOd6StateSet = unsafe extern "C" fn(c_int, c_int, *mut c_void) -> c_int;
type FnAdl2Od6StateInfoGet = unsafe extern "C" fn(*mut c_void, c_int, c_int, *mut c_void) -> c_int;
type FnAdl2Od6StateSet = unsafe extern "C" fn(*mut c_void, c_int, c_int, *mut c_void) -> c_int;
type FnOd6PowerControlSet = unsafe extern "C" fn(c_int, c_int) -> c_int;

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
struct AdlPmActivity {
    i_size: c_int,
    i_engine_clock: c_int,
    i_memory_clock: c_int,
    i_vddc: c_int,
    i_activity_percent: c_int,
    i_current_performance_level: c_int,
    i_current_bus_speed: c_int,
    i_current_bus_lanes: c_int,
    i_maximum_bus_lanes: c_int,
    i_reserved: c_int,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AdlTemperature {
    i_size: c_int,
    i_temperature: c_int,
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
    _pad: [c_int; 8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OcMode {
    Standard = 0,
    UnofficialPowerPlay = 1,
    UnofficialFlatClocks = 2,
}

impl OcMode {
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::UnofficialPowerPlay,
            2 => Self::UnofficialFlatClocks,
            _ => Self::Standard,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Standard => "Standard ADL",
            Self::UnofficialPowerPlay => "Unofficial Mode 1 (PowerPlay)",
            Self::UnofficialFlatClocks => "Unofficial Mode 2 (Flat Clocks)",
        }
    }
}

#[derive(Clone, Debug)]
pub struct AdlDiagInfo {
    pub adapter_name: String,
    pub od_supported: i32,
    pub od_enabled: i32,
    pub od_version: i32,
    pub backend: String,
    pub cur_core_mhz: u32,
    pub cur_mem_mhz: u32,
    pub min_core_mhz: u32,
    pub max_core_mhz: u32,
    pub min_mem_mhz: u32,
    pub max_mem_mhz: u32,
    pub temp_c: Option<u32>,
    pub load_pct: Option<u32>,
    pub states_summary: String,
}

#[derive(Clone, Debug)]
pub struct GpuTelemetry {
    pub gpu_name: String,
    pub driver_version: String,
    pub driver_stack: String,
    pub live_core_mhz: u32,
    pub live_mem_mhz: u32,
    pub temp_c: Option<u32>,
    pub load_pct: Option<u32>,
    pub soft_pp_active: bool,
    pub boot_oc_enabled: bool,
    pub saved_oc_mode: OcMode,
    pub saved_oc_core: u32,
    pub saved_oc_mem: u32,
}

#[derive(Clone, Debug)]
pub struct DpmState {
    pub offset: usize,
    pub core_mhz: u32,
    pub mem_mhz: u32,
    pub vddc_raw: u16,
}

#[derive(Clone, Debug)]
pub struct ParsedPowerPlay {
    pub table_bytes: Vec<u8>,
    pub format_rev: u8,
    pub content_rev: u8,
    pub states: Vec<DpmState>,
    pub boost_core_mhz: u32,
    pub boost_mem_mhz: u32,
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
    _wake: Option<DgpuWakeGuard>,
    lib: *mut c_void,
    destroy: FnMainControlDestroy,
    adl2_ctx: *mut c_void,
    adl2_destroy: Option<FnAdl2MainControlDestroy>,
}

impl AdlSession {
    fn open(wake_gpu: bool) -> Result<Self, String> {
        let wake = if wake_gpu {
            Some(DgpuWakeGuard::wake_amd())
        } else {
            None
        };
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

            let mut adl2_ctx: *mut c_void = null_mut();
            let adl2_destroy = load_fn!(lib, b"ADL2_Main_Control_Destroy\0", FnAdl2MainControlDestroy);
            if let Some(create2) = load_fn!(lib, b"ADL2_Main_Control_Create\0", FnAdl2MainControlCreate) {
                if create2(adl_malloc, 0, &mut adl2_ctx) != ADL_OK {
                    adl2_ctx = null_mut();
                }
            }

            Ok(Self {
                _wake: wake,
                lib,
                destroy,
                adl2_ctx,
                adl2_destroy,
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
            if info_fn(
                infos.as_mut_ptr(),
                (infos.len() * size_of::<AdapterInfo>()) as c_int,
            ) != ADL_OK
            {
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

    fn get_od_caps(&self, idx: c_int) -> (i32, i32, i32) {
        unsafe {
            let mut sup = 0;
            let mut en = 0;
            let mut ver = 0;
            if let Some(f) = load_fn!(self.lib, b"ADL_Overdrive_Caps\0", FnOverdriveCaps) {
                let _ = f(idx, &mut sup, &mut en, &mut ver);
            }
            (sup, en, ver)
        }
    }

    fn read_sensors(&self, idx: c_int) -> (Option<u32>, Option<u32>) {
        unsafe {
            let mut temp_c = None;
            if let Some(tfn) = load_fn!(self.lib, b"ADL_Overdrive5_Temperature_Get\0", FnOd5TempGet) {
                let mut t = AdlTemperature {
                    i_size: size_of::<AdlTemperature>() as c_int,
                    i_temperature: 0,
                };
                if tfn(idx, 0, &mut t) == ADL_OK && t.i_temperature > 0 {
                    temp_c = Some((t.i_temperature / 1000) as u32);
                }
            }
            let mut load_pct = None;
            if let Some(afn) = load_fn!(self.lib, b"ADL_Overdrive5_CurrentActivity_Get\0", FnOd5ActivityGet) {
                let mut act: AdlPmActivity = zeroed();
                act.i_size = size_of::<AdlPmActivity>() as c_int;
                if afn(idx, &mut act) == ADL_OK && (0..=100).contains(&act.i_activity_percent) {
                    load_pct = Some(act.i_activity_percent as u32);
                }
            }
            (temp_c, load_pct)
        }
    }
}

impl Drop for AdlSession {
    fn drop(&mut self) {
        unsafe {
            if !self.adl2_ctx.is_null() {
                if let Some(d2) = self.adl2_destroy {
                    let _ = d2(self.adl2_ctx);
                }
            }
            let _ = (self.destroy)();
            FreeLibrary(self.lib);
        }
    }
}

pub fn unlock_unofficial_registry_flags() -> Result<String, String> {
    let amd_path = find_amd_class_subkey()
        .ok_or_else(|| "AMD VEN_1002 Display Class registry key not found.".to_string())?;
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let gpu_key = hklm
        .open_subkey_with_flags(&amd_path, KEY_READ | KEY_WRITE)
        .map_err(|e| format!("Cannot write AMD key (run as Admin): {}", e))?;

    let _ = gpu_key.set_value("PP_EnableOverdrive", &1u32);
    let _ = gpu_key.set_value("PP_ThermalAutoThrottlingEnable", &0u32);
    let _ = gpu_key.set_value("PP_SclkDeepSleepDisable", &1u32);
    let _ = gpu_key.set_value("EnableUlps", &0u32);
    let _ = gpu_key.set_value("EnableUlps_NA", &"0");
    let _ = gpu_key.set_value("KMD_EnableOverdrive", &1u32);
    let _ = gpu_key.set_value("PP_PhmUseDummyBackEnd", &0u32);

    Ok("Enabled PP_EnableOverdrive, KMD_EnableOverdrive & ULPS=0 in AMD driver registry.".into())
}

pub fn sync_msi_afterburner_unofficial_cfg() -> Result<String, String> {
    let candidates = [
        r"C:\Program Files (x86)\MSI Afterburner\MSIAfterburner.cfg",
        r"C:\Program Files\MSI Afterburner\MSIAfterburner.cfg",
    ];
    let Some(path) = candidates.iter().map(Path::new).find(|p| p.exists()) else {
        return Err("MSI Afterburner not found in Program Files (optional).".into());
    };

    let raw = fs::read_to_string(path).map_err(|e| format!("Read error: {}", e))?;
    let eula = "I confirm that I am aware of unofficial overclocking limitations and fully understand that MSI will not provide me any support on it";
    let mut lines: Vec<String> = Vec::new();
    let mut saw_eula = false;
    let mut saw_mode = false;
    let mut in_hal = false;

    for line in raw.lines() {
        let trim = line.trim();
        if trim.starts_with('[') && trim.ends_with(']') {
            if in_hal {
                if !saw_eula {
                    lines.push(format!("UnofficialOverclockingEULA = {}", eula));
                    saw_eula = true;
                }
                if !saw_mode {
                    lines.push("UnofficialOverclockingMode = 2".into());
                    saw_mode = true;
                }
            }
            in_hal = trim.eq_ignore_ascii_case("[ATIADLHAL]");
            lines.push(line.to_string());
            continue;
        }
        if in_hal && trim.to_ascii_lowercase().starts_with("unofficialoverclockingeula") {
            lines.push(format!("UnofficialOverclockingEULA = {}", eula));
            saw_eula = true;
        } else if in_hal && trim.to_ascii_lowercase().starts_with("unofficialoverclockingmode") {
            lines.push("UnofficialOverclockingMode = 2".into());
            saw_mode = true;
        } else {
            lines.push(line.to_string());
        }
    }

    if !saw_eula || !saw_mode {
        lines.push("[ATIADLHAL]".into());
        if !saw_eula {
            lines.push(format!("UnofficialOverclockingEULA = {}", eula));
        }
        if !saw_mode {
            lines.push("UnofficialOverclockingMode = 2".into());
        }
    }

    fs::write(path, lines.join("\r\n") + "\r\n")
        .map_err(|e| format!("Failed writing MSIAfterburner.cfg (run as Admin): {}", e))?;
    Ok("Patched MSIAfterburner.cfg -> UnofficialOverclockingMode = 2 (restart Afterburner).".into())
}

pub fn query_adl_diag(wake_gpu: bool) -> Result<AdlDiagInfo, String> {
    let session = AdlSession::open(wake_gpu)?;
    let (idx, name) = session.find_amd_adapter()?;
    let (od_supported, od_enabled, od_version) = session.get_od_caps(idx);
    let (temp_c, load_pct) = session.read_sensors(idx);

    unsafe {
        if let (Some(caps_fn), Some(state_get)) = (
            load_fn!(session.lib, b"ADL_Overdrive6_Capabilities_Get\0", FnOd6CapsGet),
            load_fn!(session.lib, b"ADL_Overdrive6_StateInfo_Get\0", FnOd6StateInfoGet),
        ) {
            let mut caps: AdlOd6Capabilities = zeroed();
            if caps_fn(idx, &mut caps) == ADL_OK && caps.s_engine_clock_range.i_max > 0 {
                let mut cur = [0i32; 19];
                cur[0] = 2;
                let got_custom = state_get(
                    idx,
                    ADL_OD6_GETSTATEINFO_CUSTOM_PERFORMANCE,
                    cur.as_mut_ptr() as *mut c_void,
                ) == ADL_OK;
                let got_def = if !got_custom {
                    cur[0] = 2;
                    state_get(
                        idx,
                        ADL_OD6_GETSTATEINFO_DEFAULT_PERFORMANCE,
                        cur.as_mut_ptr() as *mut c_void,
                    ) == ADL_OK
                } else {
                    true
                };

                if got_custom || got_def {
                    let n = (cur[0] as usize).clamp(1, 8);
                    let mut parts = Vec::new();
                    for i in 0..n {
                        let off = 3 + i * 2;
                        parts.push(format!("L{}: {}/{} MHz", i, cur[off] / 100, cur[off + 1] / 100));
                    }
                    let last = 3 + (n - 1) * 2;
                    return Ok(AdlDiagInfo {
                        adapter_name: name,
                        od_supported,
                        od_enabled,
                        od_version,
                        backend: "Overdrive6".into(),
                        cur_core_mhz: (cur[last].max(0) / 100) as u32,
                        cur_mem_mhz: (cur[last + 1].max(0) / 100) as u32,
                        min_core_mhz: (caps.s_engine_clock_range.i_min.max(0) / 100) as u32,
                        max_core_mhz: (caps.s_engine_clock_range.i_max.max(0) / 100) as u32,
                        min_mem_mhz: (caps.s_memory_clock_range.i_min.max(0) / 100) as u32,
                        max_mem_mhz: (caps.s_memory_clock_range.i_max.max(0) / 100) as u32,
                        temp_c,
                        load_pct,
                        states_summary: parts.join("  •  "),
                    });
                }
            }
        }

        if let (Some(params_fn), Some(levels_get)) = (
            load_fn!(session.lib, b"ADL_Overdrive5_ODParameters_Get\0", FnOd5ParamsGet),
            load_fn!(session.lib, b"ADL_Overdrive5_ODPerformanceLevels_Get\0", FnOd5LevelsGet),
        ) {
            let mut params: AdlOdParameters = zeroed();
            params.i_size = size_of::<AdlOdParameters>() as c_int;
            if params_fn(idx, &mut params) == ADL_OK && params.i_number_of_performance_levels > 0 {
                let n = (params.i_number_of_performance_levels as usize).min(16);
                let mut cur = vec![0i32; 2 + n * 3];
                cur[0] = (8 + n * 12) as c_int;
                if levels_get(idx, 0, cur.as_mut_ptr() as *mut c_void) == ADL_OK {
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
                    return Ok(AdlDiagInfo {
                        adapter_name: name,
                        od_supported,
                        od_enabled,
                        od_version,
                        backend: "Overdrive5".into(),
                        cur_core_mhz: (cur[last].max(0) / 100) as u32,
                        cur_mem_mhz: (cur[last + 1].max(0) / 100) as u32,
                        min_core_mhz: (params.s_engine_clock.i_min.max(0) / 100) as u32,
                        max_core_mhz: (params.s_engine_clock.i_max.max(0) / 100) as u32,
                        min_mem_mhz: (params.s_memory_clock.i_min.max(0) / 100) as u32,
                        max_mem_mhz: (params.s_memory_clock.i_max.max(0) / 100) as u32,
                        temp_c,
                        load_pct,
                        states_summary: parts.join("  •  "),
                    });
                }
            }
        }
    }

    Err(format!(
        "No active Overdrive5/6 state returned (OD_Caps: sup={}, en={}, ver={}).",
        od_supported, od_enabled, od_version
    ))
}

fn try_od6_apply(
    session: &AdlSession,
    idx: c_int,
    target_core: u32,
    target_mem: u32,
    mode: OcMode,
) -> Result<(u32, u32, String), String> {
    unsafe {
        if mode != OcMode::Standard {
            if let Some(pwr) = load_fn!(session.lib, b"ADL_Overdrive6_PowerControl_Set\0", FnOd6PowerControlSet) {
                let _ = pwr(idx, 20);
            }
        }

        let Some(caps_fn) = load_fn!(session.lib, b"ADL_Overdrive6_Capabilities_Get\0", FnOd6CapsGet) else {
            return Err("OD6_Caps missing".into());
        };
        let mut caps: AdlOd6Capabilities = zeroed();
        let crc = caps_fn(idx, &mut caps);
        if crc != ADL_OK && mode == OcMode::Standard {
            return Err(format!("OD6_Caps rc={}", crc));
        }

        let state_get = load_fn!(session.lib, b"ADL_Overdrive6_StateInfo_Get\0", FnOd6StateInfoGet);
        let state_set = load_fn!(session.lib, b"ADL_Overdrive6_State_Set\0", FnOd6StateSet);
        let adl2_get = load_fn!(session.lib, b"ADL2_Overdrive6_StateInfo_Get\0", FnAdl2Od6StateInfoGet);
        let adl2_set = load_fn!(session.lib, b"ADL2_Overdrive6_State_Set\0", FnAdl2Od6StateSet);

        let mut base = [0i32; 19];
        let mut loaded = false;
        for st_type in [
            ADL_OD6_GETSTATEINFO_CUSTOM_PERFORMANCE,
            ADL_OD6_GETSTATEINFO_DEFAULT_PERFORMANCE,
        ] {
            base = [0i32; 19];
            base[0] = 2;
            if let Some(f) = state_get {
                if f(idx, st_type, base.as_mut_ptr() as *mut c_void) == ADL_OK {
                    loaded = true;
                    break;
                }
            }
            if !session.adl2_ctx.is_null() {
                if let Some(f2) = adl2_get {
                    base = [0i32; 19];
                    base[0] = 2;
                    if f2(session.adl2_ctx, idx, st_type, base.as_mut_ptr() as *mut c_void) == ADL_OK {
                        loaded = true;
                        break;
                    }
                }
            }
        }

        if !loaded {
            base = [0i32; 19];
            base[0] = 2;
            base[3] = 30_000;
            base[4] = 15_000;
            base[5] = (target_core as i32) * 100;
            base[6] = (target_mem as i32) * 100;
        }

        let n = (base[0] as usize).clamp(2, 8);
        base[0] = n as i32;
        let last = 3 + (n - 1) * 2;
        let orig_e = base[last];
        let orig_m = base[last + 1];

        // Unofficial modes skip ADL upper-bound clamping
        let want_e = if mode == OcMode::Standard && caps.s_engine_clock_range.i_max > 0 {
            ((target_core as i32) * 100).clamp(caps.s_engine_clock_range.i_min, caps.s_engine_clock_range.i_max)
        } else {
            (target_core as i32) * 100
        };
        let want_m = if mode == OcMode::Standard && caps.s_memory_clock_range.i_max > 0 {
            ((target_mem as i32) * 100).clamp(caps.s_memory_clock_range.i_min, caps.s_memory_clock_range.i_max)
        } else {
            (target_mem as i32) * 100
        };

        let attempts: [(&str, i32, i32); 3] = [
            ("Overdrive6", want_e, want_m),
            ("Overdrive6 (Core-only)", want_e, orig_m),
            ("Overdrive6 (Mem-only)", orig_e, want_m),
        ];

        let mut last_rc = -99;
        for (label, e, m) in attempts {
            if e <= 0 || m <= 0 {
                continue;
            }
            let mut buf = base;
            if mode == OcMode::UnofficialFlatClocks {
                // Flatten all states (bypass PowerPlay transitions)
                for i in 0..n {
                    let off = 3 + i * 2;
                    buf[off] = e;
                    buf[off + 1] = m;
                }
            } else {
                buf[last] = e;
                buf[last + 1] = m;
                for i in 0..(n - 1) {
                    let off = 3 + i * 2;
                    if buf[off] > e {
                        buf[off] = e;
                    }
                    if buf[off + 1] > m {
                        buf[off + 1] = m;
                    }
                }
            }

            if let Some(f) = state_set {
                let rc = f(idx, ADL_OD6_SETSTATE_PERFORMANCE, buf.as_mut_ptr() as *mut c_void);
                last_rc = rc;
                if rc == ADL_OK {
                    return Ok((
                        (e / 100) as u32,
                        (m / 100) as u32,
                        format!("{} [{}]", label, mode.label()),
                    ));
                }
            }
            if !session.adl2_ctx.is_null() {
                if let Some(f2) = adl2_set {
                    let rc2 = f2(
                        session.adl2_ctx,
                        idx,
                        ADL_OD6_SETSTATE_PERFORMANCE,
                        buf.as_mut_ptr() as *mut c_void,
                    );
                    last_rc = rc2;
                    if rc2 == ADL_OK {
                        return Ok((
                            (e / 100) as u32,
                            (m / 100) as u32,
                            format!("ADL2 {} [{}]", label, mode.label()),
                        ));
                    }
                }
            }
        }

        Err(format!("OD6_Set rc={}", last_rc))
    }
}

fn try_od5_apply(
    session: &AdlSession,
    idx: c_int,
    target_core: u32,
    target_mem: u32,
    mode: OcMode,
) -> Result<(u32, u32, String), String> {
    unsafe {
        if mode != OcMode::Standard {
            if let Some(pwr) = load_fn!(session.lib, b"ADL_Overdrive5_PowerControl_Set\0", FnOd5PowerControlSet) {
                let _ = pwr(idx, 20);
            }
        }

        let (Some(params_fn), Some(levels_get)) = (
            load_fn!(session.lib, b"ADL_Overdrive5_ODParameters_Get\0", FnOd5ParamsGet),
            load_fn!(session.lib, b"ADL_Overdrive5_ODPerformanceLevels_Get\0", FnOd5LevelsGet),
        ) else {
            return Err("OD5 missing".into());
        };
        let levels_set = load_fn!(session.lib, b"ADL_Overdrive5_ODPerformanceLevels_Set\0", FnOd5LevelsSet);
        let adl2_set = load_fn!(session.lib, b"ADL2_Overdrive5_ODPerformanceLevels_Set\0", FnAdl2Od5LevelsSet);

        let mut params: AdlOdParameters = zeroed();
        params.i_size = size_of::<AdlOdParameters>() as c_int;
        let prc = params_fn(idx, &mut params);
        if prc != ADL_OK || params.i_number_of_performance_levels <= 0 {
            return Err(format!("OD5_Params rc={}", prc));
        }

        let n = (params.i_number_of_performance_levels as usize).min(16);
        let mut base = vec![0i32; 2 + n * 3];
        base[0] = (8 + n * 12) as c_int;
        let grc = levels_get(idx, 0, base.as_mut_ptr() as *mut c_void);
        if grc != ADL_OK {
            return Err(format!("OD5_Get rc={}", grc));
        }

        let max_vddc = if params.s_vddc.i_max > 0 {
            params.s_vddc.i_max
        } else {
            1150
        };
        for i in 0..n {
            let v_off = 2 + i * 3 + 2;
            if base[v_off] == 0 {
                base[v_off] = if i == 0 && mode != OcMode::UnofficialFlatClocks {
                    params.s_vddc.i_min.max(850)
                } else {
                    max_vddc
                };
            }
        }

        let last = 2 + (n - 1) * 3;
        let orig_e = base[last];
        let orig_m = base[last + 1];

        let want_e = if mode == OcMode::Standard && params.s_engine_clock.i_max > 0 {
            ((target_core as i32) * 100).clamp(params.s_engine_clock.i_min, params.s_engine_clock.i_max)
        } else {
            (target_core as i32) * 100
        };
        let want_m = if mode == OcMode::Standard && params.s_memory_clock.i_max > 0 {
            ((target_mem as i32) * 100).clamp(params.s_memory_clock.i_min, params.s_memory_clock.i_max)
        } else {
            (target_mem as i32) * 100
        };

        let attempts: [(&str, i32, i32); 3] = [
            ("Overdrive5", want_e, want_m),
            ("Overdrive5 (Core-only)", want_e, orig_m),
            ("Overdrive5 (Mem-only)", orig_e, want_m),
        ];

        let mut last_rc = -99;
        for (label, e, m) in attempts {
            let mut buf = base.clone();
            if mode == OcMode::UnofficialFlatClocks {
                for i in 0..n {
                    let off = 2 + i * 3;
                    buf[off] = e;
                    buf[off + 1] = m;
                    buf[off + 2] = max_vddc;
                }
            } else {
                buf[last] = e;
                buf[last + 1] = m;
                for i in 0..(n - 1) {
                    let off = 2 + i * 3;
                    if buf[off] > e {
                        buf[off] = e;
                    }
                    if buf[off + 1] > m {
                        buf[off + 1] = m;
                    }
                }
            }

            if let Some(f) = levels_set {
                let rc = f(idx, buf.as_mut_ptr() as *mut c_void);
                last_rc = rc;
                if rc == ADL_OK {
                    return Ok((
                        (e / 100) as u32,
                        (m / 100) as u32,
                        format!("{} [{}]", label, mode.label()),
                    ));
                }
            }
            if !session.adl2_ctx.is_null() {
                if let Some(f2) = adl2_set {
                    let rc2 = f2(session.adl2_ctx, idx, buf.as_mut_ptr() as *mut c_void);
                    last_rc = rc2;
                    if rc2 == ADL_OK {
                        return Ok((
                            (e / 100) as u32,
                            (m / 100) as u32,
                            format!("ADL2 {} [{}]", label, mode.label()),
                        ));
                    }
                }
            }
        }

        Err(format!("OD5_Set rc={}", last_rc))
    }
}

pub fn apply_live_adl_oc(
    target_core: u32,
    target_mem: u32,
    mode: OcMode,
) -> Result<(u32, u32, String), String> {
    if mode != OcMode::Standard {
        let _ = unlock_unofficial_registry_flags();
    }

    let session = AdlSession::open(true)?;
    let (idx, _) = session.find_amd_adapter()?;
    let (sup, en, ver) = session.get_od_caps(idx);

    let od6_res = try_od6_apply(&session, idx, target_core, target_mem, mode);
    if let Ok(ok) = od6_res {
        return Ok(ok);
    }
    let od5_res = try_od5_apply(&session, idx, target_core, target_mem, mode);
    if let Ok(ok) = od5_res {
        return Ok(ok);
    }

    Err(format!(
        "Driver rejected {} ({} | {} | OD_Caps sup={}, en={}, ver={}). Click 'Unlock Registry & Reload GPU', switch to Amernime 23.9.1 iCafe, or sync MSI Afterburner Mode 2.",
        mode.label(),
        od6_res.unwrap_err(),
        od5_res.unwrap_err(),
        sup,
        en,
        ver
    ))
}

pub fn save_live_oc_profile(enabled: bool, mode: OcMode, core_mhz: u32, mem_mhz: u32) {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    if let Ok((rb, _)) = hklm.create_subkey(RBRIDGE_REG) {
        let _ = rb.set_value("BootOcEnabled", &(if enabled { 1u32 } else { 0u32 }));
        let _ = rb.set_value("BootOcMode", &(mode as u32));
        let _ = rb.set_value("LastOcCore", &core_mhz);
        let _ = rb.set_value("LastOcMem", &mem_mhz);
    }
}

pub fn apply_saved_live_oc() {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    if let Ok(rb) = hklm.open_subkey_with_flags(RBRIDGE_REG, KEY_READ) {
        let enabled = rb.get_value::<u32, _>("BootOcEnabled").unwrap_or(0) != 0;
        let mode = OcMode::from_u32(rb.get_value::<u32, _>("BootOcMode").unwrap_or(1));
        let core = rb.get_value::<u32, _>("LastOcCore").unwrap_or(0);
        let mem = rb.get_value::<u32, _>("LastOcMem").unwrap_or(0);
        if enabled && core >= 300 && mem >= 300 {
            let _ = apply_live_adl_oc(core, mem, mode);
        }
    }
}

pub fn query_gpu_telemetry() -> GpuTelemetry {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let mut tel = GpuTelemetry {
        gpu_name: "AMD Radeon HD 8670M".into(),
        driver_version: "Unknown".into(),
        driver_stack: "Standard AMD Driver".into(),
        live_core_mhz: 975,
        live_mem_mhz: 900,
        temp_c: None,
        load_pct: None,
        soft_pp_active: false,
        boot_oc_enabled: false,
        saved_oc_mode: OcMode::UnofficialPowerPlay,
        saved_oc_core: 0,
        saved_oc_mem: 0,
    };

    if let Some(amd_path) = find_amd_class_subkey() {
        if let Ok(k) = hklm.open_subkey_with_flags(&amd_path, KEY_READ) {
            if let Ok(name) = k.get_value::<String, _>("DriverDesc") {
                tel.gpu_name = name;
            }
            if let Ok(ver) = k.get_value::<String, _>("DriverVersion") {
                tel.driver_version = ver.clone();
                if ver.starts_with("27.20.21026") {
                    tel.driver_stack = "AMD Radeon PRO 21.Q2.1 (RBridge)".into();
                } else if ver.starts_with("31.0.") || ver.starts_with("30.0.") {
                    tel.driver_stack = "Amernime / R.ID Hybrid Stack".into();
                } else {
                    tel.driver_stack = format!("AMD Driver {}", ver);
                }
            }
            tel.soft_pp_active = k.get_raw_value("PP_PhmSoftPowerPlayTable").is_ok();
        }
    }

    if let Ok(rb) = hklm.open_subkey_with_flags(RBRIDGE_REG, KEY_READ) {
        tel.boot_oc_enabled = rb.get_value::<u32, _>("BootOcEnabled").unwrap_or(0) != 0;
        tel.saved_oc_mode = OcMode::from_u32(rb.get_value::<u32, _>("BootOcMode").unwrap_or(1));
        tel.saved_oc_core = rb.get_value::<u32, _>("LastOcCore").unwrap_or(0);
        tel.saved_oc_mem = rb.get_value::<u32, _>("LastOcMem").unwrap_or(0);
    }

    if let Ok(diag) = query_adl_diag(false) {
        if diag.cur_core_mhz > 0 {
            tel.live_core_mhz = diag.cur_core_mhz;
        }
        if diag.cur_mem_mhz > 0 {
            tel.live_mem_mhz = diag.cur_mem_mhz;
        }
        tel.temp_c = diag.temp_c;
        tel.load_pct = diag.load_pct;
    }

    tel
}

fn u16_le(buf: &[u8], off: usize) -> Result<u16, String> {
    if off + 2 > buf.len() {
        return Err(format!("ROM offset 0x{:X} out of bounds", off));
    }
    Ok(u16::from_le_bytes([buf[off], buf[off + 1]]))
}

fn read_le24(buf: &[u8], off: usize) -> u32 {
    (buf[off] as u32) | ((buf[off + 1] as u32) << 8) | ((buf[off + 2] as u32) << 16)
}

fn write_le24(buf: &mut [u8], off: usize, val: u32) {
    let b = val.to_le_bytes();
    buf[off] = b[0];
    buf[off + 1] = b[1];
    buf[off + 2] = b[2];
}

pub fn parse_powerplay_bytes(table: &[u8]) -> Result<ParsedPowerPlay, String> {
    if table.len() < 32 {
        return Err("PowerPlay table too small.".into());
    }
    let format_rev = table[2];
    let content_rev = table[3];
    let mut states = Vec::new();
    let mut i = 16usize;
    while i + 8 <= table.len() {
        let sclk_10k = read_le24(table, i);
        let mclk_10k = read_le24(table, i + 3);
        let vddc = u16::from_le_bytes([table[i + 6], table[i + 7]]);
        if sclk_10k >= 15_000
            && sclk_10k <= 125_000
            && sclk_10k % 100 == 0
            && (sclk_10k / 100) % 5 == 0
            && mclk_10k >= 15_000
            && mclk_10k <= 150_000
            && mclk_10k % 100 == 0
            && (mclk_10k / 100) % 5 == 0
            && ((600..=1350).contains(&vddc) || vddc >= 0xFF00)
        {
            states.push(DpmState {
                offset: i,
                core_mhz: sclk_10k / 100,
                mem_mhz: mclk_10k / 100,
                vddc_raw: vddc,
            });
            i += 8;
        } else {
            i += 1;
        }
    }

    let (boost_core, boost_mem) = states
        .iter()
        .max_by_key(|s| (s.core_mhz, s.mem_mhz))
        .map(|s| (s.core_mhz, s.mem_mhz))
        .unwrap_or((975, 900));

    Ok(ParsedPowerPlay {
        table_bytes: table.to_vec(),
        format_rev,
        content_rev,
        states,
        boost_core_mhz: boost_core,
        boost_mem_mhz: boost_mem,
    })
}

fn find_atom_rom_slice(buf: &[u8]) -> Option<&[u8]> {
    if buf.len() < 0x100 {
        return None;
    }
    for pos in 0..=buf.len() - 0x100 {
        if buf[pos] == 0x55 && buf[pos + 1] == 0xAA {
            let hdr_off = u16::from_le_bytes([buf[pos + 0x48], buf[pos + 0x49]]) as usize;
            if hdr_off >= 0x4A
                && pos + hdr_off + 36 <= buf.len()
                && &buf[pos + hdr_off + 4..pos + hdr_off + 8] == b"ATOM"
            {
                return Some(&buf[pos..]);
            }
        }
    }
    None
}

pub fn extract_powerplay_from_rom(path: &Path) -> Result<ParsedPowerPlay, String> {
    let raw = fs::read(path).map_err(|e| format!("Failed reading ROM: {}", e))?;
    let rom = find_atom_rom_slice(&raw)
        .ok_or_else(|| "No AMD ATOMBIOS signature (55 AA + ATOM) found.".to_string())?;
    let rom_hdr_off = u16_le(rom, 0x48)? as usize;
    let master_data_off = u16_le(rom, rom_hdr_off + 32)? as usize;
    let pp_off = u16_le(rom, master_data_off + 34)? as usize;
    if pp_off == 0 || pp_off + 4 > rom.len() {
        return Err("PowerPlayInfo pointer is empty.".into());
    }
    let pp_size = u16_le(rom, pp_off)? as usize;
    if pp_size < 32 || pp_off + pp_size > rom.len() {
        return Err(format!("Invalid PowerPlayInfo size ({} bytes).", pp_size));
    }
    let parsed = parse_powerplay_bytes(&rom[pp_off..pp_off + pp_size])?;
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    if let Ok((rb, _)) = hklm.create_subkey(RBRIDGE_REG) {
        let _ = rb.set_raw_value(
            "StockPowerPlayTable",
            &RegValue {
                bytes: parsed.table_bytes.clone(),
                vtype: REG_BINARY,
            },
        );
    }
    Ok(parsed)
}

pub fn load_cached_powerplay() -> Option<ParsedPowerPlay> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    if let Ok(rb) = hklm.open_subkey_with_flags(RBRIDGE_REG, KEY_READ) {
        if let Ok(val) = rb.get_raw_value("StockPowerPlayTable") {
            if let Ok(parsed) = parse_powerplay_bytes(&val.bytes) {
                return Some(parsed);
            }
        }
    }
    None
}

pub fn apply_powerplay_oc(
    stock_table: &[u8],
    stock_core: u32,
    stock_mem: u32,
    target_core: u32,
    target_mem: u32,
) -> Result<String, String> {
    let amd_path = find_amd_class_subkey()
        .ok_or_else(|| "AMD VEN_1002 Display Class registry key not found.".to_string())?;
    let mut patched = stock_table.to_vec();
    let old_sclk = stock_core * 100;
    let old_mclk = stock_mem * 100;
    let new_sclk = target_core * 100;
    let new_mclk = target_mem * 100;

    let mut dpm_hits = 0usize;
    let mut i = 16usize;
    while i + 6 <= patched.len() {
        if read_le24(&patched, i) == old_sclk && read_le24(&patched, i + 3) == old_mclk {
            write_le24(&mut patched, i, new_sclk);
            write_le24(&mut patched, i + 3, new_mclk);
            dpm_hits += 1;
            i += 6;
        } else {
            i += 1;
        }
    }
    if dpm_hits == 0 {
        return Err(format!("Stock {}/{} MHz tuple not found in table.", stock_core, stock_mem));
    }

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let gpu_key = hklm
        .open_subkey_with_flags(&amd_path, KEY_READ | KEY_WRITE)
        .map_err(|e| format!("Cannot write AMD key: {}", e))?;
    gpu_key
        .set_raw_value(
            "PP_PhmSoftPowerPlayTable",
            &RegValue {
                bytes: patched,
                vtype: REG_BINARY,
            },
        )
        .map_err(|e| format!("Write failed: {}", e))?;

    Ok(format!(
        "Patched {} DPM state(s) to {}/{} MHz in PP_PhmSoftPowerPlayTable. Click Reload AMD GPU.",
        dpm_hits, target_core, target_mem
    ))
}

pub fn restore_factory_powerplay() -> Result<String, String> {
    let amd_path = find_amd_class_subkey()
        .ok_or_else(|| "AMD VEN_1002 key not found.".to_string())?;
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    if let Ok(gpu_key) = hklm.open_subkey_with_flags(&amd_path, KEY_READ | KEY_WRITE) {
        let _ = gpu_key.delete_value("PP_PhmSoftPowerPlayTable");
        let _ = gpu_key.delete_value("PP_PhmSoftPowerPlayTable_BAK");
    }
    save_live_oc_profile(false, OcMode::Standard, 0, 0);
    Ok("Cleared SoftPowerPlayTable override & reset Boot Auto-OC.".into())
}

pub fn reload_amd_gpu() -> Result<String, String> {
    let ps = r#"$dev = Get-PnpDevice -Class Display | Where-Object { $_.InstanceId -like '*VEN_1002*' } | Select-Object -First 1; if (-not $dev) { throw 'No VEN_1002 display device found' }; Disable-PnpDevice -InstanceId $dev.InstanceId -Confirm:$false -ErrorAction Stop; Start-Sleep -Milliseconds 800; Enable-PnpDevice -InstanceId $dev.InstanceId -Confirm:$false -ErrorAction Stop; Write-Output $dev.FriendlyName"#;
    let out = Command::new("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", ps])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("PowerShell error: {}", e))?;
    if out.status.success() {
        Ok("Reloaded AMD Radeon adapter cleanly.".into())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}
