//! Colour, duration and time formatting helpers.

const C16: [u8; 6] = [6, 2, 3, 4, 5, 1];
const C256: [u8; 75] = [
    20, 21, 26, 27, 32, 33, 38, 39, 40, 41, 42, 43, 44, 45, 56, 57, 62, 63, 68, 69, 74, 75, 76, 77, 78, 79, 80, 81, 92,
    93, 98, 99, 112, 113, 129, 134, 135, 148, 149, 160, 161, 162, 163, 164, 165, 166, 167, 168, 169, 170, 171, 172,
    173, 178, 179, 184, 185, 196, 197, 198, 199, 200, 201, 202, 203, 204, 205, 206, 207, 208, 209, 214, 215, 220, 221,
];

/// A stable colour for a logger name (the hash of `Logger.code`).
pub fn name_color(name: &str, colors: u8) -> u8 {
    let mut hash: i32 = 0;
    for unit in name.encode_utf16() {
        hash = (hash << 3).wrapping_sub(hash).wrapping_add(i32::from(unit)).wrapping_add(13);
    }
    let palette: &[u8] = match colors {
        0 => return 0,
        1 => &C16,
        _ => &C256,
    };
    palette[(hash.unsigned_abs() as usize) % palette.len()]
}

pub fn color(colors: u8, code: u8, value: &str, decoration: &str) -> String {
    if colors == 0 {
        return value.to_string();
    }
    let code = if code < 8 { code.to_string() } else { format!("8;5;{code}") };
    let decoration = if colors >= 2 { decoration } else { "" };
    format!("\u{1b}[3{code}{decoration}m{value}\u{1b}[0m")
}

/// `1500` → `2s`, `61000` → `1m` (cosmokit's `Time.format`).
pub fn format_duration(ms: i64) -> String {
    const SECOND: i64 = 1000;
    const MINUTE: i64 = 60 * SECOND;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    let abs = ms.abs();
    let round = |unit: i64| (ms as f64 / unit as f64).round() as i64;
    if abs >= DAY - HOUR / 2 {
        format!("{}d", round(DAY))
    } else if abs >= HOUR - MINUTE / 2 {
        format!("{}h", round(HOUR))
    } else if abs >= MINUTE - SECOND / 2 {
        format!("{}m", round(MINUTE))
    } else if abs >= SECOND {
        format!("{}s", round(SECOND))
    } else {
        format!("{ms}ms")
    }
}

/// Expands `yyyy MM dd hh mm ss SSS` with the local time.
pub fn time_template(template: &str) -> String {
    let now = chrono::Local::now();
    [("yyyy", "%Y"), ("yy", "%y"), ("MM", "%m"), ("dd", "%d"), ("hh", "%H"), ("mm", "%M"), ("ss", "%S"), ("SSS", "%3f")]
        .iter()
        .fold(template.to_string(), |result, (token, spec)| result.replace(token, &now.format(spec).to_string()))
}
