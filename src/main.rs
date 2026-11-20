use rbridge::{oc, patcher, res_injector, signer, vram_guard};
use std::collections::HashSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

fn prompt(msg: &str) -> String {
    print!("{}", msg);
    let _ = io::stdout().flush();
    let mut input = String::new();
    let _ = io::stdin().read_line(&mut input);
    input.trim().trim_matches('"').to_string()
}

fn locate_infs() -> Option<Vec<PathBuf>> {
    let dir_str = prompt("Path to extracted AMD 21.Q2.1 folder (e.g. C:\\AMD\\21.Q2.1): ");
    let root = Path::new(&dir_str);
    let infs = patcher::find_target_infs(root);
    if infs.is_empty() {
        println!(
            "[ERR] Could not find U0366524.INF or U0369543.inf under {}",
            root.display()
        );
        None
    } else {
        Some(infs)
    }
}

fn run_patch(infs: &[PathBuf]) -> bool {
    let msi_ans = prompt("Keep MSISupported=1? [Y/n]: ");
    let enable_msi = !msi_ans.eq_ignore_ascii_case("n");
    let mut ok = true;
    for inf in infs {
        if let Err(e) = patcher::patch_inf_in_place(inf, enable_msi) {
            println!("[ERR] Patch failed on {}: {}", inf.display(), e);
            ok = false;
        }
    }
    ok
}

fn run_sign(infs: &[PathBuf]) -> bool {
    let thumb = match signer::ensure_local_cert() {
        Ok(t) => t,
        Err(e) => {
            println!("[ERR] Certificate setup failed: {}", e);
            return false;
        }
    };

    let dirs: HashSet<&Path> = infs.iter().filter_map(|p| p.parent()).collect();
    let mut ok = true;
    for d in dirs {
        if let Err(e) = signer::rebuild_and_sign_catalogs(d, &thumb) {
            println!("[ERR] Signing failed in {}: {}", d.display(), e);
            ok = false;
        }
    }
    ok
}

fn install_driver_only(infs: &[PathBuf]) -> io::Result<()> {
    for inf in infs {
        println!(
            "[..] Installing {} via pnputil (driver-only, no bloat)...",
            inf.display()
        );
        let status = Command::new("pnputil")
            .args(["/add-driver", &inf.to_string_lossy(), "/install"])
            .status()?;
        if !status.success() {
            println!(
                "[WARN] pnputil returned non-zero for {} (check if reboot needed).",
                inf.display()
            );
        }
    }
    Ok(())
}

fn register_boot_guard() -> io::Result<()> {
    let exe_dir = std::env::current_exe()?
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Missing exe directory"))?
        .to_path_buf();
    let daemon = exe_dir.join("rbridge-daemon.exe");
    if !daemon.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "Place rbridge-daemon.exe next to rbridge.exe ({})",
                daemon.display()
            ),
        ));
    }

    let status = Command::new("schtasks")
        .args([
            "/Create",
            "/TN",
            "RBridge_Oland_Guard",
            "/SC",
            "ONSTART",
            "/RU",
            "SYSTEM",
            "/RL",
            "HIGHEST",
            "/F",
            "/TR",
            &format!("\"{}\"", daemon.display()),
        ])
        .status()?;

    if status.success() {
        println!("[OK] Windowless boot guard registered (Task: RBridge_Oland_Guard).");
        let _ = Command::new(&daemon).spawn();
        match vram_guard::apply_to_current_user() {
            Ok(n) => println!(
                "[OK] Applied Flip Model + VRAM quarantine ({} background apps pinned to Intel).",
                n
            ),
            Err(e) => println!("[WARN] VRAM quarantine warning: {}", e),
        }
        println!("[OK] Applied live KMD/UMD/Vulkan/MSI/AC-power optimizations.");
    } else {
        println!("[ERR] Failed to register Scheduled Task.");
    }
    Ok(())
}

fn menu_overclock() {
    println!("\n--- Zero-RAM Native Overclock (ADL + SoftPowerPlayTable) ---");
    let saved = oc::read_saved_oc();
    println!(
        "Boot Auto-OC: {} (Saved Core: {} MHz, Saved Mem: {} MHz)",
        if saved.enabled { "ENABLED" } else { "OFF" },
        saved.engine_mhz,
        saved.memory_mhz
    );

    match oc::query_oc_status() {
        Ok(st) => {
            println!("Adapter:      {} ({})", st.adapter_name, st.backend);
            println!(
                "Core Clock:   {} MHz (Default: {} MHz | ADL Range: {}-{} MHz)",
                st.engine_mhz, st.default_engine_mhz, st.min_engine_mhz, st.max_engine_mhz
            );
            println!(
                "Memory Clock: {} MHz (Default: {} MHz | ADL Range: {}-{} MHz)",
                st.memory_mhz, st.default_memory_mhz, st.min_memory_mhz, st.max_memory_mhz
            );
            println!("States:       {}", st.levels_summary);
            println!("\nTip: Step Memory Clock +25 MHz first (e.g. 900 -> 950 -> 1000 MHz).");
            println!("  a) Apply & Save Overclock (auto-falls back to SoftPowerPlayTable if VBIOS locked)");
            println!("  r) Reset to Factory Defaults & Restore SoftPowerPlayTable");
            println!("  b) Back");

            match prompt("Choice: ").to_ascii_lowercase().as_str() {
                "a" => {
                    let e_in = prompt(&format!(
                        "Target Core MHz [Enter to keep {}]: ",
                        st.engine_mhz
                    ));
                    let m_in = prompt(&format!(
                        "Target Memory MHz [Enter to keep {}]: ",
                        st.memory_mhz
                    ));
                    let target_e = e_in.parse::<u32>().unwrap_or(st.engine_mhz);
                    let target_m = m_in.parse::<u32>().unwrap_or(st.memory_mhz);

                    match oc::apply_oc_clocks(target_e, target_m) {
                        Ok((applied_e, applied_m, api)) => {
                            println!(
                                "[OK] Applied live via {}: Core = {} MHz, Memory = {} MHz.",
                                api, applied_e, applied_m
                            );
                            if let Err(e) = oc::save_oc_profile(true, applied_e, applied_m) {
                                println!("[WARN] Could not save boot OC profile: {}", e);
                            } else {
                                println!("[OK] Saved to HKLM\\SOFTWARE\\RBridge.");
                            }
                        }
                        Err(adl_err) => {
                            println!("[INFO] {} — trying SoftPowerPlayTable DPM mod...", adl_err);
                            match oc::apply_soft_powerplay_mod(st.engine_mhz, st.memory_mhz, target_e, target_m) {
                                Ok(hits) => {
                                    let _ = oc::save_oc_profile(false, target_e, target_m);
                                    println!(
                                        "[OK] Patched {} DPM table entries in PP_PhmSoftPowerPlayTable ({}/{} MHz).",
                                        hits, target_e, target_m
                                    );
                                    println!("[INFO] Reboot (or disable/enable AMD GPU in Device Manager) to activate new DPM clocks.");
                                }
                                Err(pp_err) => println!("[ERR] SoftPowerPlayTable mod failed: {}", pp_err),
                            }
                        }
                    }
                }
                "r" => {
                    let _ = oc::apply_oc_clocks(st.default_engine_mhz, st.default_memory_mhz);
                    let _ = oc::save_oc_profile(false, 0, 0);
                    if oc::restore_soft_powerplay_table() {
                        println!("[OK] Restored stock PP_PhmSoftPowerPlayTable backup. Reboot to apply.");
                    } else {
                        println!("[OK] Disabled boot auto-OC.");
                    }
                }
                _ => {}
            }
        }
        Err(e) => println!("[ERR] {}", e),
    }
}

fn menu_custom_resolutions() {
    println!("\n--- 16:9 Custom Resolution Injector (Exclusive Fullscreen) ---");
    let displays = res_injector::list_displays();
    if displays.is_empty() {
        println!("[ERR] No EDID displays found in registry.");
        return;
    }

    for (i, d) in displays.iter().enumerate() {
        println!(
            "  {}) {} ({}x{} @ {}Hz){}{}",
            i + 1,
            d.model,
            d.native_w,
            d.native_h,
            d.refresh_hz,
            if d.active { " [ACTIVE]" } else { "" },
            if d.has_override { " [CUSTOM RES INSTALLED]" } else { "" }
        );
    }

    let sel = prompt("\nSelect display number (0 to cancel): ");
    let Ok(num) = sel.parse::<usize>() else {
        return;
    };
    if num == 0 || num > displays.len() {
        return;
    }
    let target = &displays[num - 1];

    println!("  a) Inject lower 16:9 resolutions (EDID_OVERRIDE)");
    println!("  r) Remove EDID_OVERRIDE (restore factory EDID)");
    match prompt("Choice: ").to_ascii_lowercase().as_str() {
        "a" => match res_injector::inject_16_9_modes(target) {
            Ok(modes) => {
                let list: Vec<String> = modes
                    .iter()
                    .map(|(w, h)| format!("{}x{}", w, h))
                    .collect();
                println!("[OK] Injected 16:9 modes for {}: {}", target.model, list.join(", "));
                println!("[INFO] Reboot once to activate. In Intel Graphics Control Panel, set Scaling -> Scale Full Screen.");
            }
            Err(e) => println!("[ERR] Injection failed: {}", e),
        },
        "r" => match res_injector::remove_override(target) {
            Ok(()) => println!("[OK] Removed EDID_OVERRIDE for {}. Reboot to restore factory modes.", target.model),
            Err(e) => println!("[ERR] Failed to remove override: {}", e),
        },
        _ => {}
    }
}

fn menu_vram_quarantine() {
    println!("\n--- 2 GB VRAM Quarantine & DX10/DX11 Flip Model ---");
    println!("  a) Auto-Quarantine Browsers/Discord/Steam to Intel + Force Flip Model");
    println!("  g) Pin a Game .exe to AMD Radeon R8 M670 (High Performance)");
    println!("  b) Back");

    match prompt("Choice: ").to_ascii_lowercase().as_str() {
        "a" => match vram_guard::apply_to_current_user() {
            Ok(n) => println!(
                "[OK] Forced Flip Model (SwapEffectUpgradeEnable=1) and pinned {} background apps to Intel iGPU.",
                n
            ),
            Err(e) => println!("[ERR] {}", e),
        },
        "g" => {
            let p = prompt("Full path to game .exe (you can drag-and-drop it here): ");
            if p.is_empty() {
                return;
            }
            match vram_guard::pin_game_to_amd(Path::new(&p)) {
                Ok(()) => println!("[OK] Pinned {} to AMD High Performance (GpuPreference=2;).", p),
                Err(e) => println!("[ERR] {}", e),
            }
        }
        _ => {}
    }
}

fn main() {
    println!("====================================================");
    println!("  RBridge v1.4 - AMD Radeon HD 8670M (21.Q2.1)");
    println!("  Peak GCN 1.0 Patcher, Signer, OC & Res Suite");
    println!("====================================================");

    loop {
        println!("\n1) Full Pipeline: Patch INFs + Sign + Install + Boot Guard");
        println!("2) Patch Dual INFs Only (U0366524.INF & U0369543.inf)");
        println!("3) Rebuild & Sign Catalogs Only (Zero-WDK)");
        println!("4) Install Windowless Boot Guard & Apply All Live Tweaks");
        println!("5) Native Overclock (Zero-RAM ADL via atiadlxx.dll)");
        println!("6) 16:9 Custom Resolution Injector (Exclusive Fullscreen)");
        println!("7) VRAM Quarantine & Flip Model (Pin Apps to Intel / Games to AMD)");
        println!("8) Remove Boot Guard Scheduled Task");
        println!("0) Exit");

        match prompt("\nSelect option: ").as_str() {
            "1" => {
                if let Some(infs) = locate_infs() {
                    if run_patch(&infs) && run_sign(&infs) {
                        if let Err(e) = install_driver_only(&infs) {
                            println!("[ERR] Install failed: {}", e);
                        } else {
                            let _ = register_boot_guard();
                            println!("[DONE] Reboot once to finalize driver + boot guard.");
                        }
                    }
                }
            }
            "2" => {
                if let Some(infs) = locate_infs() {
                    run_patch(&infs);
                }
            }
            "3" => {
                if let Some(infs) = locate_infs() {
                    run_sign(&infs);
                }
            }
            "4" => {
                if let Err(e) = register_boot_guard() {
                    println!("[ERR] {}", e);
                }
            }
            "5" => menu_overclock(),
            "6" => menu_custom_resolutions(),
            "7" => menu_vram_quarantine(),
            "8" => {
                let _ = Command::new("schtasks")
                    .args(["/Delete", "/TN", "RBridge_Oland_Guard", "/F"])
                    .status();
                println!("[OK] Removed boot guard task.");
            }
            "0" => break,
            _ => println!("Invalid selection."),
        }
    }
}
