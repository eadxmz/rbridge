use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

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
            if name.contains("ccc2") || name == ".git" {
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

pub fn detect_driver_package(dir: &Path) -> DetectedPackage {
    let mut all_infs = Vec::new();
    collect_infs(dir, 6, &mut all_infs);

    let mut pro_infs = Vec::new();
    let mut rid_infs = Vec::new();

    for inf in &all_infs {
        let fname = inf
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        if fname == "u0366524.inf" || fname == "u0369543.inf" {
            pro_infs.push(inf.clone());
            continue;
        }
        if let Ok(text) = fs::read_to_string(inf) {
            let upper = text.to_ascii_uppercase();
            let is_amd_display = upper.contains("VEN_1002")
                && (upper.contains("AMDKMDAG") || upper.contains("ATI2MTAG"));
            if is_amd_display {
                rid_infs.push(inf.clone());
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

    if !rid_infs.is_empty() {
        return DetectedPackage {
            kind: DriverPackageKind::AmernimeRid,
            label: format!(
                "Amernime Zone / R.ID Hybrid Driver ({} INF file(s) detected)",
                rid_infs.len()
            ),
            inf_paths: rid_infs,
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

fn should_strip_inf_line(line_lower: &str) -> bool {
    const TOKENS: &[&str] = &[
        "amdrsserv",
        "radeonsoftware",
        "atieclxx",
        "atiesrxx",
        "amdrssrcext",
        "amdow.exe",
        "amddvr.exe",
        "cncmd.exe",
        "amdfendr",
        "auepmaster",
        "aueplauncher",
        "amduserexperience",
        "amdcrashdefender",
    ];
    let trimmed = line_lower.trim_start();
    if trimmed.starts_with(';') || trimmed.starts_with('[') {
        return false;
    }
    TOKENS.iter().any(|tok| trimmed.contains(tok))
}

fn neutralize_standalone_bloat_dirs(root: &Path, log: &impl Fn(String)) -> usize {
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
            if name.contains("ccc2_install") || name == "amdfendr" {
                let renamed = p.with_extension("rbridge_stripped");
                if fs::rename(&p, &renamed).is_ok() {
                    stripped += 1;
                    log(format!("[DEBLOAT] Neutralized standalone bloat folder '{}'", name));
                }
            } else {
                stack.push(p);
            }
        }
    }
    stripped
}

fn heal_and_clean_driver_dir(dir: &Path, log: &impl Fn(String)) {
    let backup_dir = env::temp_dir().join("RBridge_INF_Backups");
    let _ = fs::create_dir_all(&backup_dir);

    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let Ok(entries) = fs::read_dir(&cur) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let name = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let lower = name.to_ascii_lowercase();

            // Move any old in-place .rbridge_bak backups out so they never pollute the catalog
            if lower.ends_with(".rbridge_bak") {
                let dst = backup_dir.join(&name);
                if fs::copy(&p, &dst).is_ok() {
                    let _ = fs::remove_file(&p);
                }
            } else if lower.ends_with(".exe.rbridge_stripped") {
                // Restore binaries renamed by older v2.1 builds so pnputil CopyFiles never misses files
                let restored_name = &name[..name.len() - ".rbridge_stripped".len()];
                let dst = cur.join(restored_name);
                if fs::rename(&p, &dst).is_ok() {
                    log(format!("[CLEAN] Restored '{}' for clean catalog hashing", restored_name));
                }
            }
        }
    }
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

fn patch_single_inf(
    inf_path: &Path,
    kind: &DriverPackageKind,
    opts: &PatchOptions,
    log: &impl Fn(String),
) -> Result<(), String> {
    let backup_dir = env::temp_dir().join("RBridge_INF_Backups");
    let _ = fs::create_dir_all(&backup_dir);
    let bak_name = format!(
        "{}.orig",
        inf_path.file_name().unwrap_or_default().to_string_lossy()
    );
    let bak = backup_dir.join(bak_name);
    if !bak.exists() {
        let _ = fs::copy(inf_path, &bak);
    }

    let raw = fs::read_to_string(inf_path)
        .map_err(|e| format!("Failed reading {}: {}", inf_path.display(), e))?;

    let mut out_lines: Vec<String> = Vec::new();
    let mut stripped_lines = 0usize;
    let mut hook_count = 0usize;
    let mut in_rbridge_section = false;

    for line in raw.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();

        if lower == "[rbridge_tweaks]" {
            in_rbridge_section = true;
            continue;
        }
        if in_rbridge_section {
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                in_rbridge_section = false;
            } else {
                continue;
            }
        }
        if lower == "addreg = rbridge_tweaks" {
            continue;
        }

        if opts.strip_amd_bloat && should_strip_inf_line(&lower) {
            out_lines.push(format!("; [RBridge Stripped] {}", line));
            stripped_lines += 1;
            continue;
        }

        let mut modified = line.to_string();
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
        if is_hook_anchor {
            out_lines.push("AddReg = RBridge_Tweaks".into());
            hook_count += 1;
        }
    }

    if hook_count == 0 {
        let mut fallback_lines = Vec::with_capacity(out_lines.len() + 16);
        for line in out_lines {
            let lower = line.trim().to_ascii_lowercase();
            let inject_after = lower.starts_with("addreg")
                && (lower.contains("ati2mtag") || lower.contains("kmd_reg"));
            fallback_lines.push(line);
            if inject_after && hook_count < 4 {
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

    log(format!(
        "[PATCH] {} -> {} hook(s), {} bloat line(s) stripped",
        inf_path.file_name().unwrap_or_default().to_string_lossy(),
        hook_count,
        stripped_lines
    ));
    Ok(())
}

fn rebuild_and_sign_dir_catalogs(
    dir: &Path,
    cat_names: &[String],
    log: &impl Fn(String),
) -> Result<(), String> {
    // Delete all stale target catalogs first so New-FileCatalog never hashes old .cat files
    for cat in cat_names {
        let p = dir.join(cat);
        if p.exists() {
            let _ = fs::remove_file(&p);
        }
    }

    let tmp_dir = env::temp_dir().join("RBridge_Signed_Cats");
    let _ = fs::create_dir_all(&tmp_dir);
    let dir_s = dir.to_string_lossy().replace('\'', "''");

    let mut staged: Vec<(PathBuf, PathBuf, String)> = Vec::new();

    for cat in cat_names {
        let tmp_cat = tmp_dir.join(cat);
        if tmp_cat.exists() {
            let _ = fs::remove_file(&tmp_cat);
        }
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
                "Catalog signing failed for {}: {}",
                cat,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }

        staged.push((tmp_cat, dir.join(cat), cat.clone()));
    }

    // Move all freshly signed catalogs into the driver directory together
    for (src, dst, name) in staged {
        fs::copy(&src, &dst)
            .map_err(|e| format!("Failed placing signed catalog {}: {}", name, e))?;
        let _ = fs::remove_file(&src);
        log(format!("[SIGN] Rebuilt & SHA-256 signed declared catalog '{}'", name));
    }

    Ok(())
}

pub fn run_patch_pipeline(
    pkg: &DetectedPackage,
    opts: &PatchOptions,
    log: impl Fn(String),
) -> Result<String, String> {
    if pkg.kind == DriverPackageKind::Unknown || pkg.inf_paths.is_empty() {
        return Err("Select a valid AMD 21.Q2.1 or Amernime/R.ID extracted driver folder first.".into());
    }

    log(format!("[INFO] Starting RBridge v2.1 pipeline for: {}", pkg.label));

    if opts.strip_amd_bloat {
        let count = neutralize_standalone_bloat_dirs(&pkg.root_dir, &log);
        if count > 0 {
            log(format!("[DEBLOAT] Neutralized {} standalone bloat folder(s)", count));
        }
    }

    // Clean any old .rbridge_bak or v2.1 .exe.rbridge_stripped files before patching & hashing
    let unique_dirs: BTreeSet<PathBuf> = pkg
        .inf_paths
        .iter()
        .filter_map(|p| p.parent().map(|d| d.to_path_buf()))
        .collect();
    for dir in &unique_dirs {
        heal_and_clean_driver_dir(dir, &log);
    }

    // Step 1: Patch ALL INFs BEFORE generating any catalogs
    let mut dir_catalogs: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for inf in &pkg.inf_paths {
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

    // Step 2: Rebuild & sign each unique declared CatalogFile per directory
    for (dir, cats) in &dir_catalogs {
        log(format!(
            "[CATALOG] Building {} declared catalog(s) in {}",
            cats.len(),
            dir.display()
        ));
        rebuild_and_sign_dir_catalogs(dir, cats, &log)?;
    }

    if pkg.kind == DriverPackageKind::AmernimeRid {
        log("[NOTE] Amernime/R.ID modifies kernel binaries; if Windows rejects signature trust (Code 52), enable Test Mode (bcdedit /set testsigning on) or install R.ID's cert.".into());
    }

    // Step 3: Install via pnputil
    if opts.install_driver {
        for inf in &pkg.inf_paths {
            log(format!(
                "[INSTALL] pnputil /add-driver {} /install",
                inf.file_name().unwrap_or_default().to_string_lossy()
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
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                if !stderr.trim().is_empty() {
                    log(format!("[WARN] pnputil stderr: {}", stderr.trim()));
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

    Ok("Pipeline completed! Reboot Windows to finalize driver & MSI settings.".into())
}
