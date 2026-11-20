#![windows_subsystem = "windows"]

use std::borrow::Cow;
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::thread;
use std::time::Duration;
use winreg::enums::*;
use winreg::{RegKey, RegValue};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DISPLAY_CLASS: &str =
    r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
const GRAPHICS_DRIVERS: &str = r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers";
const DWM_KEY: &str = r"SOFTWARE\Microsoft\Windows\Dwm";
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
    // Eliminate PCIe link-state micro-stalls during Enduro iGPU/dGPU frame copies
    let _ = Command::new("powercfg")
        .args(["/setacvalueindex", "SCHEME_CURRENT", "SUB_PCIEXPRESS", "ASPM", "0"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let _ = Command::new("powercfg")
        .args(["/setactive", "SCHEME_CURRENT"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
}

fn enforce_amd_tweaks() {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);

    // 1. Global 10s TDR protection & HAGS off (GCN 1.0 stability)
    if let Ok(gd) = hklm.open_subkey_with_flags(GRAPHICS_DRIVERS, KEY_WRITE) {
        let _ = gd.set_value("TdrDelay", &10u32);
        let _ = gd.set_value("TdrDdiDelay", &10u32);
        let _ = gd.set_value("HwSchMode", &1u32);
    }

    // 2. Disable Windows MPO (fixes hybrid Intel + AMD late-game frame-pacing stutters)
    if let Ok((dwm, _)) = hklm.create_subkey(DWM_KEY) {
        let _ = dwm.set_value("OverlayTestMode", &5u32);
    }

    // 3. Boost game thread & GPU submission priority in Windows MMCSS
    if let Ok(prof) = hklm.open_subkey_with_flags(MMCSS_PROFILE, KEY_WRITE) {
        let _ = prof.set_value("SystemResponsiveness", &10u32);
    }
    if let Ok((games, _)) = hklm.create_subkey(MMCSS_GAMES) {
        let _ = games.set_value("GPU Priority", &8u32);
        let _ = games.set_value("Priority", &6u32);
        let _ = games.set_value("Scheduling Category", &"High");
        let _ = games.set_value("SFIO Priority", &"High");
    }

    // 4. Dynamically locate AMD Oland dGPU key (never touch Intel iGPU or gamma)
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

        // Stability, Enduro DMA copy-back & anti-stutter unlocks
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
        let _ = gpu_key.set_value("DisableBlockWrite", &0u32);
        let _ = gpu_key.set_value("DisableEarlyZ", &0u32);
        let _ = gpu_key.set_value("DisableFBC", &1u32);
        let _ = gpu_key.set_value("DisableDrmdmaPowerGating", &1u32);

        // Unlocked GCN 1.0 KMD queue & scheduling tweaks
        let _ = gpu_key.set_value("KMD_EnablePreemption", &0u32);
        let _ = gpu_key.set_value("KMD_EnableComputeQueue", &1u32);
        let _ = gpu_key.set_value("KMD_EnableInternalLargePage", &0u32);
        let _ = gpu_key.set_value("KMD_DeLagEnabled", &0u32);
        let _ = gpu_key.set_value("KMD_FRTEnabled", &0u32);

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

        // Unlocked UMD gaming profile (no gamma/color keys touched)
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
            set_bin(&umd, "SurfaceFormatReplacements", &[0x31, 0x00]);
            let _ = umd.set_value("SurfaceFormatReplacements_DEF", &"1");
            set_bin(&umd, "TextureOpt", &[0x31, 0x00]);
            set_bin(&umd, "TFQ", &[0x32, 0x00]);
            set_bin(&umd, "AnisoType", &[0x30, 0x00]);
            set_bin(&umd, "AnisoDegree", &[0x30, 0x00]);
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
