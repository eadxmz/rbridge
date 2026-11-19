#![windows_subsystem = "windows"]

use std::borrow::Cow;
use std::thread;
use std::time::Duration;
use winreg::enums::*;
use winreg::{RegKey, RegValue};

const DISPLAY_CLASS: &str =
    r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
const GRAPHICS_DRIVERS: &str = r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers";

fn set_bin(key: &RegKey, name: &str, bytes: &[u8]) {
    let val = RegValue {
        bytes: Cow::Borrowed(bytes),
        vtype: REG_BINARY,
    };
    let _ = key.set_raw_value(name, &val);
}

fn enforce_amd_tweaks() {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);

    // 1. 10-second TDR timeout protection (global)
    if let Ok(gd) = hklm.open_subkey_with_flags(GRAPHICS_DRIVERS, KEY_WRITE) {
        let _ = gd.set_value("TdrDelay", &10u32);
        let _ = gd.set_value("TdrDdiDelay", &10u32);
        let _ = gd.set_value("HwSchMode", &1u32); // Keep HAGS off for GCN 1.0 stability
    }

    // 2. Dynamically locate AMD Oland dGPU key (never touch Intel iGPU key)
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

        // Re-apply KMD keys that amdkmdag.sys resets on boot
        let _ = gpu_key.set_value("EnableUlps", &0u32);
        let _ = gpu_key.set_value("EnableUlps_NA", &"0");
        let _ = gpu_key.set_value("PP_GPUPowerDownEnabled", &0u32);
        let _ = gpu_key.set_value("BacoEnable", &0u32);
        let _ = gpu_key.set_value("EnableCrossFireAutoLink", &0u32);
        let _ = gpu_key.set_value("StutterMode", &0u32);
        let _ = gpu_key.set_value("EnableAspmL0s", &0u32);
        let _ = gpu_key.set_value("EnableAspmL1", &0u32);
        let _ = gpu_key.set_value("EnablePcieGen3", &1u32);
        let _ = gpu_key.set_value("PP_ODNFeatureEnable", &1u32);
        let _ = gpu_key.set_value("PP_DisablePowerContainment", &1u32);
        let _ = gpu_key.set_value("PP_SclkDeepSleepDisable", &1u32);
        let _ = gpu_key.set_value("PP_MclkDeepSleepDisable", &1u32);
        let _ = gpu_key.set_value("PP_ThermalAutoThrottlingEnable", &0u32);
        let _ = gpu_key.set_value("KMD_DeLagEnabled", &1u32);
        let _ = gpu_key.set_value("OGL_VBO_Alloc_Policy", &1u32);
        let _ = gpu_key.set_value("OGL_EnableTripleBuffer", &0u32);

        // Enforce UMD performance profile (no gamma/color keys touched)
        if let Ok((umd, _)) = gpu_key.create_subkey("UMD") {
            set_bin(&umd, "FlipQueueSize", &[0x31, 0x00]);
            set_bin(&umd, "FlipQueueSize_SET", &[0x31, 0x00]);
            let _ = umd.set_value("FlipQueueSize_DEF", &"1");
            set_bin(&umd, "Main3D", &[0x31, 0x00]);
            set_bin(&umd, "Main3D_SET", &[0x31, 0x00]);
            let _ = umd.set_value("Main3D_DEF", &"1");
            set_bin(&umd, "ShaderCache", &[0x32, 0x00]);
            let _ = umd.set_value("ShaderCache_DEF", &"2");
            set_bin(&umd, "SurfaceFormatReplacements", &[0x31, 0x00]);
            let _ = umd.set_value("SurfaceFormatReplacements_DEF", &"1");
            set_bin(&umd, "TextureOpt", &[0x31, 0x00]);
            set_bin(&umd, "TFQ", &[0x32, 0x00]);
            set_bin(&umd, "AnisoType", &[0x30, 0x00]);
            set_bin(&umd, "AnisoDegree", &[0x30, 0x00]);
            set_bin(&umd, "Tessellation_OPTION", &[0x32, 0x00]);
            set_bin(&umd, "Tessellation", &[0x38, 0x00]);
        }
    }
}

fn main() {
    enforce_amd_tweaks();
    thread::sleep(Duration::from_secs(12));
    enforce_amd_tweaks();
}