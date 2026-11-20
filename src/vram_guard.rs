use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use winreg::enums::*;
use winreg::RegKey;

const USER_GPU_PREFS: &str = r"Software\Microsoft\DirectX\UserGpuPreferences";
const FLIP_KEY: &str = "DirectXUserGlobalSettings";

fn merge_flip_model_settings(existing: &str) -> String {
    let mut pairs: Vec<(String, String)> = existing
        .split(';')
        .filter_map(|part| {
            let trimmed = part.trim();
            if trimmed.is_empty() {
                return None;
            }
            let (k, v) = trimmed.split_once('=')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect();

    for (target_k, target_v) in [
        ("SwapEffectUpgradeEnable", "1"),
        ("VRROptimizeEnable", "0"),
    ] {
        if let Some((_, v)) = pairs
            .iter_mut()
            .find(|(k, _)| k.eq_ignore_ascii_case(target_k))
        {
            *v = target_v.to_string();
        } else {
            pairs.push((target_k.to_string(), target_v.to_string()));
        }
    }

    let mut out = String::new();
    for (k, v) in pairs {
        out.push_str(&k);
        out.push('=');
        out.push_str(&v);
        out.push(';');
    }
    out
}

pub fn discover_background_apps() -> Vec<PathBuf> {
    let mut found = Vec::new();
    let static_candidates = [
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe",
        r"C:\Program Files (x86)\BraveSoftware\Brave-Browser\Application\brave.exe",
        r"C:\Program Files\Mozilla Firefox\firefox.exe",
        r"C:\Program Files (x86)\Mozilla Firefox\firefox.exe",
        r"C:\Program Files (x86)\Steam\steam.exe",
        r"C:\Program Files (x86)\Steam\bin\cef\cef.win7x64\steamwebhelper.exe",
        r"C:\Program Files (x86)\Steam\bin\cef\cef.win7\steamwebhelper.exe",
        r"C:\Program Files (x86)\Epic Games\Launcher\Portal\Binaries\Win64\EpicWebHelper.exe",
        r"C:\Program Files (x86)\Epic Games\Launcher\Portal\Binaries\Win32\EpicWebHelper.exe",
        r"C:\Program Files\Microsoft VS Code\Code.exe",
        r"C:\Program Files\obs-studio\bin\64bit\obs64.exe",
    ];

    for p in static_candidates {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            found.push(pb);
        }
    }

    // Scan per-user AppData installs (works even when daemon runs as SYSTEM)
    if let Ok(users) = fs::read_dir(r"C:\Users") {
        for entry in users.flatten() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if matches!(
                name.as_str(),
                "public" | "default" | "default user" | "all users"
            ) || !entry.path().is_dir()
            {
                continue;
            }
            let base = entry.path();
            let user_rel = [
                r"AppData\Local\Google\Chrome\Application\chrome.exe",
                r"AppData\Local\BraveSoftware\Brave-Browser\Application\brave.exe",
                r"AppData\Local\Programs\Microsoft VS Code\Code.exe",
                r"AppData\Local\Programs\Opera\opera.exe",
                r"AppData\Local\Programs\Opera GX\opera.exe",
                r"AppData\Local\Vivaldi\Application\vivaldi.exe",
                r"AppData\Roaming\Spotify\Spotify.exe",
            ];
            for rel in user_rel {
                let p = base.join(rel);
                if p.is_file() {
                    found.push(p);
                }
            }

            // Discord versioned folders (AppData\Local\Discord\app-*\Discord.exe)
            let discord_dir = base.join(r"AppData\Local\Discord");
            if let Ok(subs) = fs::read_dir(&discord_dir) {
                for sub in subs.flatten() {
                    let sname = sub.file_name().to_string_lossy().to_ascii_lowercase();
                    if sname.starts_with("app-") {
                        let exe = sub.path().join("Discord.exe");
                        if exe.is_file() {
                            found.push(exe);
                        }
                    }
                }
            }
        }
    }

    found.sort();
    found.dedup();
    found
}

fn apply_to_user_hive(user_root: &RegKey, apps: &[PathBuf]) -> io::Result<usize> {
    let (prefs, _) = user_root.create_subkey(USER_GPU_PREFS)?;

    // 1. Force DX10/DX11 Flip Model (Optimizations for Windowed Games)
    let cur_global = prefs.get_value::<String, _>(FLIP_KEY).unwrap_or_default();
    let merged = merge_flip_model_settings(&cur_global);
    prefs.set_value(FLIP_KEY, &merged)?;

    // 2. Pin discovered browsers/Electron helpers to Intel iGPU (GpuPreference=1;)
    let mut count = 0usize;
    for app in apps {
        let key_name = app.to_string_lossy();
        if prefs.set_value(key_name.as_ref(), &"GpuPreference=1;").is_ok() {
            count += 1;
        }
    }
    Ok(count)
}

pub fn apply_to_current_user() -> io::Result<usize> {
    let apps = discover_background_apps();
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    apply_to_user_hive(&hkcu, &apps)
}

pub fn apply_to_all_loaded_users() {
    let apps = discover_background_apps();
    let hku = RegKey::predef(HKEY_USERS);
    for sid in hku.enum_keys().filter_map(|k| k.ok()) {
        if !sid.starts_with("S-1-5-21-") || sid.ends_with("_Classes") {
            continue;
        }
        if let Ok(user_hive) = hku.open_subkey_with_flags(&sid, KEY_READ | KEY_WRITE) {
            let _ = apply_to_user_hive(&user_hive, &apps);
        }
    }
}

pub fn pin_game_to_amd(exe_path: &Path) -> io::Result<()> {
    if !exe_path.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("File not found: {}", exe_path.display()),
        ));
    }
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (prefs, _) = hkcu.create_subkey(USER_GPU_PREFS)?;
    let full = fs::canonicalize(exe_path).unwrap_or_else(|_| exe_path.to_path_buf());
    let clean = full
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_string();
    prefs.set_value(&clean, &"GpuPreference=2;")?;
    Ok(())
}
