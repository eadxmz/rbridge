use winreg::enums::*;
use winreg::{RegKey, RegValue};

#[derive(Clone, Debug)]
pub struct MonitorEntry {
    pub model: String,
    pub native_w: u16,
    pub native_h: u16,
    pub reg_path: String,
    pub base_edid: Vec<u8>,
    pub has_override: bool,
}

pub fn enumerate_monitors() -> Vec<MonitorEntry> {
    let mut results = Vec::new();
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let Ok(disp) = hklm.open_subkey_with_flags(r"SYSTEM\CurrentControlSet\Enum\DISPLAY", KEY_READ)
    else {
        return results;
    };

    for model in disp.enum_keys().filter_map(|k| k.ok()) {
        if model.eq_ignore_ascii_case("Default_Monitor") {
            continue;
        }
        let mpath = format!(r"SYSTEM\CurrentControlSet\Enum\DISPLAY\{}", model);
        let Ok(mkey) = hklm.open_subkey_with_flags(&mpath, KEY_READ) else {
            continue;
        };
        for inst in mkey.enum_keys().filter_map(|k| k.ok()) {
            let dp_path = format!(r"{}\{}\Device Parameters", mpath, inst);
            let Ok(dp) = hklm.open_subkey_with_flags(&dp_path, KEY_READ) else {
                continue;
            };
            let Ok(edid_val) = dp.get_raw_value("EDID") else {
                continue;
            };
            if edid_val.bytes.len() < 128 {
                continue;
            }
            let edid = &edid_val.bytes;
            let h_active = (edid[56] as u16) | (((edid[58] as u16) & 0xF0) << 4);
            let v_active = (edid[59] as u16) | (((edid[61] as u16) & 0xF0) << 4);
            let has_override = dp.open_subkey("EDID_OVERRIDE").is_ok();

            results.push(MonitorEntry {
                model: model.clone(),
                native_w: if h_active > 0 { h_active } else { 1366 },
                native_h: if v_active > 0 { v_active } else { 768 },
                reg_path: dp_path,
                base_edid: edid[..128].to_vec(),
                has_override,
            });
            break;
        }
    }
    results
}

fn encode_dtd_60hz(w: u16, h: u16) -> [u8; 18] {
    // CVT-RB 60Hz timing block
    let h_blank: u16 = 160;
    let v_blank: u16 = 30;
    let h_sync_off: u16 = 48;
    let h_sync_w: u16 = 32;
    let v_sync_off: u16 = 3;
    let v_sync_w: u16 = 6;
    let pclk_10khz = (((w as u32 + h_blank as u32) * (h as u32 + v_blank as u32) * 60) / 10_000) as u16;

    let mut d = [0u8; 18];
    d[0] = (pclk_10khz & 0xFF) as u8;
    d[1] = (pclk_10khz >> 8) as u8;
    d[2] = (w & 0xFF) as u8;
    d[3] = (h_blank & 0xFF) as u8;
    d[4] = (((w >> 8) & 0x0F) << 4) as u8 | ((h_blank >> 8) & 0x0F) as u8;
    d[5] = (h & 0xFF) as u8;
    d[6] = (v_blank & 0xFF) as u8;
    d[7] = (((h >> 8) & 0x0F) << 4) as u8 | ((v_blank >> 8) & 0x0F) as u8;
    d[8] = (h_sync_off & 0xFF) as u8;
    d[9] = (h_sync_w & 0xFF) as u8;
    d[10] = (((v_sync_off & 0x0F) << 4) | (v_sync_w & 0x0F)) as u8;
    d[11] = ((((h_sync_off >> 8) & 0x03) << 6)
        | (((h_sync_w >> 8) & 0x03) << 4)
        | (((v_sync_off >> 4) & 0x03) << 2)
        | ((v_sync_w >> 4) & 0x03)) as u8;
    d[12] = 0x58;
    d[13] = 0xC1;
    d[14] = 0x10;
    d[17] = 0x1E;
    d
}

fn encode_std_16_9(w: u16) -> [u8; 2] {
    let b0 = ((w / 8).saturating_sub(31)) as u8;
    // Bits 7..6 = 11 (16:9), Bits 5..0 = 0 (60Hz)
    let b1 = 0xC0u8;
    [b0, b1]
}

pub fn inject_16_9_modes(mon: &MonitorEntry) -> Result<String, String> {
    let mut edid = mon.base_edid.clone();
    if edid.len() < 128 {
        return Err("Base EDID is invalid.".into());
    }

    // Fill Standard Timing slots (bytes 38..54, 8 slots of 2 bytes) with crisp 16:9 modes
    let std_widths: [u16; 6] = [1280, 1152, 1024, 960, 896, 800];
    for (idx, &w) in std_widths.iter().enumerate() {
        let off = 38 + idx * 2;
        let pair = encode_std_16_9(w);
        edid[off] = pair[0];
        edid[off + 1] = pair[1];
    }

    // Preserve native DTD at 54..72; write 1280x720, 1024x576, 896x504 into DTD slots 1..3
    let dtd_modes: [(u16, u16); 3] = [(1280, 720), (1024, 576), (896, 504)];
    for (idx, &(w, h)) in dtd_modes.iter().enumerate() {
        let off = 72 + idx * 18;
        let block = encode_dtd_60hz(w, h);
        edid[off..off + 18].copy_from_slice(&block);
    }

    // Recalculate EDID checksum (byte 127)
    let sum: u32 = edid[..127].iter().map(|&b| b as u32).sum();
    edid[127] = ((256 - (sum % 256)) % 256) as u8;

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let override_path = format!(r"{}\EDID_OVERRIDE", mon.reg_path);
    let (ov_key, _) = hklm
        .create_subkey(&override_path)
        .map_err(|e| format!("Failed creating EDID_OVERRIDE (run as Admin): {}", e))?;

    ov_key
        .set_raw_value(
            "0",
            &RegValue {
                bytes: edid,
                vtype: REG_BINARY,
            },
        )
        .map_err(|e| format!("Failed writing EDID_OVERRIDE: {}", e))?;

    Ok(format!(
        "Injected 16:9 modes (1280x720, 1152x648, 1024x576, 960x540, 896x504, 800x450) into {}. Reboot to activate.",
        mon.model
    ))
}

pub fn revert_edid(mon: &MonitorEntry) -> Result<String, String> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let dp = hklm
        .open_subkey_with_flags(&mon.reg_path, KEY_READ | KEY_WRITE)
        .map_err(|e| format!("Failed opening display registry key (run as Admin): {}", e))?;
    let _ = dp.delete_subkey_all("EDID_OVERRIDE");
    Ok(format!(
        "Removed EDID_OVERRIDE for {}. Reboot to restore factory modes.",
        mon.model
    ))
}
