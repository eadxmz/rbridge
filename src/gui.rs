use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::debloat;
use crate::display::{self, MonitorEntry};
use crate::patcher::{self, DetectedPackage, DriverPackageKind, PatchOptions};
use crate::powerplay::{self, AdlDiagInfo, GpuTelemetry, OcMode, ParsedPowerPlay};
use crate::quarantine::{self, GpuPinEntry};
use crate::tweaks::{self, TweakState};

const ICON_RGBA: &[u8] = include_bytes!("../assets/icon_64.rgba");

// Modern Obsidian Glass + Radeon Cobalt / Electric Cyan Palette
const CANVAS_BG: egui::Color32 = egui::Color32::from_rgb(9, 12, 18); // #090C12
const RAIL_BG: egui::Color32 = egui::Color32::from_rgb(13, 17, 26); // #0D111A
const CARD_BG: egui::Color32 = egui::Color32::from_rgb(17, 22, 34); // #111622
const CARD_ELEVATED: egui::Color32 = egui::Color32::from_rgb(22, 29, 44); // #161D2C
const GLASS_BORDER: egui::Color32 = egui::Color32::from_rgb(35, 48, 72); // #233048
const PRO_COBALT: egui::Color32 = egui::Color32::from_rgb(20, 124, 232); // #147CE8
const ELECTRIC_CYAN: egui::Color32 = egui::Color32::from_rgb(56, 189, 248); // #38BDF8
const TEXT_PRIMARY: egui::Color32 = egui::Color32::from_rgb(241, 245, 249);
const TEXT_MUTED: egui::Color32 = egui::Color32::from_rgb(148, 163, 184);
const MINT_OK: egui::Color32 = egui::Color32::from_rgb(16, 185, 129);
const AMBER_WARN: egui::Color32 = egui::Color32::from_rgb(245, 158, 11);
const CORAL_ERR: egui::Color32 = egui::Color32::from_rgb(239, 68, 68);

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
    logo_tex: egui::TextureHandle,
    is_admin: bool,
    telemetry: GpuTelemetry,
    adl_diag: Option<AdlDiagInfo>,
    running_bloat: usize,
    tweaks: TweakState,

    // Driver Patcher
    pkg_dir: String,
    detected_pkg: Option<DetectedPackage>,
    selected_inf_idx: usize,
    patch_opts: PatchOptions,
    patch_logs: Vec<String>,
    worker_rx: Option<Receiver<WorkerMsg>>,
    busy: bool,

    // Overclock
    oc_mode: OcMode,
    boot_auto_oc: bool,
    parsed_pp: Option<ParsedPowerPlay>,
    rom_path_label: String,
    stock_core_mhz: u32,
    stock_mem_mhz: u32,
    target_core_mhz: u32,
    target_mem_mhz: u32,

    // Display & Quarantine
    monitors: Vec<MonitorEntry>,
    pins: Vec<GpuPinEntry>,

    // Footer status
    status_line: String,
    status_is_error: bool,
}

impl RBridgeApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let logo_tex = cc.egui_ctx.load_texture(
            "rbridge_logo",
            egui::ColorImage::from_rgba_unmultiplied([64, 64], ICON_RGBA),
            egui::TextureOptions::LINEAR,
        );

        let telemetry = powerplay::query_gpu_telemetry();
        let adl_diag = powerplay::query_adl_diag(false).ok();
        let parsed_pp = powerplay::load_cached_powerplay();
        let (stock_c, stock_m) = parsed_pp
            .as_ref()
            .map(|p| (p.boost_core_mhz, p.boost_mem_mhz))
            .unwrap_or((975, 900));
        let target_c = if telemetry.saved_oc_core > 0 {
            telemetry.saved_oc_core
        } else {
            telemetry.live_core_mhz.max(975)
        };
        let target_m = if telemetry.saved_oc_mem > 0 {
            telemetry.saved_oc_mem
        } else {
            telemetry.live_mem_mhz.max(900)
        };

        let rom_label = if parsed_pp.is_some() {
            "Cached stock PowerPlay table ready in registry".into()
        } else {
            "Optional: load a .rom file only if you want SoftPowerPlayTable override".into()
        };

        let oc_mode = telemetry.saved_oc_mode;
        let boot_auto_oc = telemetry.boot_oc_enabled;

        Self {
            tab: Tab::Dashboard,
            logo_tex,
            is_admin: tweaks::is_elevated(),
            telemetry,
            adl_diag,
            running_bloat: debloat::count_running_bloat(),
            tweaks: tweaks::read_tweak_state(),
            pkg_dir: String::new(),
            detected_pkg: None,
            selected_inf_idx: 0,
            patch_opts: PatchOptions::default(),
            patch_logs: vec!["Select an extracted AMD 21.Q2.1 or Amernime / R.ID 23.9.1 iCafe folder.".into()],
            worker_rx: None,
            busy: false,
            oc_mode,
            boot_auto_oc,
            parsed_pp,
            rom_path_label: rom_label,
            stock_core_mhz: stock_c,
            stock_mem_mhz: stock_m,
            target_core_mhz: target_c,
            target_mem_mhz: target_m,
            monitors: display::enumerate_monitors(),
            pins: quarantine::list_pinned_apps(),
            status_line: "RBridge v2.1 ready — zero background RAM footprint.".into(),
            status_is_error: false,
        }
    }

    fn set_status(&mut self, msg: impl Into<String>, is_err: bool) {
        self.status_line = msg.into();
        self.status_is_error = is_err;
    }

    fn refresh_all(&mut self, wake_gpu: bool) {
        self.telemetry = powerplay::query_gpu_telemetry();
        self.adl_diag = powerplay::query_adl_diag(wake_gpu).ok();
        if let Some(d) = &self.adl_diag {
            if d.cur_core_mhz > 0 {
                self.telemetry.live_core_mhz = d.cur_core_mhz;
            }
            if d.cur_mem_mhz > 0 {
                self.telemetry.live_mem_mhz = d.cur_mem_mhz;
            }
            if d.temp_c.is_some() {
                self.telemetry.temp_c = d.temp_c;
            }
            if d.load_pct.is_some() {
                self.telemetry.load_pct = d.load_pct;
            }
        }
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
                                self.status_line = msg;
                                self.status_is_error = false;
                            }
                            Err(err) => {
                                self.patch_logs.push(format!("[ERR] {}", err));
                                self.status_line = err;
                                self.status_is_error = true;
                            }
                        }
                    }
                }
            }
        }
        if finished {
            self.worker_rx = None;
            self.busy = false;
            self.refresh_all(false);
        } else if self.busy {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn glass_card() -> egui::Frame {
        egui::Frame::none()
            .fill(CARD_BG)
            .stroke(egui::Stroke::new(1.0_f32, GLASS_BORDER))
            .rounding(egui::Rounding::same(14.0_f32))
            .inner_margin(egui::Margin::same(16.0_f32))
    }

    fn pill_primary(label: &str) -> egui::Button<'_> {
        egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE).strong().size(13.0))
            .fill(PRO_COBALT)
            .stroke(egui::Stroke::new(1.0_f32, ELECTRIC_CYAN))
            .rounding(egui::Rounding::same(18.0_f32))
            .min_size(egui::vec2(140.0, 34.0))
    }

    fn pill_secondary(label: &str) -> egui::Button<'_> {
        egui::Button::new(egui::RichText::new(label).color(TEXT_PRIMARY).size(13.0))
            .fill(CARD_ELEVATED)
            .stroke(egui::Stroke::new(1.0_f32, GLASS_BORDER))
            .rounding(egui::Rounding::same(18.0_f32))
            .min_size(egui::vec2(124.0, 32.0))
    }

    fn badge_pill(ui: &mut egui::Ui, text: &str, fill: egui::Color32, txt: egui::Color32) {
        egui::Frame::none()
            .fill(fill)
            .rounding(egui::Rounding::same(12.0_f32))
            .inner_margin(egui::Margin::symmetric(10.0_f32, 3.0_f32))
            .show(ui, |ui| {
                ui.label(egui::RichText::new(text).size(11.0).strong().color(txt));
            });
    }

    fn toggle_row(ui: &mut egui::Ui, on: &mut bool, title: &str, subtitle: &str) {
        ui.horizontal(|ui| {
            let desired_size = egui::vec2(42.0, 22.0);
            let (rect, mut response) = ui.allocate_exact_size(desired_size, egui::Sense::click());
            if response.clicked() {
                *on = !*on;
                response.mark_changed();
            }
            if ui.is_rect_visible(rect) {
                let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
                let radius = 0.5 * rect.height();
                let bg = egui::Color32::from_rgb(
                    (30.0 + (20.0 - 30.0) * how_on) as u8,
                    (41.0 + (124.0 - 41.0) * how_on) as u8,
                    (59.0 + (232.0 - 59.0) * how_on) as u8,
                );
                let border = if *on { ELECTRIC_CYAN } else { GLASS_BORDER };
                ui.painter()
                    .rect(rect, radius, bg, egui::Stroke::new(1.0_f32, border));
                let cx = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
                ui.painter().circle_filled(
                    egui::pos2(cx, rect.center().y),
                    radius - 3.0,
                    egui::Color32::WHITE,
                );
            }
            ui.add_space(6.0);
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(title).size(13.5).strong().color(TEXT_PRIMARY));
                ui.label(egui::RichText::new(subtitle).size(11.5).color(TEXT_MUTED));
            });
        });
        ui.add_space(6.0);
    }

    fn metric_bar(ui: &mut egui::Ui, label: &str, value_txt: &str, frac: f32, accent: egui::Color32) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(label).size(12.0).color(TEXT_MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(value_txt).size(13.0).strong().color(TEXT_PRIMARY));
            });
        });
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 7.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 3.5_f32, egui::Color32::from_rgb(26, 35, 54));
        let fill_w = (rect.width() * frac.clamp(0.04, 1.0)).max(6.0);
        let fill_rect = egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, rect.height()));
        ui.painter().rect_filled(fill_rect, 3.5_f32, accent);
        ui.add_space(8.0);
    }

    fn render_top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top_bar")
            .exact_height(60.0)
            .frame(
                egui::Frame::none()
                    .fill(RAIL_BG)
                    .stroke(egui::Stroke::new(1.0_f32, GLASS_BORDER))
                    .inner_margin(egui::Margin::symmetric(18.0_f32, 10.0_f32)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.image((self.logo_tex.id(), egui::vec2(34.0, 34.0)));
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("RBRIDGE").size(18.0).strong().color(TEXT_PRIMARY));
                            Self::badge_pill(ui, "PRO v2.1", PRO_COBALT, egui::Color32::WHITE);
                        });
                        ui.label(egui::RichText::new(&self.telemetry.gpu_name).size(11.5).color(ELECTRIC_CYAN));
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.is_admin {
                            Self::badge_pill(
                                ui,
                                "ADMIN ELEVATED",
                                egui::Color32::from_rgb(16, 65, 52),
                                MINT_OK,
                            );
                        } else if ui.add(Self::pill_primary("Relaunch as Admin")).clicked() {
                            tweaks::relaunch_as_admin();
                        }
                        ui.add_space(6.0);
                        if ui.add(Self::pill_secondary("Wake & Refresh")).clicked() {
                            self.refresh_all(true);
                            self.set_status("Woke AMD GPU & refreshed live sensors.", false);
                        }
                    });
                });
            });
    }

    fn render_sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("nav_rail")
            .resizable(false)
            .exact_width(220.0)
            .frame(
                egui::Frame::none()
                    .fill(RAIL_BG)
                    .stroke(egui::Stroke::new(1.0_f32, GLASS_BORDER))
                    .inner_margin(egui::Margin::same(14.0_f32)),
            )
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.label(egui::RichText::new("NAVIGATION").size(10.5).strong().color(TEXT_MUTED));
                ui.add_space(10.0);

                let items = [
                    (Tab::Dashboard, "Dashboard"),
                    (Tab::Patcher, "Driver Patcher"),
                    (Tab::Performance, "Performance"),
                    (Tab::Overclock, "Overclock Studio"),
                    (Tab::Display, "Display 16:9"),
                    (Tab::Quarantine, "VRAM Quarantine"),
                ];

                for (tab, label) in items {
                    let active = self.tab == tab;
                    let fill = if active {
                        egui::Color32::from_rgb(19, 64, 122)
                    } else {
                        egui::Color32::TRANSPARENT
                    };
                    let stroke = if active {
                        egui::Stroke::new(1.0_f32, ELECTRIC_CYAN)
                    } else {
                        egui::Stroke::NONE
                    };
                    let txt = if active {
                        egui::Color32::WHITE
                    } else {
                        TEXT_MUTED
                    };

                    let btn = egui::Button::new(egui::RichText::new(label).size(13.5).color(txt).strong())
                        .fill(fill)
                        .stroke(stroke)
                        .rounding(egui::Rounding::same(18.0_f32))
                        .min_size(egui::vec2(192.0, 38.0));

                    if ui.add(btn).clicked() {
                        self.tab = tab;
                    }
                    ui.add_space(5.0);
                }

                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    Self::glass_card().inner_margin(egui::Margin::same(10.0_f32)).show(ui, |ui| {
                        ui.label(egui::RichText::new("HARDWARE TARGET").size(10.0).strong().color(TEXT_MUTED));
                        ui.label(egui::RichText::new("Intel HD 4400 + HD 8670M").size(11.5).strong().color(ELECTRIC_CYAN));
                        ui.label(egui::RichText::new("0 MB Idle RAM Footprint").size(11.0).color(MINT_OK));
                    });
                });
            });
    }

    fn render_footer(&self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status_bar")
            .exact_height(32.0)
            .frame(
                egui::Frame::none()
                    .fill(RAIL_BG)
                    .stroke(egui::Stroke::new(1.0_f32, GLASS_BORDER))
                    .inner_margin(egui::Margin::symmetric(16.0_f32, 6.0_f32)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let dot_col = if self.status_is_error { CORAL_ERR } else { MINT_OK };
                    let (r, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    ui.painter().circle_filled(r.center(), 4.0, dot_col);
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(&self.status_line)
                            .size(12.0)
                            .color(if self.status_is_error { CORAL_ERR } else { TEXT_MUTED }),
                    );
                });
            });
    }

    fn render_dashboard(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading(egui::RichText::new("Command Dashboard").size(24.0).strong().color(TEXT_PRIMARY));
            ui.add_space(10.0);
            Self::badge_pill(
                ui,
                if self.running_bloat == 0 { "BLOAT-FREE" } else { "BLOAT DETECTED" },
                if self.running_bloat == 0 {
                    egui::Color32::from_rgb(16, 65, 52)
                } else {
                    egui::Color32::from_rgb(78, 52, 12)
                },
                if self.running_bloat == 0 { MINT_OK } else { AMBER_WARN },
            );
        });
        ui.add_space(12.0);

        ui.columns(2, |cols| {
            Self::glass_card().show(&mut cols[0], |ui| {
                ui.label(egui::RichText::new("LIVE GPU TELEMETRY").size(11.0).strong().color(ELECTRIC_CYAN));
                ui.add_space(8.0);
                Self::metric_bar(
                    ui,
                    "Core Engine Clock",
                    &format!("{} MHz", self.telemetry.live_core_mhz),
                    self.telemetry.live_core_mhz as f32 / 1150.0,
                    PRO_COBALT,
                );
                Self::metric_bar(
                    ui,
                    "DDR3 Memory Clock",
                    &format!("{} MHz", self.telemetry.live_mem_mhz),
                    self.telemetry.live_mem_mhz as f32 / 1100.0,
                    ELECTRIC_CYAN,
                );
                let temp_val = self.telemetry.temp_c.unwrap_or(0);
                Self::metric_bar(
                    ui,
                    "GPU Die Temperature",
                    &self.telemetry.temp_c.map(|t| format!("{} °C", t)).unwrap_or_else(|| "Idle / D3".into()),
                    temp_val as f32 / 95.0,
                    if temp_val >= 82 { CORAL_ERR } else { MINT_OK },
                );
                let load_val = self.telemetry.load_pct.unwrap_or(0);
                Self::metric_bar(
                    ui,
                    "3D Engine Load",
                    &self.telemetry.load_pct.map(|l| format!("{}%", l)).unwrap_or_else(|| "Idle".into()),
                    load_val as f32 / 100.0,
                    AMBER_WARN,
                );
            });

            Self::glass_card().show(&mut cols[1], |ui| {
                ui.label(egui::RichText::new("DRIVER & SYSTEM STATUS").size(11.0).strong().color(ELECTRIC_CYAN));
                ui.add_space(8.0);
                ui.label(egui::RichText::new(&self.telemetry.driver_stack).size(16.0).strong().color(TEXT_PRIMARY));
                ui.label(egui::RichText::new(format!("Driver Build: {}", self.telemetry.driver_version)).size(12.0).color(TEXT_MUTED));
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new(format!(
                        "AMD Background Processes: {}",
                        self.running_bloat
                    ))
                    .size(13.0)
                    .color(if self.running_bloat == 0 { MINT_OK } else { AMBER_WARN }),
                );
                ui.label(
                    egui::RichText::new(format!(
                        "Boot Guard Daemon: {}",
                        if self.tweaks.boot_guard_installed { "Active (exits after 12s)" } else { "Not Installed" }
                    ))
                    .size(13.0)
                    .color(TEXT_PRIMARY),
                );
                ui.label(
                    egui::RichText::new(format!(
                        "Boot Auto-OC: {}",
                        if self.telemetry.boot_oc_enabled {
                            format!("ON ({}/{} MHz)", self.telemetry.saved_oc_core, self.telemetry.saved_oc_mem)
                        } else {
                            "OFF".into()
                        }
                    ))
                    .size(13.0)
                    .color(TEXT_PRIMARY),
                );
                ui.add_space(12.0);
                ui.horizontal_wrapped(|ui| {
                    if ui.add(Self::pill_primary("1-Click Gaming Boost")).clicked() {
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
                        self.refresh_all(false);
                        self.set_status(
                            format!("Applied {} gaming tweaks & killed {} bloat process(es).", logs.len(), rep.killed),
                            false,
                        );
                    }
                    if ui.add(Self::pill_secondary("Kill AMD Bloat")).clicked() {
                        let rep = debloat::strip_live_bloat();
                        self.refresh_all(false);
                        self.set_status(
                            format!("Killed {} process(es), disabled {} service(s).", rep.killed, rep.services_disabled),
                            false,
                        );
                    }
                });
            });
        });

        ui.add_space(14.0);
        Self::glass_card().show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("RBridge App Installation:").strong().color(TEXT_PRIMARY));
                if self.tweaks.gui_installed {
                    Self::badge_pill(ui, "INSTALLED IN PROGRAM FILES", egui::Color32::from_rgb(16, 65, 52), MINT_OK);
                    if ui.add(Self::pill_secondary("Remove Start Menu Shortcut")).clicked() {
                        match tweaks::uninstall_rbridge_gui() {
                            Ok(m) => self.set_status(m, false),
                            Err(e) => self.set_status(e, true),
                        }
                        self.refresh_all(false);
                    }
                } else {
                    Self::badge_pill(ui, "PORTABLE MODE", CARD_ELEVATED, TEXT_MUTED);
                    if ui.add(Self::pill_secondary("Install RBridge GUI to Start Menu")).clicked() {
                        match tweaks::install_rbridge_gui() {
                            Ok(m) => self.set_status(m, false),
                            Err(e) => self.set_status(e, true),
                        }
                        self.refresh_all(false);
                    }
                }
            });
        });
    }

    fn render_patcher(&mut self, ui: &mut egui::Ui) {
        ui.heading(egui::RichText::new("Dual-Driver Patcher & Installer").size(24.0).strong().color(TEXT_PRIMARY));
        ui.label(egui::RichText::new("Supports AMD Radeon PRO 21.Q2.1 and Amernime Zone / R.ID 23.9.1 iCafe packages.").color(TEXT_MUTED));
        ui.add_space(12.0);

        Self::glass_card().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Driver Folder:").strong().color(TEXT_PRIMARY));
                ui.add(
                    egui::TextEdit::singleline(&mut self.pkg_dir)
                        .desired_width(430.0)
                        .hint_text(r"Select extracted 23.9.1-iCafe or 21.Q2.1 folder..."),
                );
                if ui.add(Self::pill_secondary("Browse")).clicked() {
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                        self.pkg_dir = folder.to_string_lossy().into_owned();
                        self.detected_pkg = Some(patcher::detect_driver_package(&folder));
                        self.selected_inf_idx = 0;
                    }
                }
                if ui.add(Self::pill_secondary("Scan")).clicked() && !self.pkg_dir.trim().is_empty() {
                    let p = PathBuf::from(self.pkg_dir.trim());
                    self.detected_pkg = Some(patcher::detect_driver_package(&p));
                    self.selected_inf_idx = 0;
                }
            });

            let mut rid_inf_names: Option<Vec<String>> = None;
            if let Some(pkg) = &self.detected_pkg {
                ui.add_space(8.0);
                let ok = pkg.kind != DriverPackageKind::Unknown;
                Self::badge_pill(
                    ui,
                    &pkg.label,
                    if ok { egui::Color32::from_rgb(16, 65, 52) } else { egui::Color32::from_rgb(76, 22, 28) },
                    if ok { MINT_OK } else { CORAL_ERR },
                );
                if pkg.kind == DriverPackageKind::AmernimeRid && !pkg.inf_paths.is_empty() {
                    rid_inf_names = Some(
                        pkg.inf_paths
                            .iter()
                            .map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned())
                            .collect(),
                    );
                }
            }
            if let Some(names) = rid_inf_names {
                if self.selected_inf_idx >= names.len() {
                    self.selected_inf_idx = 0;
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Target INF Profile:").strong().color(TEXT_PRIMARY));
                    egui::ComboBox::from_id_salt("rid_inf_picker")
                        .width(340.0)
                        .selected_text(&names[self.selected_inf_idx])
                        .show_ui(ui, |ui| {
                            for (idx, name) in names.iter().enumerate() {
                                ui.selectable_value(&mut self.selected_inf_idx, idx, name);
                            }
                        });
                    ui.label(egui::RichText::new("(gcnstd = Consumer OverDrive)").size(11.5).color(ELECTRIC_CYAN));
                });
            }
        });

        ui.add_space(12.0);
        Self::glass_card().show(ui, |ui| {
            Self::toggle_row(
                ui,
                &mut self.patch_opts.strip_amd_bloat,
                "Strip Background AMD Processes",
                "Removes AMDRSServ, RadeonSoftware, atieclxx, Crash Defender, telemetry & ccc2_install",
            );
            Self::toggle_row(
                ui,
                &mut self.patch_opts.keep_msi_supported,
                "Enable PCIe MSI Interrupts (MSISupported = 1)",
                "Lowest Enduro DPC latency; disable only if experiencing audio pops",
            );
            Self::toggle_row(
                ui,
                &mut self.patch_opts.rebrand_r8_m670,
                "Radeon R8 M670 Display Branding (21.Q2.1 Only)",
                "Clean custom branding without touching R.ID custom PCI tables",
            );
            Self::toggle_row(
                ui,
                &mut self.patch_opts.install_driver,
                "Install Patched Driver Automatically (pnputil)",
                "Signs local .CAT catalog (Zero-WDK) and deploys immediately",
            );
            Self::toggle_row(
                ui,
                &mut self.patch_opts.install_gui,
                "Install RBridge Control Center GUI",
                "Uncheck for pure Driver-Only Mode with no installed app",
            );
            Self::toggle_row(
                ui,
                &mut self.patch_opts.install_boot_guard,
                "Install Silent 12s Boot Guard Daemon",
                "Re-enforces tweaks & OC at logon, then exits to 0 MB RAM",
            );

            ui.add_space(8.0);
            let can_run = !self.busy
                && self
                    .detected_pkg
                    .as_ref()
                    .map(|p| p.kind != DriverPackageKind::Unknown)
                    .unwrap_or(false);
            if ui.add_enabled(can_run, Self::pill_primary("Patch, Sign & Install")).clicked() {
                if let Some(pkg) = self.detected_pkg.clone() {
                    let opts = self.patch_opts.clone();
                    let sel_idx = if pkg.kind == DriverPackageKind::AmernimeRid {
                        Some(self.selected_inf_idx)
                    } else {
                        None
                    };
                    let (tx, rx) = mpsc::channel();
                    self.worker_rx = Some(rx);
                    self.busy = true;
                    self.patch_logs.clear();
                    self.set_status("Running RBridge driver pipeline...", false);
                    thread::spawn(move || {
                        let res = patcher::run_patch_pipeline(&pkg, &opts, sel_idx, |line| {
                            let _ = tx.send(WorkerMsg::Log(line));
                        });
                        let _ = tx.send(WorkerMsg::Done(res));
                    });
                }
            }
        });

        ui.add_space(12.0);
        Self::glass_card().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("PIPELINE LOG").size(11.0).strong().color(ELECTRIC_CYAN));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(Self::pill_secondary("Clear").min_size(egui::vec2(64.0, 26.0)))
                        .clicked()
                    {
                        self.patch_logs.clear();
                        self.set_status("Cleared pipeline log.", false);
                    }
                    if ui
                        .add(Self::pill_secondary("Export .txt").min_size(egui::vec2(96.0, 26.0)))
                        .clicked()
                    {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_file_name("rbridge_pipeline_log.txt")
                            .add_filter("Text Log", &["txt"])
                            .save_file()
                        {
                            let body = self.patch_logs.join("\r\n");
                            match std::fs::write(&path, body) {
                                Ok(_) => self.set_status(
                                    format!("Exported log to {}", path.display()),
                                    false,
                                ),
                                Err(e) => self.set_status(format!("Export failed: {}", e), true),
                            }
                        }
                    }
                    if ui
                        .add(Self::pill_primary("Copy Log").min_size(egui::vec2(92.0, 26.0)))
                        .clicked()
                    {
                        let body = self.patch_logs.join("\r\n");
                        ui.ctx().copy_text(body);
                        self.set_status(
                            format!("Copied {} log line(s) to clipboard.", self.patch_logs.len()),
                            false,
                        );
                    }
                });
            });
            ui.add_space(6.0);
            let mut log_text = self.patch_logs.join("\n");
            egui::ScrollArea::vertical()
                .max_height(180.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut log_text)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .desired_rows(9),
                    );
                });
        });
    }

    fn render_performance(&mut self, ui: &mut egui::Ui) {
        ui.heading(egui::RichText::new("Performance & Latency Tuning").size(24.0).strong().color(TEXT_PRIMARY));
        ui.add_space(12.0);

        Self::glass_card().show(ui, |ui| {
            Self::toggle_row(ui, &mut self.tweaks.ulps_disabled, "Disable ULPS (Ultra Low Power State)", "Eliminates hybrid GPU wake stutters and clock drops");
            Self::toggle_row(ui, &mut self.tweaks.stutter_mode_off, "Disable StutterMode & Enable KMD Large Pages", "Smoother 1% lows on 64-bit DDR3 Oland VRAM");
            Self::toggle_row(ui, &mut self.tweaks.mpo_disabled, "Disable Windows MPO (OverlayTestMode = 5)", "Fixes DX11/Vulkan hybrid frame-pacing & black screens");
            Self::toggle_row(ui, &mut self.tweaks.gamedvr_disabled, "Disable Xbox GameDVR", "Stops background capture overhead");
            Self::toggle_row(ui, &mut self.tweaks.flip_model_enabled, "Force DX10 / DX11 Flip Model", "Reduces presentation latency in windowed/borderless games");
            Self::toggle_row(ui, &mut self.tweaks.tessellation_4x, "Cap Tessellation at 4x (GCN 1.0 Boost)", "Prevents geometry bottlenecks in DX11 titles");
            Self::toggle_row(ui, &mut self.tweaks.msi_mode_enabled, "PCIe MSI Mode + High Interrupt Priority", "Direct packet-based interrupts for VEN_1002");
            Self::toggle_row(ui, &mut self.tweaks.ac_cores_unparked, "Unpark CPU Cores (100%) & Disable PCIe ASPM on AC", "Keeps i5-4200U and PCIe link responsive under load");

            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                if ui.add(Self::pill_primary("Apply All Tweaks")).clicked() {
                    let logs = tweaks::apply_tweaks(&self.tweaks);
                    self.refresh_all(false);
                    self.set_status(logs.last().cloned().unwrap_or_else(|| "Tweaks applied.".into()), false);
                }
                if self.tweaks.boot_guard_installed {
                    if ui.add(Self::pill_secondary("Remove Boot Guard")).clicked() {
                        match tweaks::remove_boot_guard() {
                            Ok(m) => self.set_status(m, false),
                            Err(e) => self.set_status(e, true),
                        }
                        self.refresh_all(false);
                    }
                } else if ui.add(Self::pill_secondary("Install Boot Guard")).clicked() {
                    match tweaks::install_boot_guard() {
                        Ok(m) => self.set_status(m, false),
                        Err(e) => self.set_status(e, true),
                    }
                    self.refresh_all(false);
                }
            });
        });
    }

    fn render_overclock(&mut self, ui: &mut egui::Ui) {
        ui.heading(egui::RichText::new("Overclock Studio").size(24.0).strong().color(TEXT_PRIMARY));
        ui.label(egui::RichText::new("Direct Live ADL + Unofficial Overclocking Modes (no VBIOS ROM required).").color(TEXT_MUTED));
        ui.add_space(12.0);

        Self::glass_card().show(ui, |ui| {
            ui.label(egui::RichText::new("OVERCLOCK ENGINE MODE").size(11.0).strong().color(ELECTRIC_CYAN));
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                for (mode, label) in [
                    (OcMode::Standard, "Standard ADL"),
                    (OcMode::UnofficialPowerPlay, "Unofficial Mode 1 (With PowerPlay)"),
                    (OcMode::UnofficialFlatClocks, "Unofficial Mode 2 (Flat Clocks)"),
                ] {
                    let selected = self.oc_mode == mode;
                    let btn = egui::Button::new(
                        egui::RichText::new(label)
                            .size(12.5)
                            .strong()
                            .color(if selected { egui::Color32::WHITE } else { TEXT_MUTED }),
                    )
                    .fill(if selected { PRO_COBALT } else { CARD_ELEVATED })
                    .stroke(egui::Stroke::new(1.0_f32, if selected { ELECTRIC_CYAN } else { GLASS_BORDER }))
                    .rounding(egui::Rounding::same(16.0_f32))
                    .min_size(egui::vec2(140.0, 32.0));
                    if ui.add(btn).clicked() {
                        self.oc_mode = mode;
                    }
                }
            });

            ui.add_space(10.0);
            if let Some(d) = &self.adl_diag {
                ui.label(
                    egui::RichText::new(format!(
                        "{}  |  OD_Caps(ver={}, sup={}, en={})  |  Active: {}/{} MHz",
                        d.backend, d.od_version, d.od_supported, d.od_enabled, d.cur_core_mhz, d.cur_mem_mhz
                    ))
                    .monospace()
                    .size(12.0)
                    .color(ELECTRIC_CYAN),
                );
                ui.label(egui::RichText::new(&d.states_summary).monospace().size(11.5).color(TEXT_MUTED));
            } else {
                ui.label(egui::RichText::new("Click 'Wake & Refresh' in the top bar to read active AMD ADL states.").color(AMBER_WARN));
            }

            ui.add_space(12.0);
            let (max_c, max_m) = if self.oc_mode == OcMode::Standard {
                (
                    self.adl_diag.as_ref().map(|d| d.max_core_mhz.max(1100)).unwrap_or(1100),
                    self.adl_diag.as_ref().map(|d| d.max_mem_mhz.max(1000)).unwrap_or(1000),
                )
            } else {
                (1250, 1200)
            };

            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Core Engine Clock: ").strong().color(TEXT_PRIMARY));
                ui.add(egui::Slider::new(&mut self.target_core_mhz, 750..=max_c).suffix(" MHz"));
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("DDR3 Memory Clock: ").strong().color(TEXT_PRIMARY));
                ui.add(egui::Slider::new(&mut self.target_mem_mhz, 750..=max_m).suffix(" MHz"));
            });
            ui.add_space(10.0);

            Self::toggle_row(
                ui,
                &mut self.boot_auto_oc,
                "Auto-Apply Overclock on Windows Boot",
                "Re-applies selected mode silently via RBridge Boot Guard (0 MB RAM)",
            );

            ui.horizontal_wrapped(|ui| {
                if ui.add(Self::pill_primary("Apply Live Overclock")).clicked() {
                    match powerplay::apply_live_adl_oc(self.target_core_mhz, self.target_mem_mhz, self.oc_mode) {
                        Ok((c, m, api)) => {
                            powerplay::save_live_oc_profile(self.boot_auto_oc, self.oc_mode, c, m);
                            self.set_status(format!("APPLIED via {}: {} MHz Core / {} MHz Mem", api, c, m), false);
                            self.refresh_all(true);
                        }
                        Err(e) => self.set_status(e, true),
                    }
                }
                if ui.add(Self::pill_secondary("Unlock Registry & Reload GPU")).clicked() {
                    let _ = powerplay::unlock_unofficial_registry_flags();
                    let (tx, rx) = mpsc::channel();
                    self.worker_rx = Some(rx);
                    self.busy = true;
                    self.set_status("Unlocked PP_EnableOverdrive & reloading AMD GPU...", false);
                    thread::spawn(move || {
                        let _ = tx.send(WorkerMsg::Done(powerplay::reload_amd_gpu()));
                    });
                }
                if ui.add(Self::pill_secondary("Sync Afterburner Mode 2")).clicked() {
                    match powerplay::sync_msi_afterburner_unofficial_cfg() {
                        Ok(m) => self.set_status(m, false),
                        Err(e) => self.set_status(e, true),
                    }
                }
                if ui.add(Self::pill_secondary("Reset Stock (975/900)")).clicked() {
                    self.target_core_mhz = 975;
                    self.target_mem_mhz = 900;
                    self.boot_auto_oc = false;
                    let _ = powerplay::apply_live_adl_oc(975, 900, OcMode::Standard);
                    let _ = powerplay::restore_factory_powerplay();
                    self.set_status("Restored stock 975 / 900 MHz defaults.", false);
                    self.refresh_all(false);
                }
            });
        });

        ui.add_space(12.0);
        Self::glass_card().show(ui, |ui| {
            ui.label(egui::RichText::new("OPTIONAL FALLBACK: SOFTPOWERPLAYTABLE (.ROM OVERRIDE)").size(11.0).strong().color(TEXT_MUTED));
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                if ui.add(Self::pill_secondary("Load VBIOS .rom")).clicked() {
                    if let Some(path) = rfd::FileDialog::new().add_filter("AMD VBIOS ROM", &["rom", "bin"]).pick_file() {
                        match powerplay::extract_powerplay_from_rom(&path) {
                            Ok(pp) => {
                                self.stock_core_mhz = pp.boost_core_mhz;
                                self.stock_mem_mhz = pp.boost_mem_mhz;
                                self.rom_path_label = format!("Loaded {} ({} bytes)", path.file_name().unwrap_or_default().to_string_lossy(), pp.table_bytes.len());
                                self.parsed_pp = Some(pp);
                                self.set_status("Loaded PowerPlay table from ROM.", false);
                            }
                            Err(e) => self.set_status(e, true),
                        }
                    }
                }
                if ui.add_enabled(self.parsed_pp.is_some(), Self::pill_secondary("Inject Table")).clicked() {
                    if let Some(pp) = &self.parsed_pp {
                        match powerplay::apply_powerplay_oc(&pp.table_bytes, self.stock_core_mhz, self.stock_mem_mhz, self.target_core_mhz, self.target_mem_mhz) {
                            Ok(m) => self.set_status(m, false),
                            Err(e) => self.set_status(e, true),
                        }
                    }
                }
            });
            ui.add_space(4.0);
            ui.label(egui::RichText::new(&self.rom_path_label).size(11.5).color(TEXT_MUTED));
        });
    }

    fn render_display(&mut self, ui: &mut egui::Ui) {
        ui.heading(egui::RichText::new("16:9 Custom Resolution Injector").size(24.0).strong().color(TEXT_PRIMARY));
        ui.label(egui::RichText::new("Adds 1280x720, 1152x648, 1024x576, 960x540, 896x504, and 800x450 @ 60Hz via EDID_OVERRIDE.").color(TEXT_MUTED));
        ui.add_space(12.0);

        let monitors = self.monitors.clone();
        for mon in &monitors {
            Self::glass_card().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(format!("{} ({}x{} Native)", mon.model, mon.native_w, mon.native_h)).size(15.0).strong().color(TEXT_PRIMARY));
                        ui.label(
                            egui::RichText::new(if mon.has_override { "16:9 EDID_OVERRIDE Active" } else { "Factory EDID" })
                                .size(12.0)
                                .color(if mon.has_override { MINT_OK } else { TEXT_MUTED }),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(Self::pill_secondary("Revert EDID")).clicked() {
                            match display::revert_edid(mon) {
                                Ok(m) => self.set_status(m, false),
                                Err(e) => self.set_status(e, true),
                            }
                            self.monitors = display::enumerate_monitors();
                        }
                        if ui.add(Self::pill_primary("Inject 16:9 Modes")).clicked() {
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
        ui.heading(egui::RichText::new("VRAM Quarantine & GPU Pinning").size(24.0).strong().color(TEXT_PRIMARY));
        ui.label(egui::RichText::new("Keep Chrome/Discord/Steam on Intel HD 4400 so games get all 2 GB DDR3 VRAM on AMD HD 8670M.").color(TEXT_MUTED));
        ui.add_space(12.0);

        Self::glass_card().show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.add(Self::pill_primary("Auto-Quarantine Apps -> Intel")).clicked() {
                    match quarantine::quarantine_background_apps() {
                        Ok(n) => self.set_status(format!("Pinned {} background app(s) to Intel HD 4400.", n), false),
                        Err(e) => self.set_status(e, true),
                    }
                    self.pins = quarantine::list_pinned_apps();
                }
                if ui.add(Self::pill_secondary("Pin Game (.exe) -> AMD")).clicked() {
                    if let Some(path) = rfd::FileDialog::new().add_filter("Game Executable", &["exe"]).pick_file() {
                        match quarantine::pin_executable(&path.to_string_lossy(), true) {
                            Ok(m) => self.set_status(m, false),
                            Err(e) => self.set_status(e, true),
                        }
                        self.pins = quarantine::list_pinned_apps();
                    }
                }
            });
        });

        ui.add_space(12.0);
        Self::glass_card().show(ui, |ui| {
            ui.label(egui::RichText::new("ACTIVE GPU ASSIGNMENTS").size(11.0).strong().color(ELECTRIC_CYAN));
            ui.add_space(6.0);
            let pins = self.pins.clone();
            egui::ScrollArea::vertical().max_height(310.0).show(ui, |ui| {
                for entry in &pins {
                    ui.horizontal(|ui| {
                        Self::badge_pill(
                            ui,
                            if entry.high_performance { "AMD 8670M" } else { "INTEL 4400" },
                            if entry.high_performance { egui::Color32::from_rgb(19, 64, 122) } else { egui::Color32::from_rgb(16, 65, 52) },
                            if entry.high_performance { ELECTRIC_CYAN } else { MINT_OK },
                        );
                        ui.label(egui::RichText::new(&entry.exe_path).size(12.0).color(TEXT_PRIMARY));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Remove").clicked() {
                                let _ = quarantine::remove_pin(&entry.exe_path);
                                self.pins = quarantine::list_pinned_apps();
                            }
                        });
                    });
                    ui.add_space(4.0);
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
                    .inner_margin(egui::Margin::same(20.0_f32)),
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

fn apply_modern_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = CANVAS_BG;
    visuals.window_fill = CARD_BG;
    visuals.extreme_bg_color = egui::Color32::from_rgb(7, 10, 15);
    visuals.faint_bg_color = CARD_ELEVATED;
    visuals.selection.bg_fill = PRO_COBALT;
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, ELECTRIC_CYAN);
    visuals.widgets.noninteractive.bg_fill = CARD_BG;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, GLASS_BORDER);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, TEXT_PRIMARY);
    visuals.widgets.inactive.bg_fill = CARD_ELEVATED;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, GLASS_BORDER);
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(30, 42, 66);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, ELECTRIC_CYAN);
    visuals.widgets.active.bg_fill = PRO_COBALT;
    visuals.window_rounding = egui::Rounding::same(14.0_f32);
    ctx.set_visuals(visuals);
}

pub fn launch_gui() -> eframe::Result {
    let icon = Arc::new(egui::IconData {
        rgba: ICON_RGBA.to_vec(),
        width: 64,
        height: 64,
    });

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 710.0])
            .with_min_inner_size([900.0, 580.0])
            .with_icon(icon)
            .with_title("RBridge — Radeon Control Center"),
        ..Default::default()
    };

    eframe::run_native(
        "RBridge — Radeon Control Center",
        options,
        Box::new(|cc| {
            apply_modern_theme(&cc.egui_ctx);
            Ok(Box::new(RBridgeApp::new(cc)))
        }),
    )
}
