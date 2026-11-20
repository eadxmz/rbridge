#![windows_subsystem = "windows"]

use std::borrow::Cow;
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime};
use walkdir::WalkDir;
use winreg::enums::*;
use winreg::{RegKey, RegValue};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DISPLAY_CLASS: &str =
    r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
const GRAPHICS_DRIVERS: &str = r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers";
const DWM_KEY: &str = r"SOFTWARE\Microsoft\Windows\Dwm";
const GAMEDVR_POLICY: &str = r"SOFTWARE\Policies\Microsoft\Windows\GameDVR";
const SYS_ENV: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
const MMCSS_GAMES: &str =
    r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Multimedia\SystemProfile\Tasks\Games";
const MMCSS_PROFILE: &str =
    r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Multimedia\SystemProfile";

fn set_bin(key: &RegKey, name: &str, bytes: &[u8]) {
    let val = RegValue {
        bytes: Cow::Borrowed(bytes),
        vtype: REG_BINARY,
    };
    let _ = key.set_raw_value(name, &val);
}

fn disable_pcie_aspm_silent() {
    let _ = Command::new("powercfg")
        .args(["/setacvalueindex", "SCHEME_CURRENT", "SUB_PCIEXPRESS", "ASPM", "0"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let _ = Command::new("powercfg")
        .args(["/setactive", "SCHEME_CURRENT"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
}

fn find_latest_vulkan_json(filename: &str, fallback_dir: &str) -> Option<String> {
    let repo = Path::new(r"C:\Windows\System32\DriverStore\FileRepository");
    let mut best: Option<(SystemTime, PathBuf)> = None;

    if let Ok(entries) = fs::read_dir(repo) {
        for entry in entries.flatten() {
            let dir_name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if !dir_name.starts_with("u0366524.inf") && !dir_name.starts_with("u0369543.inf") {
                continue;
            }
            for sub in WalkDir::new(entry.path()).max_depth(3).into_iter().flatten() {
                if sub.file_type().is_file()
                    && sub
                        .file_name()
                        .to_string_lossy()
                        .eq_ignore_ascii_case(filename)
                {
                    let candidate = sub.into_path();
                    let mtime = fs::metadata(&candidate)
                        .and_then(|m| m.modified())
                        .unwrap_or(SystemTime::UNIX_EPOCH);
                    if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
                        best = Some((mtime, candidate));
                    }
                }
            }
        }
    }

    if let Some((_, path)) = best {
        return Some(path.to_string_lossy().into_owned());
    }

    let fallback = Path::new(fallback_dir).join(filename);
    if fallback.is_file() {
        Some(fallback.to_string_lossy().into_owned())
    } else {
        None
    }
}

fn sync_vulkan_icd_key(hklm: &RegKey, reg_path: &str, json_filename: &str, fallback_dir: &str) {
    let Ok((drivers_key, _)) = hklm.create_subkey(reg_path) else {
        return;
    };

    // Prune dead AMD Vulkan ICD entries left behind by removed drivers
    let stale: Vec<String> = drivers_key
        .enum_values()
        .filter_map(|v| v.ok())
        .map(|(name, _)| name)
        .filter(|name| {
            let lower = name.to_ascii_lowercase();
            (lower.contains("amd-vulkan") || lower.contains("amd_icd"))
                && !Path::new(name).is_file()
        })
        .collect();

    for bad in stale {
        let _ = drivers_key.delete_value(&bad);
    }

    // Register active 21.Q2.1 AMD Vulkan ICD (DWORD 0 = enabled in Khronos Loader)
    if let Some(live_json) = find_latest_vulkan_json(json_filename, fallback_dir) {
        let _ = drivers_key.set_value(&live_json, &0u32);
    }
}

fn enforce_vulkan_stack(hklm: &RegKey) {
    // 1. Disable buggy switchable-graphics Vulkan layer on driver-only Enduro setups
    if let Ok(env_key) = hklm.open_subkey_with_flags(SYS_ENV, KEY_WRITE) {
        let _ = env_key.set_value("DISABLE_LAYER_AMD_SWITCHABLE_GRAPHICS_1", &"1");
    }

    // 2. Ensure both 64-bit and 32-bit AMD Vulkan ICDs point to live 21.Q2.1 manifests
    sync_vulkan_icd_key(
        hklm,
        r"SOFTWARE\Khronos\Vulkan\Drivers",
        "amd-vulkan64.json",
        r"C:\Windows\System32",
    );
    sync_vulkan_icd_key(
        hklm,
        r"SOFTWARE\WOW6432Node\Khronos\Vulkan\Drivers",
        "amd-vulkan32.json",
        r"C:\Windows\SysWOW64",
    );
}

fn boost_amd_msi_priority(hklm: &RegKey) {
    let Ok(pci_root) = hklm.open_subkey_with_flags(r"SYSTEM\CurrentControlSet\Enum\PCI", KEY_READ)
    else {
        return;
    };

    for dev in pci_root.enum_keys().filter_map(|k| k.ok()) {
        let lower = dev.to_ascii_lowercase();
        if !lower.starts_with("ven_1002&dev_6660") && !lower.starts_with("ven_1002&dev_6600") {
            continue;
        }
        let Ok(dev_key) = pci_root.open_subkey_with_flags(&dev, KEY_READ) else {
            continue;
        };
        for inst in dev_key.enum_keys().filter_map(|k| k.ok()) {
            let aff_path = format!(
                r"SYSTEM\CurrentControlSet\Enum\PCI\{}\{}\Device Parameters\Interrupt Management\Affinity Policy",
                dev, inst
            );
            if let Ok((aff_key, _)) = hklm.create_subkey(&aff_path) {
                let _ = aff_key.set_value("DevicePriority", &3u32); // High interrupt priority
            }
        }
    }
}

fn enforce_amd_tweaks() {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);

    // 1. Global 10s TDR protection & HAGS off (GCN 1.0 stability)
    if let Ok(gd) = hklm.open_subkey_with_flags(GRAPHICS_DRIVERS, KEY_WRITE) {
        let _ = gd.set_value("TdrDelay", &10u32);
        let _ = gd.set_value("TdrDdiDelay", &10u32);
        let _ = gd.set_value("HwSchMode", &1u32);
    }

    // 2. Disable Windows MPO (fixes hybrid Intel + AMD frame-pacing stutters)
    if let Ok((dwm, _)) = hklm.create_subkey(DWM_KEY) {
        let _ = dwm.set_value("OverlayTestMode", &5u32);
    }

    // 3. Disable Xbox Game DVR overlay hooks (saves ~150-250 MB VRAM on 2 GB card)
    if let Ok((dvr, _)) = hklm.create_subkey(GAMEDVR_POLICY) {
        let _ = dvr.set_value("AllowGameDVR", &0u32);
    }

    // 4. Boost game thread, GPU priority & kill network/DPC throttling in MMCSS
    if let Ok(prof) = hklm.open_subkey_with_flags(MMCSS_PROFILE, KEY_WRITE) {
        let _ = prof.set_value("SystemResponsiveness", &10u32);
        let _ = prof.set_value("NetworkThrottlingIndex", &0xFFFF_FFFFu32);
    }
    if let Ok((games, _)) = hklm.create_subkey(MMCSS_GAMES) {
        let _ = games.set_value("GPU Priority", &8u32);
        let _ = games.set_value("Priority", &6u32);
        let _ = games.set_value("Scheduling Category", &"High");
        let _ = games.set_value("SFIO Priority", &"High");
    }

    // 5. Vulkan stack repair + High MSI interrupt priority for AMD 8670M
    enforce_vulkan_stack(&hklm);
    boost_amd_msi_priority(&hklm);

    // 6. Dynamically locate AMD Oland dGPU key (never touch Intel iGPU or gamma)
    let Ok(class_key) = hklm.open_subkey_with_flags(DISPLAY_CLASS, KEY_READ) else {
        return;
    };

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

        // Stability, Enduro SDMA copy-back & anti-stutter unlocks
        let _ = gpu_key.set_value("EnableUlps", &0u32);
        let _ = gpu_key.set_value("EnableUlps_NA", &"0");
        let _ = gpu_key.set_value("PP_GPUPowerDownEnabled", &0u32);
        let _ = gpu_key.set_value("BacoEnable", &0u32);
        let _ = gpu_key.set_value("EnableCrossFireAutoLink", &0u32);
        let _ = gpu_key.set_value("StutterMode", &0u32);
        let _ = gpu_key.set_value("EnableAspmL0s", &0u32);
        let _ = gpu_key.set_value("EnableAspmL1", &0u32);
        let _ = gpu_key.set_value("EnablePcieGen3", &1u32);
        let _ = gpu_key.set_value("DisableDMACopy", &0u32);
        let _ = gpu_key.set_value("DisableSdma", &0u32);
        let _ = gpu_key.set_value("DisableBlockWrite", &0u32);
        let _ = gpu_key.set_value("DisableEarlyZ", &0u32);
        let _ = gpu_key.set_value("DisableFBC", &1u32);
        let _ = gpu_key.set_value("DisableDrmdmaPowerGating", &1u32);

        // Unlocked GCN 1.0 KMD queue & scheduling tweaks (Intel iGPU unaffected)
        let _ = gpu_key.set_value("KMD_EnablePreemption", &0u32);
        let _ = gpu_key.set_value("KMD_EnableComputeQueue", &1u32);
        let _ = gpu_key.set_value("KMD_EnableInternalLargePage", &0u32);
        let _ = gpu_key.set_value("KMD_DeLagEnabled", &0u32);
        let _ = gpu_key.set_value("KMD_FRTEnabled", &0u32);
        let _ = gpu_key.set_value("KMD_RadeonBoostEnabled", &0u32);
        let _ = gpu_key.set_value("KMD_ChillEnabled", &0u32);
        let _ = gpu_key.set_value("KMD_EnhancedSyncEnabled", &0u32);

        // Native OverDrive & PowerPlay unlocks (smooth thermal stepping kept ON)
        let _ = gpu_key.set_value("PP_ODNFeatureEnable", &1u32);
        let _ = gpu_key.set_value("PP_DisablePowerContainment", &1u32);
        let _ = gpu_key.set_value("PP_DisableClockStretching", &1u32);
        let _ = gpu_key.set_value("PP_DisableULV", &1u32);
        let _ = gpu_key.set_value("PP_SclkDeepSleepDisable", &1u32);
        let _ = gpu_key.set_value("PP_MclkDeepSleepDisable", &1u32);
        let _ = gpu_key.set_value("PP_ThermalAutoThrottlingEnable", &1u32);
        let _ = gpu_key.set_value("PP_PhmUseDummyBackEnd", &0u32);

        // Unlocked OpenGL multi-threading & HyperZ
        let _ = gpu_key.set_value("OGL_VBO_Alloc_Policy", &1u32);
        let _ = gpu_key.set_value("OGL_EnableTripleBuffer", &0u32);
        let _ = gpu_key.set_value("OGL_ThreadControl", &1u32);
        let _ = gpu_key.set_value("OGL_EnableFastZClear", &1u32);

        // Unlocked UMD gaming, Vulkan cache & bandwidth profile (no gamma keys)
        if let Ok((umd, _)) = gpu_key.create_subkey("UMD") {
            set_bin(&umd, "CatalystAI", &[0x31, 0x00]);
            let _ = umd.set_value("CatalystAI_DEF", &"1");
            set_bin(&umd, "FlipQueueSize", &[0x32, 0x00]);
            set_bin(&umd, "FlipQueueSize_SET", &[0x32, 0x00]);
            let _ = umd.set_value("FlipQueueSize_DEF", &"2");
            set_bin(&umd, "Main3D", &[0x32, 0x00]);
            set_bin(&umd, "Main3D_SET", &[0x32, 0x00]);
            let _ = umd.set_value("Main3D_DEF", &"2");
            set_bin(&umd, "ShaderCache", &[0x32, 0x00]);
            let _ = umd.set_value("ShaderCache_DEF", &"2");
            let _ = umd.set_value("ShaderCache_NA", &"0");
            set_bin(&umd, "SurfaceFormatReplacements", &[0x31, 0x00]);
            let _ = umd.set_value("SurfaceFormatReplacements_DEF", &"1");
            set_bin(&umd, "TextureOpt", &[0x31, 0x00]);
            set_bin(&umd, "TFQ", &[0x32, 0x00]);
            set_bin(&umd, "AnisoType", &[0x30, 0x00]);
            set_bin(&umd, "AnisoDegree", &[0x30, 0x00]);
            set_bin(&umd, "AntiAlias", &[0x30, 0x00]);
            set_bin(&umd, "AntiAliasSamples", &[0x30, 0x00]);
            set_bin(&umd, "EQAA", &[0x30, 0x00]);
            set_bin(&umd, "MLF", &[0x30, 0x00]);
            set_bin(&umd, "VSyncControl", &[0x30, 0x00]);
            let _ = umd.set_value("VSyncControl_DEF", &"0");
            set_bin(&umd, "Tessellation_OPTION", &[0x32, 0x00]);
            set_bin(&umd, "Tessellation", &[0x34, 0x00]);
        }
    }
}

fn main() {
    enforce_amd_tweaks();
    disable_pcie_aspm_silent();
    thread::sleep(Duration::from_secs(12));
    enforce_amd_tweaks();
}
