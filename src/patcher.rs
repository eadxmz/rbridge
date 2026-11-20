use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const TARGET_INFS: [&str; 2] = ["u0366524.inf", "u0369543.inf"];
const SECTION_NAME: &str = "ati2mtag_Optimus_MasterTweaks";

const MASTER_TWEAKS_BLOCK: &str = r#"[ati2mtag_Optimus_MasterTweaks]
; === Stability & BSOD Prevention (No D3cold Split-Brain) ===
HKR,, EnableUlps, %REG_DWORD%, 0
HKR,, EnableUlps_NA, %REG_SZ%, "0"
HKR,, PP_GPUPowerDownEnabled, %REG_DWORD%, 0
HKR,, BacoEnable, %REG_DWORD%, 0
HKR,, EnableCrossFireAutoLink, %REG_DWORD%, 0
HKR,, StutterMode, %REG_DWORD%, 0
HKR,, EnableAspmL0s, %REG_DWORD%, 0
HKR,, EnableAspmL1, %REG_DWORD%, 0
HKR,, EnablePcieGen3, %REG_DWORD%, 1

; === Enduro Copy-Back, DMA & Anti-Stutter Unlocks ===
HKR,, DisableDMACopy, %REG_DWORD%, 0
HKR,, DisableBlockWrite, %REG_DWORD%, 0
HKR,, DisableEarlyZ, %REG_DWORD%, 0
HKR,, DisableFBC, %REG_DWORD%, 1
HKR,, DisableDrmdmaPowerGating, %REG_DWORD%, 1
HKR,, KMD_EnablePreemption, %REG_DWORD%, 0
HKR,, KMD_EnableComputeQueue, %REG_DWORD%, 1
HKR,, KMD_EnableInternalLargePage, %REG_DWORD%, 0
HKR,, KMD_DeLagEnabled, %REG_DWORD%, 0
HKR,, KMD_FRTEnabled, %REG_DWORD%, 0

; === Native OverDrive & PowerPlay Unlocks (Smooth Thermal Stepping ON) ===
HKR,, PP_ODNFeatureEnable, %REG_DWORD%, 1
HKR,, PP_DisablePowerContainment, %REG_DWORD%, 1
HKR,, PP_DisableClockStretching, %REG_DWORD%, 1
HKR,, PP_DisableULV, %REG_DWORD%, 1
HKR,, PP_SclkDeepSleepDisable, %REG_DWORD%, 1
HKR,, PP_MclkDeepSleepDisable, %REG_DWORD%, 1
HKR,, PP_ThermalAutoThrottlingEnable, %REG_DWORD%, 1
HKR,, PP_PhmUseDummyBackEnd, %REG_DWORD%, 0

; === DAL Custom Modes (Gamma/Color LUT Untouched for Intel iGPU) ===
HKR,, DALRULE_AllowNonCEModes, %REG_DWORD%, 1

; === Unlocked OpenGL Multi-Threading & HyperZ ===
HKR,, OGL_VBO_Alloc_Policy, %REG_DWORD%, 1
HKR,, OGL_EnableTripleBuffer, %REG_DWORD%, 0
HKR,, OGL_ThreadControl, %REG_DWORD%, 1
HKR,, OGL_EnableFastZClear, %REG_DWORD%, 1

; === Unlocked UMD Gaming Profile (Catalyst AI + 2-Frame Enduro Queue + 4x Tess) ===
HKR, UMD, CatalystAI, %REG_BINARY%, 31,00
HKR, UMD, CatalystAI_DEF, %REG_SZ%, "1"
HKR, UMD, FlipQueueSize, %REG_BINARY%, 32,00
HKR, UMD, FlipQueueSize_SET, %REG_BINARY%, 32,00
HKR, UMD, FlipQueueSize_DEF, %REG_SZ%, "2"
HKR, UMD, Main3D, %REG_BINARY%, 32,00
HKR, UMD, Main3D_SET, %REG_BINARY%, 32,00
HKR, UMD, Main3D_DEF, %REG_SZ%, "2"
HKR, UMD, ShaderCache, %REG_BINARY%, 32,00
HKR, UMD, ShaderCache_DEF, %REG_SZ%, "2"
HKR, UMD, SurfaceFormatReplacements, %REG_BINARY%, 31,00
HKR, UMD, SurfaceFormatReplacements_DEF, %REG_SZ%, "1"
HKR, UMD, SurfaceFormatReplacements_NA, %REG_SZ%, "0"
HKR, UMD, TextureOpt, %REG_BINARY%, 31,00
HKR, UMD, TextureOpt_DEF, %REG_SZ%, "1"
HKR, UMD, TFQ, %REG_BINARY%, 32,00
HKR, UMD, TFQ_DEF, %REG_SZ%, "2"
HKR, UMD, AnisoType, %REG_BINARY%, 30,00
HKR, UMD, AnisoDegree, %REG_BINARY%, 30,00
HKR, UMD, VSyncControl, %REG_BINARY%, 30,00
HKR, UMD, VSyncControl_DEF, %REG_SZ%, "0"
HKR, UMD, Tessellation_OPTION, %REG_BINARY%, 32,00
HKR, UMD, Tessellation_OPTION_DEF, %REG_SZ%, "2"
HKR, UMD, Tessellation, %REG_BINARY%, 34,00
HKR, UMD, Tessellation_DEF, %REG_SZ%, "4"
"#;

pub fn find_target_infs(root: &Path) -> Vec<PathBuf> {
    WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|n| TARGET_INFS.contains(&n.to_ascii_lowercase().as_str()))
                .unwrap_or(false)
        })
        .map(|e| e.into_path())
        .collect()
}

fn read_inf(path: &Path) -> io::Result<(String, bool)> {
    let bytes = fs::read(path)?;
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let u16s: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        Ok((String::from_utf16_lossy(&u16s), true))
    } else {
        Ok((String::from_utf8_lossy(&bytes).into_owned(), false))
    }
}

fn write_inf(path: &Path, text: &str, utf16le: bool) -> io::Result<()> {
    let normalized = text.replace("\r\n", "\n").replace('\n', "\r\n");
    if utf16le {
        let mut out = vec![0xFFu8, 0xFEu8];
        for unit in normalized.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        fs::write(path, out)
    } else {
        fs::write(path, normalized.as_bytes())
    }
}

pub fn patch_inf_in_place(inf_path: &Path, enable_msi: bool) -> io::Result<()> {
    let backup = inf_path.with_extension("rbridge_bak");
    if !backup.exists() {
        fs::copy(inf_path, &backup)?;
    }

    let (raw, is_utf16) = read_inf(&backup)?;
    let mut output = Vec::new();
    let mut in_strings_section = false;
    let mut injected_count = 0usize;
    let mut stripped_px_count = 0usize;

    for line in raw.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_strings_section = lower == "[strings]";
        }

        // 1. Prevent 0x139 D3cold Split-Brain BSOD
        if lower.contains("addreg") && lower.contains("ati2mtag_px_dgpu_only_2id") {
            output.push(format!("; [RBridge stripped BSOD conflict] {}", line));
            stripped_px_count += 1;
            continue;
        }

        // 2. Clean Branding inside [Strings] ONLY (preserves true Oland ASIC mapping)
        if in_strings_section {
            if lower.starts_with("amd6660.1") {
                output.push(r#"AMD6660.1 = "AMD Radeon R8 M670""#.to_string());
                continue;
            }
            if lower.starts_with("amd6600.1") {
                output.push(r#"AMD6600.1 = "AMD Radeon R8 M670""#.to_string());
                continue;
            }
        }

        // 3. Optional MSI toggle
        if !enable_msi && lower.contains("msisupported") && lower.contains("0x00000001") {
            output.push(line.replace("0x00000001", "0x00000000"));
            continue;
        }

        output.push(line.to_string());

        // 4. Inject MasterTweaks strictly AFTER DelReg so Windows PnP never wipes them
        if lower.contains("delreg") && lower.contains("ati2mtag_removedevicesettings") {
            output.push(format!("AddReg = {}", SECTION_NAME));
            injected_count += 1;
        }
    }

    output.push(String::new());
    output.push(MASTER_TWEAKS_BLOCK.to_string());

    write_inf(inf_path, &output.join("\n"), is_utf16)?;
    println!(
        "[OK] Patched {} (stripped {} PX conflicts, injected {} post-DelReg hooks)",
        inf_path.display(),
        stripped_px_count,
        injected_count
    );
    Ok(())
}
