use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use crate::debloat;
use crate::display::{self, MonitorEntry};
use crate::patcher::{self, DetectedPackage, DriverPackageKind, PatchOptions};
use crate::powerplay::{self, GpuTelemetry, ParsedPowerPlay};
use crate::quarantine::{self, GpuPinEntry};
use crate::tweaks::{self, TweakState};

// AMD Radeon PRO Enterprise Palette
const CANVAS_BG: egui::Color32 = egui::Color32::from_rgb(18, 21, 26); // #12151A
const RAIL_BG: egui::Color32 = egui::Color32::from_rgb(22, 26, 33); // #161A21
const CARD_BG: egui::Color32 = egui::Color32::from_rgb(27, 32, 40); // #1B2028
const CARD_BORDER: egui::Color32 = egui::Color32::from_rgb(42, 50, 61); // #2A323D
const PRO_BLUE: egui::Color32 = egui::Color32::from_rgb(0, 114, 206); // #0072CE
const PRO_BLUE_BRIGHT: egui::Color32 = egui::Color32::from_rgb(29, 140, 248); // #1D8CF8
const TEXT_PRIMARY: egui::Color32 = egui::Color32::from_rgb(232, 238, 245);
const TEXT_MUTED: egui::Color32 = egui::Color32::from_rgb(154, 165, 180);
const STATUS_OK: egui::Color32 = egui::Color32::from_rgb(46, 204, 113);
const STATUS_WARN: egui::Color32 = egui::Color32::from_rgb(241, 196, 15);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Dashboard,
    Patcher,
    Performance,
    Overclock,
    Display,
    Quarantine,
}

enum WorkerMsg {
    Log(String),
    Done(Result<String, String>),
}

struct RBridgeApp {
    tab: Tab,
    is_admin: bool,
    telemetry: GpuTelemetry,
    running_bloat: usize,
    tweaks: TweakState,

    // Driver Patcher state
    pkg_dir: String,
    detected_pkg: Option<DetectedPackage>,
    patch_opts: PatchOptions,
    patch_logs: Vec<String>,
    worker_rx: Option<Receiver<WorkerMsg>>,
    busy: bool,

    // PowerPlay Overclock state
    parsed_pp: Option<ParsedPowerPlay>,
    rom_path_label: String,
    stock_core_mhz: u32,
    stock_mem_mhz: u32,
    target_core_mhz: u32,
    target_mem_mhz: u32,

    // Display & Quarantine state
    monitors: Vec<MonitorEntry>,
    pins: Vec<GpuPinEntry>,

    // Footer status message
    status_line: String,
    status_is_error: bool,
}

impl RBridgeApp {
    fn new() -> Self {
        let telemetry = powerplay::query_gpu_telemetry();
        let parsed_pp = powerplay::load_cached_powerplay();
        let (stock_c, stock_m) = parsed_pp
            .as_ref()
            .map(|p| (p.boost_core_mhz, p.boost_mem_mhz))
            .unwrap_or((975, 900));
        let target_c = if telemetry.saved_oc_core > 0 {
            telemetry.saved_oc_core
        } else {
            stock_c
        };
        let target_m = if telemetry.saved_oc_mem > 0 {
            telemetry.saved_oc_mem
        } else {
            stock_m
        };
        let rom_label = if parsed_pp.is_some() {
            "Loaded cached stock PowerPlay table from registry".into()
        } else {
            "No VBIOS ROM loaded yet — export Oland.rom from GPU-Z".into()
        };

        Self {
            tab: Tab::Dashboard,
            is_admin: tweaks::is_elevated(),
            telemetry,
            running_bloat: debloat::count_running_bloat(),
            tweaks: tweaks::read_tweak_state(),
            pkg_dir: String::new(),
            detected_pkg: None,
            patch_opts: PatchOptions::default(),
            patch_logs: vec!["Select an extracted AMD 21.Q2.1 or Amernime / R.ID driver folder to begin.".into()],
            worker_rx: None,
            busy: false,
            parsed_pp,
            rom_path_label: rom_label,
            stock_core_mhz: stock_c,
            stock_mem_mhz: stock_m,
            target_core_mhz: target_c,
            target_mem_mhz: target_m,
            monitors: display::enumerate_monitors(),
            pins: quarantine::list_pinned_apps(),
            status_line: "RBridge v2.0 ready.".into(),
            status_is_error: false,
        }
    }

    fn set_status(&mut self, msg: impl Into<String>, is_err: bool) {
        self.status_line = msg.into();
        self.status_is_error = is_err;
    }

    fn refresh_all(&mut self) {
        self.telemetry = powerplay::query_gpu_telemetry();
        self.running_bloat = debloat::count_running_bloat();
        self.tweaks = tweaks::read_tweak_state();
        self.monitors = display::enumerate_monitors();
        self.pins = quarantine::list_pinned_apps();
    }

    fn poll_worker(&mut self, ctx: &egui::Context) {
        let mut finished = false;
        if let Some(rx) = &self.worker_rx {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    WorkerMsg::Log(line) => self.patch_logs.push(line),
                    WorkerMsg::Done(res) => {
                        finished = true;
                        match res {
                            Ok(msg) => {
                                self.patch_logs.push(format!("[DONE] {}", msg));
                                self.set_status(msg, false);
                            }
                            Err(err) => {
                                self.patch_logs.push(format!("[ERR] {}", err));
                                self.set_status(err, true);
                            }
                        }
                    }
                }
            }
        }
        if finished {
            self.worker_rx = None;
            self.busy = false;
            self.refresh_all();
        } else if self.busy {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn card_frame() -> egui::Frame {
        egui::Frame::none()
            .fill(CARD_BG)
            .stroke(egui::Stroke::new(1.0, CARD_BORDER))
            .rounding(egui::Rounding::same(6.0))
            .inner_margin(egui::Margin::same(14.0))
    }

    fn primary_btn(label: &str) -> egui::Button<'_> {
        egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE).strong())
            .fill(PRO_BLUE)
            .rounding(egui::Rounding::same(5.0))
            .min_size(egui::vec2(130.0, 32.0))
    }

    fn secondary_btn(label: &str) -> egui::Button<'_> {
        egui::Button::new(egui::RichText::new(label).color(TEXT_PRIMARY))
            .fill(egui::Color32::from_rgb(35, 42, 53))
            .stroke(egui::Stroke::new(1.0, CARD_BORDER))
            .rounding(egui::Rounding::same(5.0))
            .min_size(egui::vec2(120.0, 30.0))
    }

    fn render_top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top_bar")
            .exact_height(54.0)
            .frame(
                egui::Frame::none()
                    .fill(RAIL_BG)
                    .stroke(egui::Stroke::new(1.0, CARD_BORDER))
                    .inner_margin(egui::Margin::symmetric(16.0, 10.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(
                        egui::RichText::new("RBRIDGE")
                            .size(20.0)
                            .strong()
                            .color(TEXT_PRIMARY),
                    );
                    egui::Frame::none()
                        .fill(PRO_BLUE)
                        .rounding(egui::Rounding::same(4.0))
                        .inner_margin(egui::Margin::symmetric(7.0, 2.0))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new("PRO EDITION v2.0")
                                    .size(11.0)
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            );
                        });

                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new(&self.telemetry.gpu_name)
                            .size(13.0)
                            .color(TEXT_MUTED),
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.is_admin {
                            ui.label(
                                egui::RichText::new("ADMIN ELEVATED")
                                    .size(11.0)
                                    .strong()
                                    .color(STATUS_OK),
                            );
                        } else if ui.add(Self::primary_btn("Relaunch as Admin")).clicked() {
                            tweaks::relaunch_as_admin();
                        }
                        if ui.add(Self::secondary_btn("Refresh")).clicked() {
                            self.refresh_all();
                            self.set_status("Refreshed live hardware & registry telemetry.", false);
                        }
                    });
                });
            });
    }

    fn render_sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("nav_rail")
            .resizable(false)
            .exact_width(210.0)
            .frame(
                egui::Frame::none()
                    .fill(RAIL_BG)
                    .stroke(egui::Stroke::new(1.0, CARD_BORDER))
                    .inner_margin(egui::Margin::same(12.0)),
            )
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("CONTROL CENTER")
                        .size(11.0)
                        .strong()
                        .color(TEXT_MUTED),
                );
                ui.add_space(8.0);

                let items = [
                    (Tab::Dashboard, "Dashboard"),
                    (Tab::Patcher, "Driver Patcher"),
                    (Tab::Performance, "Performance"),
                    (Tab::Overclock, "Overclock (PowerPlay)"),
                    (Tab::Display, "Display 16:9"),
                    (Tab::Quarantine, "VRAM Quarantine"),
                ];

                for (tab, label) in items {
                    let active = self.tab == tab;
                    let fill = if active {
                        PRO_BLUE
                    } else {
                        egui::Color32::TRANSPARENT
                    };
                    let text_col = if active {
                        egui::Color32::WHITE
                    } else {
                        TEXT_PRIMARY
                    };
                    let btn = egui::Button::new(
                        egui::RichText::new(label).size(14.0).color(text_col).strong(),
                    )
                    .fill(fill)
                    .rounding(egui::Rounding::same(5.0))
                    .min_size(egui::vec2(186.0, 36.0));

                    if ui.add(btn).clicked() {
                        self.tab = tab;
                    }
                    ui.add_space(4.0);
                }

                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.label(
                        egui::RichText::new("0 MB Idle Footprint")
                            .size(11.0)
                            .color(TEXT_MUTED),
                    );
                    ui.label(
                        egui::RichText::new("Intel HD 4400 + AMD 8670M")
                            .size(11.0)
                            .color(PRO_BLUE_BRIGHT),
                    );
                });
            });
    }

    fn render_footer(&self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status_bar")
            .exact_height(30.0)
            .frame(
                egui::Frame::none()
                    .fill(RAIL_BG)
                    .stroke(egui::Stroke::new(1.0, CARD_BORDER))
                    .inner_margin(egui::Margin::symmetric(14.0, 6.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let color = if self.status_is_error {
                        egui::Color32::from_rgb(231, 76, 60)
                    } else {
                        TEXT_MUTED
                    };
                    ui.label(egui::RichText::new(&self.status_line).size(12.0).color(color));
                });
            });
    }

    fn render_dashboard(&mut self, ui: &mut egui::Ui) {
        ui.heading(egui::RichText::new("System Overview").size(22.0).color(TEXT_PRIMARY));
        ui.add_space(10.0);

        ui.columns(3, |cols| {
            Self::card_frame().show(&mut cols[0], |ui| {
                ui.label(egui::RichText::new("ACTIVE DRIVER STACK").size(11.0).color(TEXT_MUTED));
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(&self.telemetry.driver_stack)
                        .size(14.0)
                        .strong()
                        .color(PRO_BLUE_BRIGHT),
                );
                ui.label(
                    egui::RichText::new(format!("Version: {}", self.telemetry.driver_version))
                        .size(12.0)
                        .color(TEXT_MUTED),
                );
            });

            Self::card_frame().show(&mut cols[1], |ui| {
                ui.label(egui::RichText::new("GPU CLOCKS / POWERPLAY").size(11.0).color(TEXT_MUTED));
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(format!(
                        "{} MHz Core / {} MHz Mem",
                        self.telemetry.live_core_mhz, self.telemetry.live_mem_mhz
                    ))
                    .size(14.0)
                    .strong()
                    .color(TEXT_PRIMARY),
                );
                let pp_txt = if self.telemetry.soft_pp_active {
                    "SoftPowerPlayTable: ACTIVE"
                } else {
                    "SoftPowerPlayTable: Stock VBIOS"
                };
                ui.label(
                    egui::RichText::new(pp_txt).size(12.0).color(
                        if self.telemetry.soft_pp_active {
                            STATUS_OK
                        } else {
                            TEXT_MUTED
                        },
                    ),
                );
            });

            Self::card_frame().show(&mut cols[2], |ui| {
                ui.label(egui::RichText::new("AMD BACKGROUND BLOAT").size(11.0).color(TEXT_MUTED));
                ui.add_space(4.0);
                let bloat_col = if self.running_bloat == 0 {
                    STATUS_OK
                } else {
                    STATUS_WARN
                };
                ui.label(
                    egui::RichText::new(format!("{} Running Process(es)", self.running_bloat))
                        .size(14.0)
                        .strong()
                        .color(bloat_col),
                );
                ui.label(
                    egui::RichText::new(if self.tweaks.boot_guard_installed {
                        "Boot Guard: Active (12s exit)"
                    } else {
                        "Boot Guard: Not Installed"
                    })
                    .size(12.0)
                    .color(TEXT_MUTED),
                );
            });
        });

        ui.add_space(14.0);

        Self::card_frame().show(ui, |ui| {
            ui.label(egui::RichText::new("Quick Actions").size(15.0).strong().color(TEXT_PRIMARY));
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                if ui.add(Self::primary_btn("Apply Recommended Gaming Tweaks")).clicked() {
                    let rec = TweakState {
                        ulps_disabled: true,
                        stutter_mode_off: true,
                        mpo_disabled: true,
                        gamedvr_disabled: true,
                        flip_model_enabled: true,
                        tessellation_4x: true,
                        msi_mode_enabled: true,
                        ac_cores_unparked: true,
                        ..self.tweaks.clone()
                    };
                    let logs = tweaks::apply_tweaks(&rec);
                    let rep = debloat::strip_live_bloat();
                    self.tweaks = tweaks::read_tweak_state();
                    self.running_bloat = debloat::count_running_bloat();
                    self.set_status(
                        format!(
                            "Applied {} tweak steps & stripped {} bloat process(es).",
                            logs.len(),
                            rep.killed
                        ),
                        false,
                    );
                }

                if ui.add(Self::secondary_btn("Strip Running AMD Bloat Now")).clicked() {
                    let rep = debloat::strip_live_bloat();
                    self.running_bloat = debloat::count_running_bloat();
                    self.set_status(
                        format!(
                            "Killed {} process(es), disabled {} service(s) & {} task(s).",
                            rep.killed, rep.services_disabled, rep.tasks_disabled
                        ),
                        false,
                    );
                }

                if self.tweaks.gui_installed {
                    if ui.add(Self::secondary_btn("Uninstall RBridge Shortcut")).clicked() {
                        match tweaks::uninstall_rbridge_gui() {
                            Ok(m) => self.set_status(m, false),
                            Err(e) => self.set_status(e, true),
                        }
                        self.refresh_all();
                    }
                } else if ui.add(Self::secondary_btn("Install RBridge GUI to Start Menu")).clicked() {
                    match tweaks::install_rbridge_gui() {
                        Ok(m) => self.set_status(m, false),
                        Err(e) => self.set_status(e, true),
                    }
                    self.refresh_all();
                }
            });
        });

        ui.add_space(12.0);
        Self::card_frame().show(ui, |ui| {
            ui.label(egui::RichText::new("Recommended Setup for Dell HD 8670M + Intel HD 4400").size(14.0).strong().color(TEXT_PRIMARY));
            ui.add_space(4.0);
            ui.label(egui::RichText::new("• Driver: AMD Radeon PRO 21.Q2.1 (best Enduro stability) OR Amernime / R.ID 23.9.1 iCafe GCNFlex (best OpenGL/modern Vulkan).").color(TEXT_MUTED));
            ui.label(egui::RichText::new("• Overclock: Export Oland.rom from GPU-Z once, load it in Overclock tab, apply SoftPowerPlayTable, and click Reload AMD GPU.").color(TEXT_MUTED));
            ui.label(egui::RichText::new("• Scaling & VRAM: Inject 16:9 resolutions in Display tab and quarantine browsers/Discord to Intel HD 4400.").color(TEXT_MUTED));
        });
    }

    fn render_patcher(&mut self, ui: &mut egui::Ui) {
        ui.heading(egui::RichText::new("Dual-Driver Patcher & Installer").size(22.0).color(TEXT_PRIMARY));
        ui.label(
            egui::RichText::new("Auto-detects AMD Radeon PRO 21.Q2.1 and Amernime Zone / R.ID (23.9.1 iCafe) packages.")
                .color(TEXT_MUTED),
        );
        ui.add_space(10.0);

        Self::card_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Extracted Driver Folder:").color(TEXT_PRIMARY));
                ui.add(
                    egui::TextEdit::singleline(&mut self.pkg_dir)
                        .desired_width(420.0)
                        .hint_text(r"e.g. C:\Drivers\23.9.1-iCafe or C:\AMD\21.Q2.1"),
                );
                if ui.add(Self::secondary_btn("Browse...")).clicked() {
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                        self.pkg_dir = folder.to_string_lossy().into_owned();
                        self.detected_pkg = Some(patcher::detect_driver_package(&folder));
                    }
                }
                if ui.add(Self::secondary_btn("Scan")).clicked() && !self.pkg_dir.trim().is_empty() {
                    let p = PathBuf::from(self.pkg_dir.trim());
                    self.detected_pkg = Some(patcher::detect_driver_package(&p));
                }
            });

            ui.add_space(8.0);
            if let Some(pkg) = &self.detected_pkg {
                let col = match pkg.kind {
                    DriverPackageKind::Unknown => egui::Color32::from_rgb(231, 76, 60),
                    _ => STATUS_OK,
                };
                ui.label(egui::RichText::new(format!("Detected: {}", pkg.label)).strong().color(col));
            }
        });

        ui.add_space(10.0);

        Self::card_frame().show(ui, |ui| {
            ui.label(egui::RichText::new("Patch & Install Options").size(15.0).strong().color(TEXT_PRIMARY));
            ui.add_space(6.0);
            ui.checkbox(
                &mut self.patch_opts.strip_amd_bloat,
                "Strip all background AMD processes (AMDRSServ, RadeonSoftware, Crash Defender, atieclxx, ccc2_install)",
            );
            ui.checkbox(
                &mut self.patch_opts.keep_msi_supported,
                "Enable PCIe MSI Mode (MSISupported = 1 — lowest Enduro DPC latency)",
            );
            ui.checkbox(
                &mut self.patch_opts.rebrand_r8_m670,
                "Apply Radeon R8 M670 branding string (21.Q2.1 only)",
            );
            ui.checkbox(
                &mut self.patch_opts.install_driver,
                "Install patched driver automatically via pnputil after signing",
            );
            ui.checkbox(
                &mut self.patch_opts.install_gui,
                "Install RBridge Control Center GUI to Program Files & Start Menu (uncheck for Driver-Only Mode)",
            );
            ui.checkbox(
                &mut self.patch_opts.install_boot_guard,
                "Install silent 12-second Boot Guard daemon (0 MB resident RAM)",
            );

            ui.add_space(10.0);
            let can_run = !self.busy
                && self
                    .detected_pkg
                    .as_ref()
                    .map(|p| p.kind != DriverPackageKind::Unknown)
                    .unwrap_or(false);

            if ui
                .add_enabled(can_run, Self::primary_btn("Patch, Sign & Install"))
                .clicked()
            {
                if let Some(pkg) = self.detected_pkg.clone() {
                    let opts = self.patch_opts.clone();
                    let (tx, rx) = mpsc::channel();
                    self.worker_rx = Some(rx);
                    self.busy = true;
                    self.patch_logs.clear();
                    self.set_status("Running RBridge driver pipeline...", false);

                    thread::spawn(move || {
                        let res = patcher::run_patch_pipeline(&pkg, &opts, |line| {
                            let _ = tx.send(WorkerMsg::Log(line));
                        });
                        let _ = tx.send(WorkerMsg::Done(res));
                    });
                }
            }
        });

        ui.add_space(10.0);
        Self::card_frame().show(ui, |ui| {
            ui.label(egui::RichText::new("Pipeline Output").size(13.0).strong().color(TEXT_MUTED));
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .max_height(180.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &self.patch_logs {
                        ui.label(egui::RichText::new(line).monospace().size(12.0).color(TEXT_PRIMARY));
                    }
                });
        });
    }

    fn render_performance(&mut self, ui: &mut egui::Ui) {
        ui.heading(egui::RichText::new("Performance & Latency Tuning").size(22.0).color(TEXT_PRIMARY));
        ui.add_space(10.0);

        Self::card_frame().show(ui, |ui| {
            ui.checkbox(&mut self.tweaks.ulps_disabled, "Disable ULPS (prevents Enduro sleep stutter & clock drops)");
            ui.checkbox(&mut self.tweaks.stutter_mode_off, "Disable AMD StutterMode + enable KMD Large Pages");
            ui.checkbox(&mut self.tweaks.mpo_disabled, "Disable Windows MPO (fixes DX11/Vulkan overlay & hybrid frame-pacing)");
            ui.checkbox(&mut self.tweaks.gamedvr_disabled, "Disable Xbox GameDVR background capture");
            ui.checkbox(&mut self.tweaks.flip_model_enabled, "Force DX10/DX11 Flip Model (SwapEffectUpgradeEnable = 1)");
            ui.checkbox(&mut self.tweaks.tessellation_4x, "Cap Tessellation at 4x (major GCN 1.0 shader boost)");
            ui.checkbox(&mut self.tweaks.msi_mode_enabled, "Enable PCIe MSI Mode + High Interrupt Priority (VEN_1002)");
            ui.checkbox(&mut self.tweaks.ac_cores_unparked, "Unpark CPU Cores (100%) & disable PCIe ASPM on AC power");

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.add(Self::primary_btn("Apply All Tweaks")).clicked() {
                    let logs = tweaks::apply_tweaks(&self.tweaks);
                    self.tweaks = tweaks::read_tweak_state();
                    self.set_status(
                        logs.last().cloned().unwrap_or_else(|| "Tweaks applied.".into()),
                        false,
                    );
                }

                if self.tweaks.boot_guard_installed {
                    if ui.add(Self::secondary_btn("Remove Boot Guard")).clicked() {
                        match tweaks::remove_boot_guard() {
                            Ok(m) => self.set_status(m, false),
                            Err(e) => self.set_status(e, true),
                        }
                        self.tweaks = tweaks::read_tweak_state();
                    }
                } else if ui.add(Self::secondary_btn("Install Boot Guard (0 MB RAM)")).clicked() {
                    match tweaks::install_boot_guard() {
                        Ok(m) => self.set_status(m, false),
                        Err(e) => self.set_status(e, true),
                    }
                    self.tweaks = tweaks::read_tweak_state();
                }
            });
        });
    }

    fn render_overclock(&mut self, ui: &mut egui::Ui) {
        ui.heading(egui::RichText::new("Overclock — ATOMBIOS SoftPowerPlayTable").size(22.0).color(TEXT_PRIMARY));
        ui.label(
            egui::RichText::new("Bypasses Dell VBIOS ADL locks by extracting PowerPlayInfo from Oland.rom and injecting PP_PhmSoftPowerPlayTable.")
                .color(TEXT_MUTED),
        );
        ui.add_space(10.0);

        Self::card_frame().show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.add(Self::primary_btn("Auto-Extract from ACPI")).clicked() {
                    match powerplay::extract_powerplay_from_acpi() {
                        Ok((pp, source)) => {
                            self.stock_core_mhz = pp.boost_core_mhz;
                            self.stock_mem_mhz = pp.boost_mem_mhz;
                            self.target_core_mhz = pp.boost_core_mhz;
                            self.target_mem_mhz = pp.boost_mem_mhz;
                            self.rom_path_label = format!(
                                "Extracted from {} ({} bytes, rev {}.{}, {} DPM states)",
                                source,
                                pp.table_bytes.len(),
                                pp.format_rev,
                                pp.content_rev,
                                pp.states.len()
                            );
                            self.parsed_pp = Some(pp);
                            self.set_status(
                                format!("Auto-extracted & cached stock PowerPlay table from {}!", source),
                                false,
                            );
                        }
                        Err(e) => self.set_status(e, true),
                    }
                }
                if ui.add(Self::secondary_btn("Load VBIOS ROM (.rom)")).clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("AMD VBIOS ROM", &["rom", "bin"])
                        .pick_file()
                    {
                        match powerplay::extract_powerplay_from_rom(&path) {
                            Ok(pp) => {
                                self.stock_core_mhz = pp.boost_core_mhz;
                                self.stock_mem_mhz = pp.boost_mem_mhz;
                                self.target_core_mhz = pp.boost_core_mhz;
                                self.target_mem_mhz = pp.boost_mem_mhz;
                                self.rom_path_label = format!(
                                    "Loaded {} ({} bytes, rev {}.{}, {} DPM states found)",
                                    path.file_name().unwrap_or_default().to_string_lossy(),
                                    pp.table_bytes.len(),
                                    pp.format_rev,
                                    pp.content_rev,
                                    pp.states.len()
                                );
                                self.parsed_pp = Some(pp);
                                self.set_status("Extracted & cached stock PowerPlay table from VBIOS ROM.", false);
                            }
                            Err(e) => self.set_status(e, true),
                        }
                    }
                }
                ui.label(egui::RichText::new(&self.rom_path_label).size(12.0).color(TEXT_MUTED));
            });

            if let Some(pp) = &self.parsed_pp {
                ui.add_space(8.0);
                let states_str: String = pp
                    .states
                    .iter()
                    .enumerate()
                    .map(|(i, s)| format!("DPM{}: {}/{} MHz", i, s.core_mhz, s.mem_mhz))
                    .collect::<Vec<_>>()
                    .join("  |  ");
                ui.label(
                    egui::RichText::new(if states_str.is_empty() {
                        format!("Detected Boost State: {}/{} MHz", self.stock_core_mhz, self.stock_mem_mhz)
                    } else {
                        states_str
                    })
                    .monospace()
                    .size(12.0)
                    .color(PRO_BLUE_BRIGHT),
                );
            }
        });

        ui.add_space(10.0);

        Self::card_frame().show(ui, |ui| {
            ui.label(egui::RichText::new("Target Boost Clocks (Step Memory +25 MHz first on 64-bit DDR3)").size(14.0).strong().color(TEXT_PRIMARY));
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                ui.label("Stock Boost (Match):");
                ui.add(egui::DragValue::new(&mut self.stock_core_mhz).range(600..=1200).suffix(" MHz Core"));
                ui.add(egui::DragValue::new(&mut self.stock_mem_mhz).range(600..=1200).suffix(" MHz Mem"));
            });
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Target Core Clock:  ").strong());
                ui.add(egui::Slider::new(&mut self.target_core_mhz, 800..=1150).suffix(" MHz"));
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Target Memory Clock:").strong());
                ui.add(egui::Slider::new(&mut self.target_mem_mhz, 800..=1100).suffix(" MHz"));
            });

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let has_table = self.parsed_pp.is_some();
                if ui
                    .add_enabled(has_table, Self::primary_btn("Apply SoftPowerPlayTable"))
                    .clicked()
                {
                    if let Some(pp) = &self.parsed_pp {
                        match powerplay::apply_powerplay_oc(
                            &pp.table_bytes,
                            self.stock_core_mhz,
                            self.stock_mem_mhz,
                            self.target_core_mhz,
                            self.target_mem_mhz,
                        ) {
                            Ok(m) => self.set_status(m, false),
                            Err(e) => self.set_status(e, true),
                        }
                        self.refresh_all();
                    }
                }

                if ui
                    .add_enabled(!self.busy, Self::secondary_btn("Reload AMD GPU Now"))
                    .clicked()
                {
                    let (tx, rx) = mpsc::channel();
                    self.worker_rx = Some(rx);
                    self.busy = true;
                    self.set_status("Reloading AMD Radeon display adapter...", false);
                    thread::spawn(move || {
                        let _ = tx.send(WorkerMsg::Done(powerplay::reload_amd_gpu()));
                    });
                }

                if ui.add(Self::secondary_btn("Restore Factory Clocks")).clicked() {
                    match powerplay::restore_factory_powerplay() {
                        Ok(m) => self.set_status(m, false),
                        Err(e) => self.set_status(e, true),
                    }
                    self.target_core_mhz = self.stock_core_mhz;
                    self.target_mem_mhz = self.stock_mem_mhz;
                    self.refresh_all();
                }
            });
        });
    }

    fn render_display(&mut self, ui: &mut egui::Ui) {
        ui.heading(egui::RichText::new("16:9 Custom Resolution Injector").size(22.0).color(TEXT_PRIMARY));
        ui.label(
            egui::RichText::new("Injects 1280x720, 1152x648, 1024x576, 960x540, 896x504, and 800x450 @ 60Hz via EDID_OVERRIDE.")
                .color(TEXT_MUTED),
        );
        ui.add_space(10.0);

        let monitors = self.monitors.clone();
        if monitors.is_empty() {
            Self::card_frame().show(ui, |ui| {
                ui.label("No active display EDID found in registry.");
            });
            return;
        }

        for mon in &monitors {
            Self::card_frame().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} ({}x{} Native)",
                                mon.model, mon.native_w, mon.native_h
                            ))
                            .size(15.0)
                            .strong()
                            .color(TEXT_PRIMARY),
                        );
                        ui.label(
                            egui::RichText::new(if mon.has_override {
                                "Status: 16:9 EDID_OVERRIDE Installed"
                            } else {
                                "Status: Factory EDID"
                            })
                            .size(12.0)
                            .color(if mon.has_override { STATUS_OK } else { TEXT_MUTED }),
                        );
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(Self::secondary_btn("Revert Stock EDID")).clicked() {
                            match display::revert_edid(mon) {
                                Ok(m) => self.set_status(m, false),
                                Err(e) => self.set_status(e, true),
                            }
                            self.monitors = display::enumerate_monitors();
                        }
                        if ui.add(Self::primary_btn("Inject 16:9 Modes")).clicked() {
                            match display::inject_16_9_modes(mon) {
                                Ok(m) => self.set_status(m, false),
                                Err(e) => self.set_status(e, true),
                            }
                            self.monitors = display::enumerate_monitors();
                        }
                    });
                });
            });
            ui.add_space(8.0);
        }
    }

    fn render_quarantine(&mut self, ui: &mut egui::Ui) {
        ui.heading(egui::RichText::new("VRAM Quarantine & GPU Pinning").size(22.0).color(TEXT_PRIMARY));
        ui.label(
            egui::RichText::new("Pin browsers & launchers to Intel HD 4400 to keep your 2 GB DDR3 VRAM free for games on the AMD HD 8670M.")
                .color(TEXT_MUTED),
        );
        ui.add_space(10.0);

        Self::card_frame().show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add(Self::primary_btn("Auto-Quarantine Browsers & Apps -> Intel"))
                    .clicked()
                {
                    match quarantine::quarantine_background_apps() {
                        Ok(n) => self.set_status(
                            format!("Quarantined {} background app(s) to Intel HD 4400.", n),
                            false,
                        ),
                        Err(e) => self.set_status(e, true),
                    }
                    self.pins = quarantine::list_pinned_apps();
                }

                if ui.add(Self::secondary_btn("Pin Game (.exe) -> AMD HD 8670M")).clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("Game Executable", &["exe"])
                        .pick_file()
                    {
                        match quarantine::pin_executable(&path.to_string_lossy(), true) {
                            Ok(m) => self.set_status(m, false),
                            Err(e) => self.set_status(e, true),
                        }
                        self.pins = quarantine::list_pinned_apps();
                    }
                }
            });
        });

        ui.add_space(10.0);
        Self::card_frame().show(ui, |ui| {
            ui.label(egui::RichText::new("Active Windows GPU Assignments").size(14.0).strong().color(TEXT_PRIMARY));
            ui.add_space(6.0);
            let pins = self.pins.clone();
            egui::ScrollArea::vertical().max_height(310.0).show(ui, |ui| {
                for entry in &pins {
                    ui.horizontal(|ui| {
                        let badge = if entry.high_performance {
                            egui::RichText::new("[AMD 8670M]").strong().color(PRO_BLUE_BRIGHT)
                        } else {
                            egui::RichText::new("[INTEL 4400]").strong().color(STATUS_OK)
                        };
                        ui.label(badge);
                        ui.label(egui::RichText::new(&entry.exe_path).size(12.0).color(TEXT_PRIMARY));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Remove").clicked() {
                                let _ = quarantine::remove_pin(&entry.exe_path);
                                self.pins = quarantine::list_pinned_apps();
                            }
                        });
                    });
                    ui.separator();
                }
            });
        });
    }
}

impl eframe::App for RBridgeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_worker(ctx);
        self.render_top_bar(ctx);
        self.render_sidebar(ctx);
        self.render_footer(ctx);

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(CANVAS_BG)
                    .inner_margin(egui::Margin::same(18.0)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| match self.tab {
                    Tab::Dashboard => self.render_dashboard(ui),
                    Tab::Patcher => self.render_patcher(ui),
                    Tab::Performance => self.render_performance(ui),
                    Tab::Overclock => self.render_overclock(ui),
                    Tab::Display => self.render_display(ui),
                    Tab::Quarantine => self.render_quarantine(ui),
                });
            });
    }
}

fn apply_radeon_pro_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = CANVAS_BG;
    visuals.window_fill = CARD_BG;
    visuals.extreme_bg_color = egui::Color32::from_rgb(12, 15, 19);
    visuals.faint_bg_color = egui::Color32::from_rgb(24, 29, 36);
    visuals.selection.bg_fill = PRO_BLUE;
    visuals.selection.stroke = egui::Stroke::new(1.0, PRO_BLUE_BRIGHT);
    visuals.widgets.noninteractive.bg_fill = CARD_BG;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, CARD_BORDER);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(35, 42, 53);
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, CARD_BORDER);
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(44, 55, 70);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, PRO_BLUE_BRIGHT);
    visuals.widgets.active.bg_fill = PRO_BLUE;
    ctx.set_visuals(visuals);
}

pub fn launch_gui() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1060.0, 680.0])
            .with_min_inner_size([880.0, 560.0])
            .with_title("RBridge — Radeon PRO Control Center"),
        ..Default::default()
    };

    eframe::run_native(
        "RBridge — Radeon PRO Control Center",
        options,
        Box::new(|cc| {
            apply_radeon_pro_theme(&cc.egui_ctx);
            Ok(Box::new(RBridgeApp::new()))
        }),
    )
}
