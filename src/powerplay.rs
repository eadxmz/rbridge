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
const ACPI_PROVIDER: u32 = 0x41435049; // 'ACPI'
const VFCT_TABLE_ID: u32 = 0x54434656; // 'VFCT' (LE)

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryA(lp_lib_file_name: *const c_char) -> *mut c_void;
    fn GetProcAddress(h_module: *mut c_void, lp_proc_name: *const c_char) -> *mut c_void;
    fn FreeLibrary(h_lib_module: *mut c_void) -> c_int;
    fn EnumSystemFirmwareTables(
        firmware_table_provider_signature: u32,
        p_firmware_table_enum_buffer: *mut u8,
        buffer_size: u32,
    ) -> u32;
    fn GetSystemFirmwareTable(
        firmware_table_provider_signature: u32,
        firmware_table_id: u32,
        p_firmware_table_buffer: *mut u8,
        buffer_size: u32,
    ) -> u32;
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

#[derive(Clone, Debug)]
pub struct GpuTelemetry {
    pub gpu_name: String,
    pub driver_version: String,
    pub driver_stack: String,
    pub live_core_mhz: u32,
    pub live_mem_mhz: u32,
    pub soft_pp_active: bool,
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

fn query_adl_clocks() -> Option<(u32, u32)> {
    unsafe {
        let mut lib = LoadLibraryA(b"atiadlxx.dll\0".as_ptr() as *const c_char);
        if lib.is_null() {
            lib = LoadLibraryA(b"atiadlxy.dll\0".as_ptr() as *const c_char);
        }
        if lib.is_null() {
            return None;
        }
        let create = load_fn!(lib, b"ADL_Main_Control_Create\0", FnMainControlCreate)?;
        let destroy = load_fn!(lib, b"ADL_Main_Control_Destroy\0", FnMainControlDestroy)?;
        if create(adl_malloc, 0) != ADL_OK {
            FreeLibrary(lib);
            return None;
        }

        let mut result = None;
        if let (Some(num_fn), Some(info_fn), Some(params_fn), Some(levels_get)) = (
            load_fn!(lib, b"ADL_Adapter_NumberOfAdapters_Get\0", FnAdapterNumGet),
            load_fn!(lib, b"ADL_Adapter_AdapterInfo_Get\0", FnAdapterInfoGet),
            load_fn!(lib, b"ADL_Overdrive5_ODParameters_Get\0", FnOd5ParamsGet),
            load_fn!(lib, b"ADL_Overdrive5_ODPerformanceLevels_Get\0", FnOd5LevelsGet),
        ) {
            let mut count: c_int = 0;
            if num_fn(&mut count) == ADL_OK && count > 0 {
                let mut infos: Vec<AdapterInfo> = vec![zeroed(); count as usize];
                for item in &mut infos {
                    item.i_size = size_of::<AdapterInfo>() as c_int;
                }
                if info_fn(
                    infos.as_mut_ptr(),
                    (infos.len() * size_of::<AdapterInfo>()) as c_int,
                ) == ADL_OK
                {
                    if let Some(adp) = infos.iter().find(|i| {
                        let pnp = cstr_from_buf(&i.str_pnp_string).to_ascii_lowercase();
                        i.i_vendor_id == 0x1002 || i.i_vendor_id == 1002 || pnp.contains("ven_1002")
                    }) {
                        let mut params: AdlOdParameters = zeroed();
                        params.i_size = size_of::<AdlOdParameters>() as c_int;
                        if params_fn(adp.i_adapter_index, &mut params) == ADL_OK
                            && params.i_number_of_performance_levels > 0
                        {
                            let n = (params.i_number_of_performance_levels as usize).min(16);
                            let mut cur = vec![0i32; 2 + n * 3];
                            cur[0] = (8 + n * 12) as c_int;
                            if levels_get(
                                adp.i_adapter_index,
                                0,
                                cur.as_mut_ptr() as *mut c_void,
                            ) == ADL_OK
                            {
                                let last = 2 + (n - 1) * 3;
                                result = Some((
                                    (cur[last].max(0) / 100) as u32,
                                    (cur[last + 1].max(0) / 100) as u32,
                                ));
                            }
                        }
                    }
                }
            }
        }

        let _ = destroy();
        FreeLibrary(lib);
        result
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
        soft_pp_active: false,
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
        tel.saved_oc_core = rb.get_value::<u32, _>("LastOcCore").unwrap_or(0);
        tel.saved_oc_mem = rb.get_value::<u32, _>("LastOcMem").unwrap_or(0);
    }

    if let Some((e, m)) = query_adl_clocks() {
        if e > 0 {
            tel.live_core_mhz = e;
        }
        if m > 0 {
            tel.live_mem_mhz = m;
        }
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

        let sclk_valid = sclk_10k >= 15_000
            && sclk_10k <= 125_000
            && sclk_10k % 100 == 0
            && (sclk_10k / 100) % 5 == 0;
        let mclk_valid = mclk_10k >= 15_000
            && mclk_10k <= 150_000
            && mclk_10k % 100 == 0
            && (mclk_10k / 100) % 5 == 0;
        let vddc_valid = (600..=1350).contains(&vddc) || vddc >= 0xFF00;

        if sclk_valid && mclk_valid && vddc_valid {
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

pub fn extract_powerplay_from_bytes(raw: &[u8]) -> Result<ParsedPowerPlay, String> {
    let rom = find_atom_rom_slice(raw)
        .ok_or_else(|| "No AMD ATOMBIOS signature (55 AA + ATOM) found in data.".to_string())?;

    let rom_hdr_off = u16_le(rom, 0x48)? as usize;
    let master_data_off = u16_le(rom, rom_hdr_off + 32)? as usize;
    let pp_off = u16_le(rom, master_data_off + 34)? as usize;
    if pp_off == 0 || pp_off + 4 > rom.len() {
        return Err("PowerPlayInfo pointer is empty in MasterDataTable.".into());
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

pub fn extract_powerplay_from_rom(path: &Path) -> Result<ParsedPowerPlay, String> {
    let raw = fs::read(path).map_err(|e| format!("Failed to read ROM file: {}", e))?;
    extract_powerplay_from_bytes(&raw)
}

fn read_acpi_table(table_id: u32) -> Option<Vec<u8>> {
    unsafe {
        let size = GetSystemFirmwareTable(ACPI_PROVIDER, table_id, null_mut(), 0);
        if size == 0 || size > 16 * 1024 * 1024 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let written = GetSystemFirmwareTable(ACPI_PROVIDER, table_id, buf.as_mut_ptr(), size);
        if written == 0 {
            return None;
        }
        buf.truncate(written as usize);
        Some(buf)
    }
}

pub fn extract_powerplay_from_acpi() -> Result<(ParsedPowerPlay, String), String> {
    if let Some(vfct) = read_acpi_table(VFCT_TABLE_ID) {
        if let Ok(parsed) = extract_powerplay_from_bytes(&vfct) {
            return Ok((parsed, "ACPI VFCT".into()));
        }
    }

    unsafe {
        let enum_size = EnumSystemFirmwareTables(ACPI_PROVIDER, null_mut(), 0);
        if enum_size >= 4 && enum_size <= 65536 {
            let mut raw_ids = vec![0u8; enum_size as usize];
            let written = EnumSystemFirmwareTables(ACPI_PROVIDER, raw_ids.as_mut_ptr(), enum_size);
            for chunk in raw_ids[..written as usize].chunks_exact(4) {
                let id = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                if id == VFCT_TABLE_ID {
                    continue;
                }
                if let Some(tbl) = read_acpi_table(id) {
                    if let Ok(parsed) = extract_powerplay_from_bytes(&tbl) {
                        let tag = String::from_utf8_lossy(chunk).into_owned();
                        return Ok((parsed, format!("ACPI {}", tag)));
                    }
                }
            }
        }
    }

    Err("ACPI VFCT table not exposed by Dell BIOS (legacy ATRM mode). Load a matching 015.037.000.002 (1002 6660 - 1028 05EA) .rom file instead.".into())
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
    if let Some(amd_path) = find_amd_class_subkey() {
        if let Ok(k) = hklm.open_subkey_with_flags(&amd_path, KEY_READ) {
            for name in ["PP_PhmSoftPowerPlayTable_BAK", "PP_PhmSoftPowerPlayTable"] {
                if let Ok(val) = k.get_raw_value(name) {
                    if let Ok(parsed) = parse_powerplay_bytes(&val.bytes) {
                        return Some(parsed);
                    }
                }
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
        return Err(format!(
            "Could not find stock {}/{} MHz DPM clock tuple in PowerPlay table.",
            stock_core, stock_mem
        ));
    }

    let mut od_bumped = 0usize;
    let mut j = 16usize;
    while j + 8 <= patched.len() {
        let max_e = u32::from_le_bytes([patched[j], patched[j + 1], patched[j + 2], patched[j + 3]]);
        let max_m =
            u32::from_le_bytes([patched[j + 4], patched[j + 5], patched[j + 6], patched[j + 7]]);
        if (95_000..=140_000).contains(&max_e)
            && (90_000..=150_000).contains(&max_m)
            && max_e % 100 == 0
            && max_m % 100 == 0
        {
            let raised_e = max_e.max(new_sclk + 10_000);
            let raised_m = max_m.max(new_mclk + 10_000);
            patched[j..j + 4].copy_from_slice(&raised_e.to_le_bytes());
            patched[j + 4..j + 8].copy_from_slice(&raised_m.to_le_bytes());
            od_bumped += 1;
            j += 8;
        } else {
            j += 1;
        }
    }

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let gpu_key = hklm
        .open_subkey_with_flags(&amd_path, KEY_READ | KEY_WRITE)
        .map_err(|e| format!("Cannot write AMD registry key (run as Admin): {}", e))?;

    if gpu_key.get_raw_value("PP_PhmSoftPowerPlayTable_BAK").is_err() {
        let _ = gpu_key.set_raw_value(
            "PP_PhmSoftPowerPlayTable_BAK",
            &RegValue {
                bytes: stock_table.to_vec(),
                vtype: REG_BINARY,
            },
        );
    }

    gpu_key
        .set_raw_value(
            "PP_PhmSoftPowerPlayTable",
            &RegValue {
                bytes: patched,
                vtype: REG_BINARY,
            },
        )
        .map_err(|e| format!("Failed writing PP_PhmSoftPowerPlayTable: {}", e))?;

    if let Ok((rb, _)) = hklm.create_subkey(RBRIDGE_REG) {
        let _ = rb.set_value("LastOcCore", &target_core);
        let _ = rb.set_value("LastOcMem", &target_mem);
    }

    Ok(format!(
        "Injected PP_PhmSoftPowerPlayTable: {} DPM state(s) patched to {}/{} MHz ({} OD cap(s) raised). Click 'Reload AMD GPU Now' to activate.",
        dpm_hits, target_core, target_mem, od_bumped
    ))
}

pub fn restore_factory_powerplay() -> Result<String, String> {
    let amd_path = find_amd_class_subkey()
        .ok_or_else(|| "AMD VEN_1002 Display Class registry key not found.".to_string())?;
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let gpu_key = hklm
        .open_subkey_with_flags(&amd_path, KEY_READ | KEY_WRITE)
        .map_err(|e| format!("Cannot open AMD registry key (run as Admin): {}", e))?;

    let _ = gpu_key.delete_value("PP_PhmSoftPowerPlayTable");
    let _ = gpu_key.delete_value("PP_PhmSoftPowerPlayTable_BAK");

    if let Ok((rb, _)) = hklm.create_subkey(RBRIDGE_REG) {
        let _ = rb.set_value("LastOcCore", &0u32);
        let _ = rb.set_value("LastOcMem", &0u32);
    }

    Ok("Removed PP_PhmSoftPowerPlayTable override. Click 'Reload AMD GPU Now' or reboot to return to stock VBIOS clocks.".into())
}

pub fn reload_amd_gpu() -> Result<String, String> {
    let ps = r#"$dev = Get-PnpDevice -Class Display | Where-Object { $_.InstanceId -like '*VEN_1002*' } | Select-Object -First 1; if (-not $dev) { throw 'No VEN_1002 display device found' }; Disable-PnpDevice -InstanceId $dev.InstanceId -Confirm:$false -ErrorAction Stop; Start-Sleep -Milliseconds 800; Enable-PnpDevice -InstanceId $dev.InstanceId -Confirm:$false -ErrorAction Stop; Write-Output $dev.FriendlyName"#;
    let out = Command::new("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", ps])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("Failed invoking PowerShell: {}", e))?;

    if out.status.success() {
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok(format!(
            "Reloaded {} — new PowerPlay clocks are now active!",
            if name.is_empty() { "AMD GPU" } else { &name }
        ))
    } else {
        Err(format!(
            "GPU reload failed (run as Admin or reboot manually): {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}
