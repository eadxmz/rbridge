mod patcher;
mod signer;

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
        println!("[OK] Applied live registry optimizations.");
    } else {
        println!("[ERR] Failed to register Scheduled Task.");
    }
    Ok(())
}

fn main() {
    println!("====================================================");
    println!("  RBridge v1.0 - AMD Radeon HD 8670M (21.Q2.1)");
    println!("  Peak GCN 1.0 Oland Patcher, Signer & Boot Guard");
    println!("====================================================");

    loop {
        println!("\n1) Full Pipeline: Patch INFs + Sign Catalogs + Driver-Only Install");
        println!("2) Patch Dual INFs Only (U0366524.INF & U0369543.inf)");
        println!("3) Rebuild & Sign Catalogs Only (No Test Mode)");
        println!("4) Install Windowless Boot Guard & Apply Live Tweaks Now");
        println!("5) Remove Boot Guard Scheduled Task");
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
            "5" => {
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
