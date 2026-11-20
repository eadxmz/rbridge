use std::borrow::Cow;
use std::io;
use winreg::enums::*;
use winreg::{RegKey, RegValue};

const DISPLAY_ENUM: &str = r"SYSTEM\CurrentControlSet\Enum\DISPLAY";
const EDID_HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

// Exact 16:9 resolutions whose widths are multiples of 8 (valid EDID 1.3/1.4 Standard Timings)
const CANDIDATE_16_9: [(u16, u16); 7] = [
    (1600, 900),
    (1280, 720),
    (1152, 648),
    (1024, 576),
    (960, 540),
    (896, 504),
    (800, 450),
];

pub struct DisplayTarget {
    pub model: String,
    pub dev_params_path: String,
    pub native_w: u16,
    pub native_h: u16,
    pub refresh_hz: u8,
    pub active: bool,
    pub has_override: bool,
    pub edid_raw: Vec<u8>,
}

fn parse_native_timing(edid: &[u8]) -> (u16, u16, u8) {
    if edid.len() < 128 {
        return (0, 0, 60);
    }
    let h_active = edid[56] as u16 | (((edid[58] >> 4) as u16) << 8);
    let v_active = edid[59] as u16 | (((edid[61] >> 4) as u16) << 8);
    let h_blank = edid[57] as u64 | (((edid[58] & 0x0F) as u64) << 8);
    let v_blank = edid[60] as u64 | (((edid[61] & 0x0F) as u64) << 8);
    let pclk_10khz = u16::from_le_bytes([edid[54], edid[55]]) as u64;

    let h_tot = h_active as u64 + h_blank;
    let v_tot = v_active as u64 + v_blank;
    let hz = if pclk_10khz > 0 && h_tot > 0 && v_tot > 0 {
        let calc = ((pclk_10khz * 10_000) as f64 / (h_tot * v_tot) as f64).round() as u32;
        calc.clamp(60, 120) as u8
    } else {
        60
    };
    (h_active, v_active, hz)
}

pub fn list_displays() -> Vec<DisplayTarget> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let Ok(disp_root) = hklm.open_subkey_with_flags(DISPLAY_ENUM, KEY_READ) else {
        return Vec::new();
    };

    let mut targets = Vec::new();
    for model in disp_root.enum_keys().filter_map(|k| k.ok()) {
        let Ok(model_key) = disp_root.open_subkey_with_flags(&model, KEY_READ) else {
            continue;
        };
        for inst in model_key.enum_keys().filter_map(|k| k.ok()) {
            let inst_path = format!(r"{}\{}\{}", DISPLAY_ENUM, model, inst);
            let dev_params_path = format!(r"{}\Device Parameters", inst_path);
            let Ok(dp_key) = hklm.open_subkey_with_flags(&dev_params_path, KEY_READ) else {
                continue;
            };
            let Ok(edid_val) = dp_key.get_raw_value("EDID") else {
                continue;
            };
            let edid_raw = edid_val.bytes.into_owned();
            if edid_raw.len() < 128 || edid_raw[..8] != EDID_HEADER {
                continue;
            }

            let (native_w, native_h, refresh_hz) = parse_native_timing(&edid_raw);
            if native_w == 0 || native_h == 0 {
                continue;
            }

            let active = hklm
                .open_subkey_with_flags(format!(r"{}\Control", inst_path), KEY_READ)
                .is_ok();
            let has_override = dp_key
                .open_subkey_with_flags("EDID_OVERRIDE", KEY_READ)
                .is_ok();

            targets.push(DisplayTarget {
                model: model.clone(),
                dev_params_path,
                native_w,
                native_h,
                refresh_hz,
                active,
                has_override,
                edid_raw,
            });
        }
    }

    // Show active displays first
    targets.sort_by(|a, b| b.active.cmp(&a.active).then(a.model.cmp(&b.model)));
    targets
}

pub fn inject_16_9_modes(target: &DisplayTarget) -> io::Result<Vec<(u16, u16)>> {
    let mut block0 = [0u8; 128];
    block0.copy_from_slice(&target.edid_raw[..128]);

    // Ensure EDID >= 1.3 so aspect ratio bits 11b strictly decode as 16:9
    if block0[18] == 1 && block0[19] < 3 {
        block0[19] = 3;
    }

    let hz_bits = target.refresh_hz.saturating_sub(60) & 0x3F;
    let modes_to_add: Vec<(u16, u16)> = CANDIDATE_16_9
        .iter()
        .copied()
        .filter(|&(w, _)| w < target.native_w)
        .collect();

    if modes_to_add.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Native panel width is already <= 800px.",
        ));
    }

    // Collect existing valid Standard Timings (bytes 38..54 = 8 slots)
    let mut slots: Vec<[u8; 2]> = Vec::new();
    for s in 0..8 {
        let pair = [block0[38 + s * 2], block0[39 + s * 2]];
        if pair != [0x01, 0x01] && pair != [0x00, 0x00] {
            slots.push(pair);
        }
    }

    // Append our 16:9 lower-res modes (prioritizing them if slots exceed 8)
    let mut injected = Vec::new();
    for &(w, h) in &modes_to_add {
        let b0 = ((w / 8) - 31) as u8;
        let b1 = 0xC0u8 | hz_bits; // 11b = 16:9 aspect ratio
        let pair = [b0, b1];
        if !slots.contains(&pair) {
            if slots.len() >= 8 {
                slots.remove(0);
            }
            slots.push(pair);
        }
        injected.push((w, h));
    }

    while slots.len() < 8 {
        slots.push([0x01, 0x01]);
    }
    for (s, pair) in slots.iter().take(8).enumerate() {
        block0[38 + s * 2] = pair[0];
        block0[39 + s * 2] = pair[1];
    }

    // Recalculate EDID Block 0 checksum
    let sum = block0[..127].iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    block0[127] = 0u8.wrapping_sub(sum);

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let dp_key = hklm.open_subkey_with_flags(&target.dev_params_path, KEY_WRITE)?;
    let (ovr_key, _) = dp_key.create_subkey("EDID_OVERRIDE")?;

    ovr_key.set_raw_value(
        "0",
        &RegValue {
            bytes: Cow::Borrowed(&block0),
            vtype: REG_BINARY,
        },
    )?;

    // Preserve any existing CEA/DisplayID extension blocks verbatim
    let ext_blocks = target.edid_raw.len() / 128;
    for blk in 1..ext_blocks {
        let start = blk * 128;
        let slice = &target.edid_raw[start..start + 128];
        ovr_key.set_raw_value(
            &blk.to_string(),
            &RegValue {
                bytes: Cow::Borrowed(slice),
                vtype: REG_BINARY,
            },
        )?;
    }

    Ok(injected)
}

pub fn remove_override(target: &DisplayTarget) -> io::Result<()> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let dp_key = hklm.open_subkey_with_flags(&target.dev_params_path, KEY_WRITE)?;
    match dp_key.delete_subkey_all("EDID_OVERRIDE") {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}
