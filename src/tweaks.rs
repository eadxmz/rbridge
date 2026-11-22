use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use winreg::enums::*;
use winreg::RegKey;

use crate::debloat;

pub const DISPLAY_CLASS: &str =
    r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
pub const RBRIDGE_REG: &str = r"SOFTWARE\RBridge";
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Clone, Debug)]
pub struct TweakState {
    pub ulps_disabled: bool,
    pub stutter_mode_off: bool,
    pub mpo_disabled: bool,
    pub gamedvr_disabled: bool,
    pub flip_model_enabled: bool,
    pub tessellation_4x: bool,
    pub msi_mode_enabled: bool,
    pub ac_cores_unparked: bool,
    pub boot_guard_installed: bool,
    pub gui_installed: bool,
}

impl Default for TweakState {
    fn default() -> Self {
        Self {
            ulps_disabled: true,
            stutter_mode_off: true,
            mpo_disabled: true,
            gamedvr_disabled: true,
            flip_model_enabled: true,
            tessellation_4x: true,
            msi_mode_enabled: true,
            ac_cores_unparked: true,
            boot_guard_installed: false,
            gui_installed: false,
        }
    }
}

pub fn is_elevated() -> bool {
    Command::new("net")
        .arg("session")
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn relaunch_as_admin() {
    if let Ok(exe) = std::env::current_exe() {
        let exe_str = exe.to_string_lossy().replace('\'', "''");
        let ps = format!("Start-Process -FilePath '{}' -Verb RunAs", exe_str);
        let _ = Command::new("powershell")
            .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &ps])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
        std::process::exit(0);
    }
}

pub fn find_amd_class_subkey() -> Option<String> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let class_key = hklm.open_subkey_with_flags(DISPLAY_CLASS, KEY_READ).ok()?;
    for sub in class_key.enum_keys().filter_map(|k| k.ok()) {
        if sub.len() != 4 || !sub.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let full = format!(r"{}\{}", DISPLAY_CLASS, sub);
        if let Ok(k) = hklm.open_subkey_with_flags(&full, KEY_READ) {
            let match_id = k
                .get_value::<String, _>("MatchingDeviceId")
                .unwrap_or_default()
                .to_ascii_lowercase();
            let desc = k
                .get_value::<String, _>("DriverDesc")
                .unwrap_or_default()
                .to_ascii_lowercase();
            if match_id.contains("ven_1002") || desc.contains("radeon") || desc.contains("amd") {
                return Some(full);
            }
        }
    }
    None
}

pub fn read_tweak_state() -> TweakState {
    let mut st = TweakState::default();
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    if let Some(amd_path) = find_amd_class_subkey() {
        if let Ok(k) = hklm.open_subkey_with_flags(&amd_path, KEY_READ) {
            st.ulps_disabled = k.get_value::<u32, _>("EnableUlps").unwrap_or(1) == 0;
            st.stutter_mode_off = k.get_value::<u32, _>("StutterMode").unwrap_or(1) == 0;
        }
        let umd_path = format!(r"{}\UMD", amd_path);
        if let Ok(umd) = hklm.open_subkey_with_flags(&umd_path, KEY_READ) {
            let tess = umd
                .get_value::<String, _>("Tessellation_OPTION")
                .unwrap_or_default();
            st.tessellation_4x = tess == "2";
        }
    }

    if let Ok(dwm) = hklm.open_subkey_with_flags(r"SOFTWARE\Microsoft\Windows\Dwm", KEY_READ) {
        st.mpo_disabled = dwm.get_value::<u32, _>("OverlayTestMode").unwrap_or(0) == 5;
    }

    if let Ok(gcs) = hkcu.open_subkey_with_flags(r"System\GameConfigStore", KEY_READ) {
        st.gamedvr_disabled = gcs.get_value::<u32, _>("GameDVR_Enabled").unwrap_or(1) == 0;
    }

    if let Ok(dx) = hkcu.open_subkey_with_flags(
        r"Software\Microsoft\DirectX\UserGpuPreferences",
        KEY_READ,
    ) {
        let global = dx
            .get_value::<String, _>("DirectXUserGlobalSettings")
            .unwrap_or_default();
        st.flip_model_enabled = global.contains("SwapEffectUpgradeEnable=1");
    }

    if let Ok(rb) = hklm.open_subkey_with_flags(RBRIDGE_REG, KEY_READ) {
        st.msi_mode_enabled = rb.get_value::<u32, _>("MsiSupported").unwrap_or(1) != 0;
        st.ac_cores_unparked = rb.get_value::<u32, _>("AcCoreUnpark").unwrap_or(1) != 0;
    }

    st.boot_guard_installed = Command::new("schtasks")
        .args(["/Query", "/TN", "RBridgeBootGuard"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    st.gui_installed = Path::new(r"C:\Program Files\RBridge\rbridge.exe").exists();
    st
}

fn apply_msi_mode(enable: bool) -> usize {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let Ok(pci) = hklm.open_subkey_with_flags(r"SYSTEM\CurrentControlSet\Enum\PCI", KEY_READ)
    else {
        return 0;
    };
    let mut updated = 0usize;
    for dev in pci.enum_keys().filter_map(|k| k.ok()) {
        if !dev.to_ascii_uppercase().contains("VEN_1002") {
            continue;
        }
        let dev_path = format!(r"SYSTEM\CurrentControlSet\Enum\PCI\{}", dev);
        let Ok(dev_key) = hklm.open_subkey_with_flags(&dev_path, KEY_READ) else {
            continue;
        };
        for inst in dev_key.enum_keys().filter_map(|k| k.ok()) {
            let msi_path = format!(
                r"{}\{}\Device Parameters\Interrupt Management\MessageSignaledInterruptProperties",
                dev_path, inst
            );
            if let Ok((k, _)) = hklm.create_subkey(&msi_path) {
                if k.set_value("MSISupported", &(if enable { 1u32 } else { 0u32 }))
                    .is_ok()
                {
                    updated += 1;
                }
            }
            if enable {
                let aff_path = format!(
                    r"{}\{}\Device Parameters\Interrupt Management\Affinity Policy",
                    dev_path, inst
                );
                if let Ok((ak, _)) = hklm.create_subkey(&aff_path) {
                    let _ = ak.set_value("DevicePriority", &3u32);
                }
            }
        }
    }
    updated
}

pub fn apply_tweaks(cfg: &TweakState) -> Vec<String> {
    let mut logs = Vec::new();
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    if let Some(amd_path) = find_amd_class_subkey() {
        match hklm.open_subkey_with_flags(&amd_path, KEY_READ | KEY_WRITE) {
            Ok(k) => {
                let _ = k.set_value("EnableUlps", &(if cfg.ulps_disabled { 0u32 } else { 1u32 }));
                let _ = k.set_value("EnableUlps_NA", &"0");
                let _ = k.set_value(
                    "StutterMode",
                    &(if cfg.stutter_mode_off { 0u32 } else { 1u32 }),
                );
                let _ = k.set_value("KMD_EnableInternalLargePage", &2u32);
                let _ = k.set_value("PP_SclkDeepSleepDisable", &1u32);
                let _ = k.set_value("PP_ThermalAutoThrottlingEnable", &0u32);
                let _ = k.set_value("D3ColdSupported", &0u32);
                logs.push(format!("[OK] Applied AMD driver registry keys ({})", amd_path));
            }
            Err(e) => logs.push(format!("[ERR] Cannot write AMD driver key (run as Admin): {}", e)),
        }

        let umd_path = format!(r"{}\UMD", amd_path);
        if let Ok((umd, _)) = hklm.create_subkey(&umd_path) {
            if cfg.tessellation_4x {
                let _ = umd.set_value("Tessellation_OPTION", &"2");
                let _ = umd.set_value("Tessellation", &"4");
                logs.push("[OK] Set 4x Tessellation cap in UMD".into());
            } else {
                let _ = umd.set_value("Tessellation_OPTION", &"0");
                let _ = umd.set_value("Tessellation", &"64");
                logs.push("[OK] Restored application-controlled Tessellation".into());
            }
        }
    } else {
        logs.push("[WARN] AMD VEN_1002 Display Class key not found".into());
    }

    if let Ok((dwm, _)) = hklm.create_subkey(r"SOFTWARE\Microsoft\Windows\Dwm") {
        if cfg.mpo_disabled {
            let _ = dwm.set_value("OverlayTestMode", &5u32);
            logs.push("[OK] Disabled Windows MPO (OverlayTestMode = 5)".into());
        } else {
            let _ = dwm.delete_value("OverlayTestMode");
            logs.push("[OK] Re-enabled default Windows MPO".into());
        }
    }

    if let Ok((gcs, _)) = hkcu.create_subkey(r"System\GameConfigStore") {
        let _ = gcs.set_value(
            "GameDVR_Enabled",
            &(if cfg.gamedvr_disabled { 0u32 } else { 1u32 }),
        );
    }
    if let Ok((pol, _)) = hklm.create_subkey(r"SOFTWARE\Policies\Microsoft\Windows\GameDVR") {
        let _ = pol.set_value(
            "AllowGameDVR",
            &(if cfg.gamedvr_disabled { 0u32 } else { 1u32 }),
        );
    }
    logs.push(format!(
        "[OK] GameDVR {}",
        if cfg.gamedvr_disabled { "disabled" } else { "enabled" }
    ));

    if let Ok((dx, _)) = hkcu.create_subkey(r"Software\Microsoft\DirectX\UserGpuPreferences") {
        let val = if cfg.flip_model_enabled {
            "VRROptimizeEnable=0;SwapEffectUpgradeEnable=1;"
        } else {
            "VRROptimizeEnable=0;SwapEffectUpgradeEnable=0;"
        };
        let _ = dx.set_value("DirectXUserGlobalSettings", &val);
        logs.push(format!(
            "[OK] DX10/11 Flip Model {}",
            if cfg.flip_model_enabled { "forced ON" } else { "disabled" }
        ));
    }

    let msi_hits = apply_msi_mode(cfg.msi_mode_enabled);
    logs.push(format!(
        "[OK] PCIe MSI Mode = {} ({} VEN_1002 instance(s))",
        if cfg.msi_mode_enabled { 1 } else { 0 },
        msi_hits
    ));

    if cfg.ac_cores_unparked {
        let _ = Command::new("powercfg")
            .args(["-setacvalueindex", "SCHEME_CURRENT", "SUB_PROCESSOR", "CPMINCORES", "100"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        let _ = Command::new("powercfg")
            .args(["-setacvalueindex", "SCHEME_CURRENT", "SUB_PCIEXPRESS", "ASPM", "0"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        let _ = Command::new("powercfg")
            .args(["-setactive", "SCHEME_CURRENT"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        logs.push("[OK] Unparked AC CPU cores (100%) and disabled PCIe ASPM on AC".into());
    }

    if let Ok((rb, _)) = hklm.create_subkey(RBRIDGE_REG) {
        let _ = rb.set_value("UlpsDisabled", &(cfg.ulps_disabled as u32));
        let _ = rb.set_value("StutterOff", &(cfg.stutter_mode_off as u32));
        let _ = rb.set_value("MpoDisabled", &(cfg.mpo_disabled as u32));
        let _ = rb.set_value("GameDvrDisabled", &(cfg.gamedvr_disabled as u32));
        let _ = rb.set_value("FlipModel", &(cfg.flip_model_enabled as u32));
        let _ = rb.set_value("Tess4x", &(cfg.tessellation_4x as u32));
        let _ = rb.set_value("MsiSupported", &(cfg.msi_mode_enabled as u32));
        let _ = rb.set_value("AcCoreUnpark", &(cfg.ac_cores_unparked as u32));
    }

    logs
}

pub fn install_boot_guard() -> Result<String, String> {
    let installed = PathBuf::from(r"C:\Program Files\RBridge\rbridge.exe");
    let exe_path = if installed.exists() {
        installed
    } else {
        std::env::current_exe().map_err(|e| e.to_string())?
    };

    let tr = format!("\"{}\" --daemon", exe_path.display());
    let out = Command::new("schtasks")
        .args([
            "/Create",
            "/F",
            "/TN",
            "RBridgeBootGuard",
            "/SC",
            "ONLOGON",
            "/RL",
            "HIGHEST",
            "/TR",
            &tr,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("Failed to invoke schtasks: {}", e))?;

    if out.status.success() {
        Ok(format!("Installed Boot Guard task ({})", tr))
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub fn remove_boot_guard() -> Result<String, String> {
    let out = Command::new("schtasks")
        .args(["/Delete", "/F", "/TN", "RBridgeBootGuard"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("Failed to invoke schtasks: {}", e))?;

    if out.status.success() {
        Ok("Removed RBridgeBootGuard scheduled task.".into())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub fn install_rbridge_gui() -> Result<String, String> {
    let target_dir = PathBuf::from(r"C:\Program Files\RBridge");
    fs::create_dir_all(&target_dir)
        .map_err(|e| format!("Cannot create C:\\Program Files\\RBridge (run as Admin): {}", e))?;

    let target_exe = target_dir.join("rbridge.exe");
    let cur_exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if !cur_exe
        .to_string_lossy()
        .eq_ignore_ascii_case(&target_exe.to_string_lossy())
    {
        fs::copy(&cur_exe, &target_exe)
            .map_err(|e| format!("Failed copying rbridge.exe to Program Files: {}", e))?;
    }

    let ps = r#"$ws = New-Object -ComObject WScript.Shell; $s = $ws.CreateShortcut('C:\ProgramData\Microsoft\Windows\Start Menu\Programs\RBridge.lnk'); $s.TargetPath = 'C:\Program Files\RBridge\rbridge.exe'; $s.WorkingDirectory = 'C:\Program Files\RBridge'; $s.Description = 'RBridge Radeon Control Center'; $s.Save()"#;
    let _ = Command::new("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", ps])
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    Ok(r"Installed RBridge Control Center to C:\Program Files\RBridge\rbridge.exe + Start Menu.".into())
}

pub fn uninstall_rbridge_gui() -> Result<String, String> {
    let _ = remove_boot_guard();
    let lnk = Path::new(r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\RBridge.lnk");
    if lnk.exists() {
        let _ = fs::remove_file(lnk);
    }
    Ok("Removed Start Menu shortcut and Boot Guard task.".into())
}

pub fn run_boot_guard_daemon() {
    std::thread::sleep(Duration::from_secs(12));
    let mut cfg = read_tweak_state();
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    if let Ok(rb) = hklm.open_subkey_with_flags(RBRIDGE_REG, KEY_READ) {
        cfg.ulps_disabled = rb.get_value::<u32, _>("UlpsDisabled").unwrap_or(1) != 0;
        cfg.stutter_mode_off = rb.get_value::<u32, _>("StutterOff").unwrap_or(1) != 0;
        cfg.mpo_disabled = rb.get_value::<u32, _>("MpoDisabled").unwrap_or(1) != 0;
        cfg.gamedvr_disabled = rb.get_value::<u32, _>("GameDvrDisabled").unwrap_or(1) != 0;
        cfg.flip_model_enabled = rb.get_value::<u32, _>("FlipModel").unwrap_or(1) != 0;
        cfg.tessellation_4x = rb.get_value::<u32, _>("Tess4x").unwrap_or(1) != 0;
        cfg.msi_mode_enabled = rb.get_value::<u32, _>("MsiSupported").unwrap_or(1) != 0;
        cfg.ac_cores_unparked = rb.get_value::<u32, _>("AcCoreUnpark").unwrap_or(1) != 0;
    }
    let _ = apply_tweaks(&cfg);
    let _ = debloat::strip_live_bloat();
}
