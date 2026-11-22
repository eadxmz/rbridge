use std::env;
use std::path::PathBuf;
use winreg::enums::*;
use winreg::RegKey;

const GPU_PREFS_KEY: &str = r"Software\Microsoft\DirectX\UserGpuPreferences";

#[derive(Clone, Debug)]
pub struct GpuPinEntry {
    pub exe_path: String,
    pub high_performance: bool,
}

pub fn list_pinned_apps() -> Vec<GpuPinEntry> {
    let mut entries = Vec::new();
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(k) = hkcu.open_subkey_with_flags(GPU_PREFS_KEY, KEY_READ) else {
        return entries;
    };
    for (name, val) in k.enum_values().filter_map(|v| v.ok()) {
        if name.eq_ignore_ascii_case("DirectXUserGlobalSettings") {
            continue;
        }
        let s = String::from_utf8_lossy(&val.bytes)
            .trim_end_matches('\0')
            .to_string();
        let high = s.contains("GpuPreference=2");
        entries.push(GpuPinEntry {
            exe_path: name,
            high_performance: high,
        });
    }
    entries.sort_by(|a, b| a.exe_path.cmp(&b.exe_path));
    entries
}

pub fn pin_executable(path: &str, high_performance: bool) -> Result<String, String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (k, _) = hkcu
        .create_subkey(GPU_PREFS_KEY)
        .map_err(|e| format!("Cannot open UserGpuPreferences: {}", e))?;
    let val = if high_performance {
        "GpuPreference=2;"
    } else {
        "GpuPreference=1;"
    };
    k.set_value(path, &val)
        .map_err(|e| format!("Failed writing GPU preference: {}", e))?;
    Ok(format!(
        "Pinned '{}' -> {}",
        path,
        if high_performance {
            "AMD HD 8670M (High Performance)"
        } else {
            "Intel HD 4400 (VRAM Quarantine)"
        }
    ))
}

pub fn remove_pin(path: &str) -> Result<String, String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let k = hkcu
        .open_subkey_with_flags(GPU_PREFS_KEY, KEY_READ | KEY_WRITE)
        .map_err(|e| format!("Cannot open UserGpuPreferences: {}", e))?;
    k.delete_value(path)
        .map_err(|e| format!("Failed removing pin: {}", e))?;
    Ok(format!("Removed GPU pin for '{}'", path))
}

pub fn quarantine_background_apps() -> Result<usize, String> {
    let local = env::var("LOCALAPPDATA").unwrap_or_default();
    let pf = env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    let pf86 = env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into());

    let candidates: Vec<PathBuf> = vec![
        PathBuf::from(&pf).join(r"Google\Chrome\Application\chrome.exe"),
        PathBuf::from(&pf86).join(r"Google\Chrome\Application\chrome.exe"),
        PathBuf::from(&pf86).join(r"Microsoft\Edge\Application\msedge.exe"),
        PathBuf::from(&pf).join(r"Microsoft\Edge\Application\msedge.exe"),
        PathBuf::from(&pf).join(r"BraveSoftware\Brave-Browser\Application\brave.exe"),
        PathBuf::from(&pf).join(r"Mozilla Firefox\firefox.exe"),
        PathBuf::from(&pf86).join(r"Steam\steam.exe"),
        PathBuf::from(&pf86).join(r"Steam\bin\cef\cef.win7x64\steamwebhelper.exe"),
        PathBuf::from(&local).join(r"Spotify\Spotify.exe"),
        PathBuf::from(&local).join(r"Programs\Microsoft VS Code\Code.exe"),
        PathBuf::from(&local).join(r"Discord\Update.exe"),
    ];

    let mut count = 0usize;
    for path in candidates {
        if path.exists() && pin_executable(&path.to_string_lossy(), false).is_ok() {
            count += 1;
        }
    }

    // Also catch versioned Discord app-x.x.x\Discord.exe
    let discord_root = PathBuf::from(&local).join("Discord");
    if let Ok(entries) = std::fs::read_dir(discord_root) {
        for e in entries.flatten() {
            let exe = e.path().join("Discord.exe");
            if exe.exists() && pin_executable(&exe.to_string_lossy(), false).is_ok() {
                count += 1;
            }
        }
    }

    Ok(count)
}
