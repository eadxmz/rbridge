use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use winreg::enums::*;
use winreg::RegKey;

use crate::debloat;
use crate::tweaks;

const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DriverPackageKind {
    AmdPro21Q21,
    AmernimeRid,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct DetectedPackage {
    pub kind: DriverPackageKind,
    pub label: String,
    pub inf_paths: Vec<PathBuf>,
    pub root_dir: PathBuf,
}

#[derive(Clone, Debug)]
pub struct PatchOptions {
    pub strip_amd_bloat: bool,
    pub keep_msi_supported: bool,
    pub rebrand_r8_m670: bool,
    pub install_driver: bool,
    pub install_gui: bool,
    pub install_boot_guard: bool,
}

impl Default for PatchOptions {
    fn default() -> Self {
        Self {
            strip_amd_bloat: true,
            keep_msi_supported: true,
            rebrand_r8_m670: true,
            install_driver: true,
            install_gui: true,
            install_boot_guard: true,
        }
    }
}

fn detect_local_amd_dev_id() -> Option<String> {
    let amd_path = tweaks::find_amd_class_subkey()?;
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let k = hklm.open_subkey_with_flags(&amd_path, KEY_READ).ok()?;
    let match_id = k
        .get_value::<String, _>("MatchingDeviceId")
        .ok()?
        .to_ascii_lowercase();
    let pos = match_id.find("dev_")?;
    let tail = &match_id[pos..];
    if tail.len() >= 8 {
        Some(tail[..8].to_string())
    } else {
        None
    }
}

fn collect_infs(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            let name = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_ascii_lowercase();
            if name.contains("ccc2") || name == ".git" || name == "msftparse" {
                continue;
            }
            collect_infs(&p, depth - 1, out);
        } else if p
            .extension()
            .map(|e| e.eq_ignore_ascii_case("inf"))
            .unwrap_or(false)
        {
            out.push(p);
        }
    }
}

fn rid_priority(path: &Path) -> (u8, String) {
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    let tier = if name.contains("gcnstd") {
        0
    } else if name.contains("gcnflexdgpu") {
        1
    } else if name.contains("gcnpro") {
        2
    } else {
        3
    };
    (tier, name)
}

pub fn detect_driver_package(dir: &Path) -> DetectedPackage {
    let mut all_infs = Vec::new();
    collect_infs(dir, 6, &mut all_infs);

    let local_dev = detect_local_amd_dev_id();
    let mut pro_infs = Vec::new();
    let mut rid_matched = Vec::new();
    let mut rid_all = Vec::new();

    for inf in &all_infs {
        let fname = inf
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        if fname.contains("_msft") {
            continue;
        }
        if fname == "u0366524.inf" || fname == "u0369543.inf" {
            pro_infs.push(inf.clone());
            continue;
        }
        if let Ok(text) = fs::read_to_string(inf) {
            let lower = text.to_ascii_lowercase();
            let is_amd_display = lower.contains("ven_1002")
                && (lower.contains("amdkmdag") || lower.contains("ati2mtag"));
            if is_amd_display {
                rid_all.push(inf.clone());
                if let Some(ref dev) = local_dev {
                    if lower.contains(dev) {
                        rid_matched.push(inf.clone());
                    }
                }
            }
        }
    }

    if !pro_infs.is_empty() {
        return DetectedPackage {
            kind: DriverPackageKind::AmdPro21Q21,
            label: format!(
                "AMD Radeon PRO 21.Q2.1 ({} INF file(s) detected)",
                pro_infs.len()
            ),
            inf_paths: pro_infs,
            root_dir: dir.to_path_buf(),
        };
    }

    let mut chosen = if !rid_matched.is_empty() {
        rid_matched
    } else {
        rid_all
    };

    if !chosen.is_empty() {
        chosen.sort_by(|a, b| {
            let (ta, na) = rid_priority(a);
            let (tb, nb) = rid_priority(b);
            ta.cmp(&tb).then_with(|| nb.cmp(&na))
        });

        let dev_note = local_dev
            .as_deref()
            .map(|d| format!(" matching {}", d.to_ascii_uppercase()))
            .unwrap_or_default();
        return DetectedPackage {
            kind: DriverPackageKind::AmernimeRid,
            label: format!(
                "Amernime / R.ID Hybrid ({} compatible INF(s){})",
                chosen.len(),
                dev_note
            ),
            inf_paths: chosen,
            root_dir: dir.to_path_buf(),
        };
    }

    DetectedPackage {
        kind: DriverPackageKind::Unknown,
        label: "No compatible AMD 21.Q2.1 or Amernime/R.ID INF found".into(),
        inf_paths: Vec::new(),
        root_dir: dir.to_path_buf(),
    }
}

fn neutralize_ccc2_dirs(root: &Path, log: &impl Fn(String)) -> usize {
    let mut stripped = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if !p.is_dir() {
                continue;
            }
            let name = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_ascii_lowercase();
            if name.contains("ccc2_install") && !name.ends_with(".rbridge_stripped") {
                let renamed = p.with_extension("rbridge_stripped");
                if fs::rename(&p, &renamed).is_ok() {
                    stripped += 1;
                    log(format!("[DEBLOAT] Neutralized Adrenalin UI folder '{}'", name));
                }
            } else {
                stack.push(p);
            }
        }
    }
    stripped
}

fn heal_and_clean_package_tree(root: &Path, log: &impl Fn(String)) {
    let backup_dir = env::temp_dir().join("RBridge_INF_Backups");
    let _ = fs::create_dir_all(&backup_dir);

    let mut stack = vec![root.to_path_buf()];
    let mut restored_files = 0usize;
    while let Some(cur) = stack.pop() {
        let Ok(entries) = fs::read_dir(&cur) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            let name = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let lower = name.to_ascii_lowercase();

            if p.is_dir() {
                if lower == "amdfendr.rbridge_stripped" {
                    let dst = cur.join("amdfendr");
                    if fs::rename(&p, &dst).is_ok() {
                        log("[CLEAN] Restored 'amdfendr' folder for clean INF CopyFiles".into());
                        stack.push(dst);
                    }
                } else {
                    stack.push(p);
                }
                continue;
            }

            if lower.ends_with(".rbridge_bak") {
                let dst = backup_dir.join(&name);
                if fs::copy(&p, &dst).is_ok() {
                    let _ = fs::remove_file(&p);
                }
            } else if lower.ends_with(".exe.rbridge_stripped") {
                let restored_name = &name[..name.len() - ".rbridge_stripped".len()];
                let dst = cur.join(restored_name);
                if fs::rename(&p, &dst).is_ok() {
                    restored_files += 1;
                }
            }
        }
    }
    if restored_files > 0 {
        log(format!(
            "[CLEAN] Restored {} previously renamed binary file(s) so pnputil finds all payloads",
            restored_files
        ));
    }
}

fn clean_rel_subpath(raw: &str) -> String {
    let s = raw
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .trim_matches('"')
        .trim_start_matches(".\\")
        .trim_start_matches("./")
        .trim_matches(['\\', '/']);
    s.to_string()
}

fn parse_inf_payload_refs(inf_path: &Path) -> Vec<PathBuf> {
    let Ok(raw) = fs::read_to_string(inf_path) else {
        return Vec::new();
    };

    let mut strings: BTreeMap<String, String> = BTreeMap::new();
    let mut disk_paths: BTreeMap<String, String> = BTreeMap::new();
    let mut sys_dll_refs: Vec<PathBuf> = Vec::new();
    let mut other_refs: Vec<PathBuf> = Vec::new();

    #[derive(PartialEq, Eq)]
    enum Sec {
        None,
        Strings,
        Disks,
        Files,
    }

    let mut sec = Sec::None;
    for line in raw.lines() {
        let trim = line.trim();
        if trim.is_empty() || trim.starts_with(';') {
            continue;
        }
        if trim.starts_with('[') && trim.ends_with(']') {
            let low = trim.to_ascii_lowercase();
            sec = if low == "[strings]" || low.starts_with("[strings.") {
                Sec::Strings
            } else if low == "[sourcedisksnames]" || low.starts_with("[sourcedisksnames.") {
                Sec::Disks
            } else {
                Sec::None
            };
            continue;
        }
        if let Some((lhs, rhs)) = trim.split_once('=') {
            match sec {
                Sec::Strings => {
                    let k = lhs.trim().to_ascii_lowercase();
                    let v = rhs.split(';').next().unwrap_or("").trim().trim_matches('"');
                    strings.insert(k, v.to_string());
                }
                Sec::Disks => {
                    let id = lhs.trim().to_ascii_lowercase();
                    let fields: Vec<&str> = rhs.split(',').collect();
                    let path_raw = fields.get(3).copied().unwrap_or("");
                    disk_paths.insert(id, clean_rel_subpath(path_raw));
                }
                _ => {}
            }
        }
    }

    for val in disk_paths.values_mut() {
        if val.starts_with('%') && val.ends_with('%') && val.len() > 2 {
            let key = val[1..val.len() - 1].to_ascii_lowercase();
            if let Some(sub) = strings.get(&key) {
                *val = clean_rel_subpath(sub);
            }
        }
    }

    sec = Sec::None;
    for line in raw.lines() {
        let trim = line.trim();
        if trim.is_empty() || trim.starts_with(';') {
            continue;
        }
        if trim.starts_with('[') && trim.ends_with(']') {
            let low = trim.to_ascii_lowercase();
            sec = if low == "[sourcedisksfiles]" || low.starts_with("[sourcedisksfiles.") {
                Sec::Files
            } else {
                Sec::None
            };
            continue;
        }
        if sec != Sec::Files {
            continue;
        }
        if let Some((lhs, rhs)) = trim.split_once('=') {
            let fname = lhs.trim().trim_matches('"');
            if fname.is_empty() {
                continue;
            }
            let fields: Vec<&str> = rhs.split(',').collect();
            let disk_id = fields
                .first()
                .copied()
                .unwrap_or("")
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            let file_sub = fields
                .get(1)
                .map(|s| clean_rel_subpath(s))
                .unwrap_or_default();

            let mut rel = PathBuf::new();
            if let Some(dp) = disk_paths.get(&disk_id) {
                if !dp.is_empty() {
                    rel.push(dp);
                }
            }
            if !file_sub.is_empty() {
                rel.push(&file_sub);
            }
            rel.push(fname);

            let low_name = fname.to_ascii_lowercase();
            if low_name.ends_with(".sys") || low_name.ends_with(".dll") {
                if sys_dll_refs.len() < 30 {
                    sys_dll_refs.push(rel);
                }
            } else if other_refs.len() < 15 {
                other_refs.push(rel);
            }
        }
    }

    if !sys_dll_refs.is_empty() {
        sys_dll_refs
    } else {
        other_refs
    }
}

fn payload_exists(base_dir: &Path, rel: &Path) -> bool {
    let full = base_dir.join(rel);
    if full.exists() {
        return true;
    }
    if let Some(s) = full.to_str() {
        if s.len() > 2 {
            let mut alt = s[..s.len() - 1].to_string();
            alt.push('_');
            if Path::new(&alt).exists() {
                return true;
            }
        }
    }
    false
}

fn collect_candidate_dirs(root: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    out.push(root.to_path_buf());
    if depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        let name = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        if name == ".git" || name.contains("ccc2") || name == "msftparse" {
            continue;
        }
        collect_candidate_dirs(&p, depth - 1, out);
    }
}

fn stage_inf_into_payload_dir(
    inf_path: &Path,
    root_dir: &Path,
    log: &impl Fn(String),
) -> Result<PathBuf, String> {
    let refs = parse_inf_payload_refs(inf_path);
    if refs.is_empty() {
        return Ok(inf_path.to_path_buf());
    }

    let cur_parent = inf_path
        .parent()
        .ok_or_else(|| "INF has no parent directory".to_string())?;
    let cur_hits = refs.iter().filter(|r| payload_exists(cur_parent, r)).count();

    if cur_hits == refs.len() {
        return Ok(inf_path.to_path_buf());
    }

    let mut dirs = Vec::new();
    collect_candidate_dirs(root_dir, 5, &mut dirs);

    let mut best_dir = cur_parent.to_path_buf();
    let mut best_hits = cur_hits;

    for dir in dirs {
        let hits = refs.iter().filter(|r| payload_exists(&dir, r)).count();
        if hits > best_hits {
            best_hits = hits;
            best_dir = dir;
        }
    }

    if best_hits == 0 {
        log(format!(
            "[WARN] Could not locate payload files (e.g. '{}') under {}",
            refs[0].display(),
            root_dir.display()
        ));
        return Ok(inf_path.to_path_buf());
    }

    if best_dir != cur_parent {
        let fname = inf_path
            .file_name()
            .ok_or_else(|| "Invalid INF filename".to_string())?;
        let staged = best_dir.join(fname);
        fs::copy(inf_path, &staged).map_err(|e| {
            format!(
                "Failed staging {} into {}: {}",
                fname.to_string_lossy(),
                best_dir.display(),
                e
            )
        })?;
        log(format!(
            "[STAGE] Staged {} into payload directory {} ({}/{} sample binaries verified)",
            fname.to_string_lossy(),
            best_dir.display(),
            best_hits,
            refs.len()
        ));
        return Ok(staged);
    }

    Ok(inf_path.to_path_buf())
}

fn extract_declared_catalogs(inf_path: &Path) -> Vec<String> {
    let mut cats = Vec::new();
    if let Ok(raw) = fs::read_to_string(inf_path) {
        let mut in_version = false;
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                in_version = trimmed.eq_ignore_ascii_case("[version]");
                continue;
            }
            if !in_version || trimmed.starts_with(';') {
                continue;
            }
            if let Some((lhs, rhs)) = trimmed.split_once('=') {
                let key = lhs.trim().to_ascii_lowercase();
                if key == "catalogfile" || key.starts_with("catalogfile.") {
                    let val = rhs
                        .split(';')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .trim_matches('"');
                    if !val.is_empty() && !cats.iter().any(|c: &String| c.eq_ignore_ascii_case(val))
                    {
                        cats.push(val.to_string());
                    }
                }
            }
        }
    }
    if cats.is_empty() {
        let stem = inf_path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        cats.push(format!("{}.cat", stem));
    }
    cats
}

fn is_valid_ddinstall_section(sec: &str) -> bool {
    if sec.is_empty() {
        return false;
    }
    const BLOCKED_EXACT: &[&str] = &[
        "version",
        "strings",
        "sourcedisksnames",
        "sourcedisksfiles",
        "destinationdirs",
        "manufacturer",
        "controlflags",
        "rbridge_tweaks",
    ];
    if BLOCKED_EXACT.iter().any(|&b| sec == b) {
        return false;
    }
    const BLOCKED_SUFFIXES: &[&str] = &[
        ".services",
        ".hw",
        ".coinstallers",
        ".interfaces",
        ".wmi",
        ".logconfigoverride",
        "_inst",
        "_service_inst",
    ];
    !BLOCKED_SUFFIXES.iter().any(|&s| sec.ends_with(s))
}

fn patch_single_inf(
    inf_path: &Path,
    kind: &DriverPackageKind,
    opts: &PatchOptions,
    log: &impl Fn(String),
) -> Result<(), String> {
    let raw = fs::read_to_string(inf_path)
        .map_err(|e| format!("Failed reading {}: {}", inf_path.display(), e))?;

    let mut out_lines: Vec<String> = Vec::new();
    let mut restored_lines = 0usize;
    let mut hook_count = 0usize;
    let mut in_rbridge_section = false;
    let mut current_sec = String::new();

    for line in raw.lines() {
        // Automatically heal any lines commented out by older v2.0/v2.1 bloat stripping
        // so ServiceBinary / CopyFiles sections are 100% valid for pnputil
        let effective = if let Some(unstripped) = line.strip_prefix("; [RBridge Stripped] ") {
            restored_lines += 1;
            unstripped
        } else {
            line
        };

        let trimmed = effective.trim();
        let lower = trimmed.to_ascii_lowercase();

        if lower == "[rbridge_tweaks]" {
            in_rbridge_section = true;
            current_sec.clear();
            continue;
        }
        if in_rbridge_section {
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                in_rbridge_section = false;
            } else {
                continue;
            }
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            current_sec = trimmed[1..trimmed.len() - 1].trim().to_ascii_lowercase();
        }
        if lower == "addreg = rbridge_tweaks" {
            continue;
        }

        let mut modified = effective.to_string();
        if opts.rebrand_r8_m670 && *kind == DriverPackageKind::AmdPro21Q21 {
            if modified.contains("AMD Radeon HD 8670M") {
                modified = modified.replace("AMD Radeon HD 8670M", "AMD Radeon R8 M670");
            }
            if modified.contains("AMD Radeon R5 M230") {
                modified = modified.replace("AMD Radeon R5 M230", "AMD Radeon R8 M670");
            }
        }

        out_lines.push(modified);

        let is_hook_anchor = lower.starts_with("delreg")
            && (lower.contains("ati2mtag") || lower.contains("removedevicesettings"));
        if is_hook_anchor && is_valid_ddinstall_section(&current_sec) {
            out_lines.push("AddReg = RBridge_Tweaks".into());
            hook_count += 1;
        }
    }

    if hook_count == 0 {
        let mut fallback_lines = Vec::with_capacity(out_lines.len() + 16);
        let mut sec = String::new();
        for line in out_lines {
            let trimmed = line.trim();
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                sec = trimmed[1..trimmed.len() - 1].trim().to_ascii_lowercase();
            }
            let lower = trimmed.to_ascii_lowercase();
            let inject_after = lower.starts_with("addreg")
                && (lower.contains("ati2mtag") || lower.contains("kmd_reg"));
            fallback_lines.push(line);
            if inject_after && is_valid_ddinstall_section(&sec) && hook_count < 4 {
                fallback_lines.push("AddReg = RBridge_Tweaks".into());
                hook_count += 1;
            }
        }
        out_lines = fallback_lines;
    }

    let msi_val = if opts.keep_msi_supported { "1" } else { "0" };

    out_lines.push(String::new());
    out_lines.push("[RBridge_Tweaks]".into());
    out_lines.push("HKR,, EnableUlps, 0x00010001, 0".into());
    out_lines.push("HKR,, EnableUlps_NA, 0x00000000, \"0\"".into());
    out_lines.push("HKR,, StutterMode, 0x00010001, 0".into());
    out_lines.push("HKR,, D3ColdSupported, 0x00010001, 0".into());
    out_lines.push("HKR,, KMD_EnableInternalLargePage, 0x00010001, 2".into());
    out_lines.push("HKR,, PP_SclkDeepSleepDisable, 0x00010001, 1".into());
    out_lines.push("HKR,, PP_ThermalAutoThrottlingEnable, 0x00010001, 0".into());
    out_lines.push("HKR,, PP_EnableOverdrive, 0x00010001, 1".into());
    out_lines.push(format!(
        "HKR, \"Interrupt Management\\MessageSignaledInterruptProperties\", MSISupported, 0x00010001, {}",
        msi_val
    ));

    if *kind == DriverPackageKind::AmdPro21Q21 {
        out_lines.push("HKR, UMD, OGL_ThreadControl, 0x00010001, 1".into());
        out_lines.push("HKR, UMD, Tessellation_OPTION, 0x00000000, \"2\"".into());
        out_lines.push("HKR, UMD, Tessellation, 0x00000000, \"4\"".into());
        out_lines.push("HKR, UMD, CatalystAI, 0x00000000, \"32\"".into());
        out_lines.push("HKR, UMD, SurfaceFormatReplacements, 0x00000000, \"1\"".into());
    }

    let joined = out_lines.join("\r\n") + "\r\n";
    fs::write(inf_path, joined)
        .map_err(|e| format!("Failed writing {}: {}", inf_path.display(), e))?;

    if restored_lines > 0 {
        log(format!(
            "[HEAL] Restored {} previously stripped INF line(s) in {} (fixing ServiceBinary/CopyFiles)",
            restored_lines,
            inf_path.file_name().unwrap_or_default().to_string_lossy()
        ));
    }
    log(format!(
        "[PATCH] {} -> {} safe DDInstall hook(s) applied",
        inf_path.file_name().unwrap_or_default().to_string_lossy(),
        hook_count
    ));
    Ok(())
}

fn rebuild_and_sign_dir_catalogs(
    dir: &Path,
    cat_names: &[String],
    log: &impl Fn(String),
) -> Result<(), String> {
    for cat in cat_names {
        let p = dir.join(cat);
        if p.is_dir() {
            let _ = fs::remove_dir_all(&p);
        } else if p.exists() {
            let _ = fs::remove_file(&p);
        }
    }

    let tmp_dir = env::temp_dir().join("RBridge_Signed_Cats");
    let _ = fs::remove_dir_all(&tmp_dir);
    let _ = fs::create_dir_all(&tmp_dir);
    let tmp_cat = tmp_dir.join("rbridge_build.cat");

    let dir_s = dir.to_string_lossy().replace('\'', "''");
    let tmp_cat_s = tmp_cat.to_string_lossy().replace('\'', "''");

    let ps = format!(
        r#"$ErrorActionPreference = 'Stop'
$cert = Get-ChildItem Cert:\LocalMachine\My | Where-Object {{ $_.Subject -eq 'CN=RBridge Driver Signing' }} | Select-Object -First 1
if (-not $cert) {{
  $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject 'CN=RBridge Driver Signing' -CertStoreLocation 'Cert:\LocalMachine\My' -NotAfter (Get-Date).AddYears(10)
}}
if (-not (Get-ChildItem Cert:\LocalMachine\TrustedPublisher | Where-Object {{ $_.Thumbprint -eq $cert.Thumbprint }})) {{
  $cerPath = Join-Path $env:TEMP 'rbridge_sign.cer'
  Export-Certificate -Cert $cert -FilePath $cerPath -Force | Out-Null
  Import-Certificate -FilePath $cerPath -CertStoreLocation 'Cert:\LocalMachine\Root' | Out-Null
  Import-Certificate -FilePath $cerPath -CertStoreLocation 'Cert:\LocalMachine\TrustedPublisher' | Out-Null
}}
New-FileCatalog -Path '{dir}' -CatalogFilePath '{tmp_cat}' -CatalogVersion 2.0 | Out-Null
if (-not (Test-Path -LiteralPath '{tmp_cat}' -PathType Leaf)) {{
  throw "New-FileCatalog did not emit a catalog file at {tmp_cat}"
}}
$sig = Set-AuthenticodeSignature -FilePath '{tmp_cat}' -Certificate $cert -HashAlgorithm SHA256
if ($sig.Status -ne 'Valid') {{ throw "Catalog signature status: $($sig.Status)" }}"#,
        dir = dir_s,
        tmp_cat = tmp_cat_s
    );

    let out = Command::new("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &ps])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("Failed executing PowerShell signing: {}", e))?;

    if !out.status.success() {
        return Err(format!(
            "Catalog signing failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    for cat in cat_names {
        let dst = dir.join(cat);
        fs::copy(&tmp_cat, &dst)
            .map_err(|e| format!("Failed placing signed catalog {}: {}", cat, e))?;
        log(format!("[SIGN] Rebuilt & SHA-256 signed declared catalog '{}'", cat));
    }

    let _ = fs::remove_dir_all(&tmp_dir);
    Ok(())
}

fn read_setupapi_errors() -> Vec<String> {
    let Ok(bytes) = fs::read(r"C:\Windows\INF\setupapi.dev.log") else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(120);
    lines[start..]
        .iter()
        .filter(|l| l.contains("!!!") || l.contains("Error 0x"))
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|s| format!("  [SetupAPI] {}", s.trim()))
        .collect()
}

pub fn run_patch_pipeline(
    pkg: &DetectedPackage,
    opts: &PatchOptions,
    selected_inf_idx: Option<usize>,
    log: impl Fn(String),
) -> Result<String, String> {
    if pkg.kind == DriverPackageKind::Unknown || pkg.inf_paths.is_empty() {
        return Err("Select a valid AMD 21.Q2.1 or Amernime/R.ID extracted driver folder first.".into());
    }

    let raw_infs: Vec<PathBuf> = match (pkg.kind.clone(), selected_inf_idx) {
        (DriverPackageKind::AmernimeRid, Some(idx)) => {
            let pick = idx.min(pkg.inf_paths.len() - 1);
            vec![pkg.inf_paths[pick].clone()]
        }
        _ => pkg.inf_paths.clone(),
    };

    log(format!(
        "[INFO] Starting RBridge v2.1 pipeline for: {} ({} target INF)",
        pkg.label,
        raw_infs.len()
    ));

    heal_and_clean_package_tree(&pkg.root_dir, &log);

    if opts.strip_amd_bloat {
        let count = neutralize_ccc2_dirs(&pkg.root_dir, &log);
        if count > 0 {
            log(format!("[DEBLOAT] Neutralized {} Adrenalin UI folder(s)", count));
        }
    }

    let mut staged_infs: Vec<PathBuf> = Vec::with_capacity(raw_infs.len());
    for inf in &raw_infs {
        let staged = stage_inf_into_payload_dir(inf, &pkg.root_dir, &log)?;
        staged_infs.push(staged);
    }

    let mut dir_catalogs: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for inf in &staged_infs {
        patch_single_inf(inf, &pkg.kind, opts, &log)?;
        if let Some(parent) = inf.parent() {
            let entry = dir_catalogs.entry(parent.to_path_buf()).or_default();
            for cat in extract_declared_catalogs(inf) {
                if !entry.iter().any(|c| c.eq_ignore_ascii_case(&cat)) {
                    entry.push(cat);
                }
            }
        }
    }

    for (dir, cats) in &dir_catalogs {
        log(format!(
            "[CATALOG] Hashing payload & signing {} catalog(s) in {} (may take 10-20s)...",
            cats.len(),
            dir.display()
        ));
        rebuild_and_sign_dir_catalogs(dir, cats, &log)?;
    }

    let mut installed_ok = 0usize;
    if opts.install_driver {
        for inf in &staged_infs {
            log(format!(
                "[INSTALL] pnputil /add-driver {} /install",
                inf.display()
            ));
            let out = Command::new("pnputil")
                .args(["/add-driver", &inf.to_string_lossy(), "/install"])
                .creation_flags(CREATE_NO_WINDOW)
                .output()
                .map_err(|e| format!("Failed running pnputil: {}", e))?;

            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
                log(format!("  {}", line.trim()));
            }
            if out.status.success() {
                installed_ok += 1;
            } else {
                let stderr = String::from_utf8_lossy(&out.stderr);
                if !stderr.trim().is_empty() {
                    log(format!("[WARN] pnputil stderr: {}", stderr.trim()));
                }
                for sa_line in read_setupapi_errors() {
                    log(sa_line);
                }
            }
        }
    }

    if opts.strip_amd_bloat {
        let rep = debloat::strip_live_bloat();
        log(format!(
            "[DEBLOAT] Live cleanup: {} process(es) killed, {} service(s) & {} task(s) disabled",
            rep.killed, rep.services_disabled, rep.tasks_disabled
        ));
    }

    if opts.install_gui {
        match tweaks::install_rbridge_gui() {
            Ok(msg) => log(format!("[GUI] {}", msg)),
            Err(e) => log(format!("[WARN] GUI install skipped: {}", e)),
        }
    } else {
        log("[INFO] Driver-Only mode selected — RBridge GUI not installed.".into());
    }

    if opts.install_boot_guard {
        match tweaks::install_boot_guard() {
            Ok(msg) => log(format!("[DAEMON] {}", msg)),
            Err(e) => log(format!("[WARN] Boot Guard warning: {}", e)),
        }
    }

    let cur_tweaks = tweaks::read_tweak_state();
    for line in tweaks::apply_tweaks(&cur_tweaks) {
        log(line);
    }

    if opts.install_driver && installed_ok == 0 {
        Err("Driver installation did not complete cleanly — check [SetupAPI] / pnputil lines above.".into())
    } else {
        Ok("Pipeline completed! Reboot Windows to finalize driver & OverDrive settings.".into())
    }
}
