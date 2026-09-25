//! Plain-text rendering: aligned tables, the hub's sources table, the pairing QR code and
//! small value formatters.

use hfa_audio::meter::SILENCE_DB;
use hfa_core::SourceInfo;
use qrcode::render::unicode::Dense1x2;
use qrcode::{EcLevel, QrCode};

/// Longest text shown in a table cell (longer values are cut with `…`).
const MAX_CELL: usize = 48;

/// A left-aligned plain-text table with a header row.
#[derive(Debug, Clone)]
pub struct Table {
    rows: Vec<Vec<String>>,
}

impl Table {
    /// A table with these column headers.
    pub fn new<const N: usize>(header: [&str; N]) -> Self {
        Self {
            rows: vec![header.iter().map(|h| (*h).to_owned()).collect()],
        }
    }

    /// Appends a row (cells are cut to [`MAX_CELL`] characters).
    pub fn row<const N: usize>(&mut self, cells: [String; N]) {
        self.rows
            .push(cells.iter().map(|c| truncate(c, MAX_CELL)).collect());
    }

    /// The table as text, one line per row, columns separated by two spaces.
    pub fn render(&self) -> String {
        let columns = self.rows.iter().map(Vec::len).max().unwrap_or(0);
        let widths: Vec<usize> = (0..columns)
            .map(|c| {
                self.rows
                    .iter()
                    .filter_map(|r| r.get(c))
                    .map(|s| s.chars().count())
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let mut out = String::new();
        for row in &self.rows {
            let mut line = String::new();
            for (c, cell) in row.iter().enumerate() {
                if c > 0 {
                    line.push_str("  ");
                }
                line.push_str(cell);
                let pad = widths[c].saturating_sub(cell.chars().count());
                line.extend(std::iter::repeat_n(' ', pad));
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }
}

/// Cuts `s` to at most `max` characters, ending with `…` when cut.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// A level in dBFS, or `silent` at/below [`SILENCE_DB`] + 1.
pub fn level(db: f32) -> String {
    if !db.is_finite() || db <= SILENCE_DB + 1.0 {
        "silent".to_owned()
    } else {
        format!("{db:.1} dB")
    }
}

/// A duration in ms, without decimals from 10 ms up.
pub fn millis(ms: f32) -> String {
    if !ms.is_finite() {
        "-".to_owned()
    } else if ms < 10.0 {
        format!("{ms:.1} ms")
    } else {
        format!("{ms:.0} ms")
    }
}

/// A percentage with one decimal.
pub fn percent(pct: f32) -> String {
    if pct.is_finite() {
        format!("{pct:.1} %")
    } else {
        "-".to_owned()
    }
}

/// The hub's sources as a table (device, label, gain, muted, prio, loss, jitter, buffer,
/// latency, level, state).
pub fn sources_table(sources: &[SourceInfo]) -> String {
    if sources.is_empty() {
        return "No sources yet. Start `hfa send --to <this host>` on another device.\n".to_owned();
    }
    let mut table = Table::new([
        "DEVICE", "LABEL", "GAIN", "MUTED", "PRIO", "LOSS", "JITTER", "BUFFER", "LATENCY", "LEVEL",
        "STATE",
    ]);
    let flag = |b: bool| if b { "yes" } else { "-" }.to_owned();
    for s in sources {
        table.row([
            s.device_name.clone(),
            s.label.clone(),
            format!("{:.2}", s.gain),
            flag(s.muted),
            flag(s.priority),
            percent(s.stats.loss_pct),
            millis(s.stats.jitter_ms),
            millis(s.stats.buffer_ms),
            millis(s.stats.latency_ms),
            level(s.stats.level_db),
            if s.active { "active" } else { "idle" }.to_owned(),
        ]);
    }
    table.render()
}

/// The QR code of `data` drawn with Unicode half blocks (two modules per character row),
/// inverted for terminals with a dark background (scanners read both polarities). `None`
/// if `data` does not fit in a QR code.
pub fn qr_text(data: &str) -> Option<String> {
    let code = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::L).ok()?;
    Some(
        code.render::<Dense1x2>()
            .dark_color(Dense1x2::Light)
            .light_color(Dense1x2::Dark)
            .quiet_zone(true)
            .build(),
    )
}

/// `YYYY-MM-DD HH:MM` (UTC) of a unix time in seconds.
pub fn format_unix_time(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(i64::MAX / 2);
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60
    )
}

/// Proleptic Gregorian date of a day count since 1970-01-01 (H. Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Whether a stream that `is_terminal` understands ANSI escapes (colours, cursor control): a
/// terminal whose `TERM` is not `dumb`; on Windows only a VT-capable console (Windows
/// Terminal sets `WT_SESSION`; MSYS/Cygwin terminals set `TERM`).
pub fn vt_console(is_terminal: bool) -> bool {
    if !is_terminal {
        return false;
    }
    let term = std::env::var_os("TERM");
    if cfg!(windows) {
        std::env::var_os("WT_SESSION").is_some() || term.is_some_and(|t| t != "dumb")
    } else {
        term.is_none_or(|t| t != "dumb")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hfa_core::StreamStats;

    #[test]
    fn table_aligns_columns() {
        let mut t = Table::new(["A", "LONGER"]);
        t.row(["wide cell".to_owned(), "x".to_owned()]);
        assert_eq!(t.render(), "A          LONGER\nwide cell  x\n");
    }

    #[test]
    fn truncates_on_char_boundaries() {
        assert_eq!(truncate("héllo wörld", 5), "héll…");
        assert_eq!(truncate("short", 5), "short");
    }

    #[test]
    fn formats_values() {
        assert_eq!(level(-120.0), "silent");
        assert_eq!(level(-12.34), "-12.3 dB");
        assert_eq!(millis(3.25), "3.2 ms");
        assert_eq!(millis(81.6), "82 ms");
        assert_eq!(percent(1.25), "1.2 %");
        assert_eq!(format_unix_time(0), "1970-01-01 00:00");
        assert_eq!(format_unix_time(1_790_000_000), "2026-09-21 14:13");
        assert_eq!(format_unix_time(951_782_400), "2000-02-29 00:00");
    }

    #[test]
    fn sources_table_lists_every_field() {
        let info = SourceInfo {
            stream_id: 7,
            device_id: "ab12-cd34-ef56-7890".into(),
            device_name: "Laptop".into(),
            label: "System audio".into(),
            platform: "linux".into(),
            gain: 0.5,
            muted: true,
            priority: false,
            active: true,
            stats: StreamStats {
                loss_pct: 2.5,
                jitter_ms: 1.5,
                buffer_ms: 40.0,
                latency_ms: 75.0,
                level_db: -18.0,
            },
        };
        let text = sources_table(&[info]);
        let mut lines = text.lines();
        let header = lines.next().unwrap();
        assert!(header.starts_with("DEVICE"));
        let row = lines.next().unwrap();
        for field in [
            "Laptop",
            "System audio",
            "0.50",
            "yes",
            "2.5 %",
            "1.5 ms",
            "40 ms",
            "75 ms",
            "-18.0 dB",
            "active",
        ] {
            assert!(row.contains(field), "{field:?} missing in {row:?}");
        }
        assert!(sources_table(&[]).contains("No sources"));
    }

    #[test]
    fn qr_code_renders_a_pairing_uri() {
        let uri = "hfa://pair?v=0&h=192.168.1.20&p=47810&id=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&t=abcdefghijklmnopqrstuv&n=Desk";
        let qr = qr_text(uri).expect("fits");
        let lines: Vec<&str> = qr.lines().collect();
        // A square code: character rows hold two module rows each.
        let width = lines[0].chars().count();
        assert!(width >= 21 + 8, "width {width}");
        assert!(lines.len() * 2 >= width && lines.len() * 2 <= width + 1);
        assert!(lines.iter().all(|l| l.chars().count() == width));
        assert!(qr_text(&"x".repeat(5000)).is_none());
    }
}
