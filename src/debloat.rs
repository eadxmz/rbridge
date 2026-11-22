use std::os::windows::process::CommandExt;
use std::process::Command;

const CREATE_NO_WINDOW: u32 = 0x08000000;

pub const BLOAT_PROCESSES: &[&str] = &[
    "AMDRSServ.exe",
    "RadeonSoftware.exe",
    "atieclxx.exe",
    "atiesrxx.exe",
    "AMDRSSrcExt.exe",
    "amdow.exe",
    "amddvr.exe",
    "cncmd.exe",
    "amdfendrsr.exe",
    "AMDInstallManager.exe",
    "AUEPMaster.exe",
    "AUEPLauncher.exe",
];

pub const BLOAT_SERVICES: &[&str] = &[
    "AMD External Events Utility",
    "AMD Crash Defender Service",
    "AMD User Experience Program",
    "AMDRSServ",
    "AUEPLauncher",
    "atiesrxx",
];

pub const BLOAT_TASKS: &[&str] = &[
    "ModifyLinkUpdate",
    "StartCN",
    "StartDVR",
    "AMDLinkUpdate",
    "AUEP",
];

pub struct DebloatReport {
    pub killed: usize,
    pub services_disabled: usize,
    pub tasks_disabled: usize,
    pub logs: Vec<String>,
}

pub fn count_running_bloat() -> usize {
    let Ok(out) = Command::new("tasklist")
        .args(["/FO", "CSV", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return 0;
    };
    let text = String::from_utf8_lossy(&out.stdout).to_ascii_lowercase();
    BLOAT_PROCESSES
        .iter()
        .filter(|p| text.contains(&format!("\"{}\"", p.to_ascii_lowercase())))
        .count()
}

pub fn strip_live_bloat() -> DebloatReport {
    let mut report = DebloatReport {
        killed: 0,
        services_disabled: 0,
        tasks_disabled: 0,
        logs: Vec::new(),
    };

    for proc_name in BLOAT_PROCESSES {
        if let Ok(out) = Command::new("taskkill")
            .args(["/F", "/IM", proc_name])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            if out.status.success() {
                report.killed += 1;
                report.logs.push(format!("[KILL] Terminated {}", proc_name));
            }
        }
    }

    for svc in BLOAT_SERVICES {
        let _ = Command::new("sc")
            .args(["stop", svc])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        if let Ok(out) = Command::new("sc")
            .args(["config", svc, "start=", "disabled"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            if out.status.success() {
                report.services_disabled += 1;
                report
                    .logs
                    .push(format!("[SVC] Disabled service '{}'", svc));
            }
        }
    }

    for task in BLOAT_TASKS {
        if let Ok(out) = Command::new("schtasks")
            .args(["/Change", "/TN", task, "/DISABLE"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            if out.status.success() {
                report.tasks_disabled += 1;
                report
                    .logs
                    .push(format!("[TASK] Disabled scheduled task '{}'", task));
            }
        }
    }

    report
}
