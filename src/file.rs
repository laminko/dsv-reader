//! What the window needs besides drawing: the settings file, the font files, Save and Export.
//! Copied from the egui app's `main.rs` (step 2 of the move); that copy goes in step 6.

use crate::dsv::{ENCODINGS, Scan};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const SIZES: RangeInclusive<f32> = 8.0..=32.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Theme {
    /// Follows macOS light or dark mode.
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    pub const ALL: [Theme; 3] = [Theme::System, Theme::Light, Theme::Dark];

    pub fn name(self) -> &'static str {
        match self {
            Theme::System => "System",
            Theme::Light => "Light",
            Theme::Dark => "Dark",
        }
    }
}

/// The grid font, the Viewer pane, and the theme, in ~/Library/Application Support/dsv-reader/
/// settings.txt: the same file and keys as the egui app.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// A font file, or None for the built-in font.
    pub font: Option<PathBuf>,
    pub size: f32,
    /// The Viewer pane is below the grid, not on its right. The Profile is always on the right.
    pub pane_bottom: bool,
    /// The width of the panes on the right, and the height of the Viewer below the grid.
    pub pane_size: f32,
    pub pane_height: f32,
    /// The upper pane's share of the side pane when both panes show.
    pub split: f32,
    pub theme: Theme,
    /// Every second row of the grid on a tint.
    pub zebra: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { font: None, size: 13.0, pane_bottom: false, pane_size: 360.0, pane_height: 280.0, split: 0.5, theme: Theme::System, zebra: true }
    }
}

impl Settings {
    fn path() -> PathBuf {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Library/Application Support/dsv-reader/settings.txt")
    }

    /// `font=PATH`, `size=N`, `pane=right|bottom`, `pane_size=N`, `pane_height=N`, `split=0.15–0.85`,
    /// `theme=system|light|dark`, and `zebra=on|off` lines.
    /// A line it cannot read keeps the default.
    pub fn parse(text: &str) -> Settings {
        let mut s = Settings::default();
        let mut height = false;
        for line in text.lines() {
            match line.split_once('=') {
                Some(("font", p)) if !p.is_empty() => s.font = Some(PathBuf::from(p)),
                Some(("size", n)) => {
                    if let Ok(n) = n.trim().parse::<f32>() {
                        s.size = n.clamp(*SIZES.start(), *SIZES.end());
                    }
                }
                Some(("pane", p)) => s.pane_bottom = p.trim() == "bottom",
                Some(("zebra", z)) => s.zebra = z.trim() != "off",
                Some(("theme", t)) => {
                    s.theme = match t.trim() {
                        "light" => Theme::Light,
                        "dark" => Theme::Dark,
                        _ => Theme::System,
                    }
                }
                Some(("pane_size", n)) => {
                    if let Ok(n) = n.trim().parse::<f32>() {
                        s.pane_size = n.clamp(120.0, 4000.0);
                    }
                }
                Some(("pane_height", n)) => {
                    height = true;
                    if let Ok(n) = n.trim().parse::<f32>() {
                        s.pane_height = n.clamp(120.0, 4000.0);
                    }
                }
                Some(("split", n)) => {
                    if let Ok(n) = n.trim().parse::<f32>().map(|n| n.clamp(0.15, 0.85)) {
                        s.split = n;
                    }
                }
                _ => {}
            }
        }
        // A file older than `pane_height` sized the Viewer below with `pane_size`.
        if s.pane_bottom && !height {
            s.pane_height = s.pane_size;
        }
        s
    }

    pub fn text(&self) -> String {
        let font = self.font.as_deref().map_or(String::new(), |p| p.display().to_string());
        let pane = if self.pane_bottom { "bottom" } else { "right" };
        let theme = self.theme.name().to_lowercase();
        let zebra = if self.zebra { "on" } else { "off" };
        format!("font={font}\nsize={}\npane={pane}\npane_size={}\npane_height={}\nsplit={}\ntheme={theme}\nzebra={zebra}\n", self.size, self.pane_size, self.pane_height, self.split)
    }

    pub fn load() -> Settings {
        std::fs::read_to_string(Self::path()).map_or_else(|_| Settings::default(), |t| Settings::parse(&t))
    }

    pub fn save(&self) {
        let p = Self::path();
        let r = p.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(&p, self.text()));
        if let Err(e) = r {
            eprintln!("settings not saved: {}: {e}", p.display());
        }
    }
}

/// The .ttf, .otf, and .ttc files in the macOS font folders, by name.
pub fn font_files() -> Vec<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let mut dirs = vec![PathBuf::from("/System/Library/Fonts"), PathBuf::from("/Library/Fonts"), home.join("Library/Fonts")];
    let mut out = Vec::new();
    while let Some(d) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for p in entries.flatten().map(|e| e.path()) {
            let ext = p.extension().and_then(|x| x.to_str()).map(str::to_ascii_lowercase);
            if p.is_dir() {
                dirs.push(p);
            } else if matches!(ext.as_deref(), Some("ttf" | "otf" | "ttc")) {
                out.push(p);
            }
        }
    }
    out.sort_by_key(|p| font_name(p).to_lowercase());
    out
}

pub fn font_name(p: &Path) -> String {
    p.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned())
}

/// The family name in a TrueType or OpenType file: name ID 16, else 1, of the first font.
pub fn family(b: &[u8]) -> Option<String> {
    let u16_at = |i: usize| Some(u16::from_be_bytes(b.get(i..i + 2)?.try_into().ok()?));
    let u32_at = |i: usize| Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?));
    let base = if b.get(..4)? == b"ttcf" { u32_at(12)? as usize } else { 0 };
    let tables = u16_at(base + 4)? as usize;
    let name = (0..tables).map(|t| base + 12 + 16 * t).find(|&r| b.get(r..r + 4) == Some(b"name"))?;
    let off = u32_at(name + 8)? as usize;
    let (count, strings) = (u16_at(off + 2)? as usize, off + u16_at(off + 4)? as usize);
    // Apple's own fonts often name themselves only for the Mac platform (1), in 8-bit text.
    let mut found: Option<(u16, String)> = None;
    for i in 0..count {
        let r = off + 6 + 12 * i;
        let (platform, id, len, at) = (u16_at(r)?, u16_at(r + 6)?, u16_at(r + 8)? as usize, u16_at(r + 10)? as usize);
        let raw = b.get(strings + at..strings + at + len)?;
        let s = match (platform, id) {
            (0 | 3, 1 | 16) => String::from_utf16(&raw.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect::<Vec<_>>()).ok()?,
            (1, 1 | 16) => raw.iter().map(|&c| c as char).collect(),
            _ => continue,
        };
        // Name ID 16 before 1, Unicode before Mac.
        let rank = (id == 1) as u16 * 2 + (platform == 1) as u16;
        if found.as_ref().is_none_or(|f| rank < f.0) {
            found = Some((rank, s));
        }
    }
    found.map(|f| f.1)
}

/// Size and modified time of a file: what Save compares to find a change on disk.
#[derive(Clone, Debug, PartialEq)]
pub struct Stamp {
    pub len: u64,
    pub modified: Option<SystemTime>,
}

pub fn stamp(p: &Path) -> std::io::Result<Stamp> {
    let m = std::fs::metadata(p)?;
    Ok(Stamp { len: m.len(), modified: m.modified().ok() })
}

/// The record delimiter of a file as text.
pub fn eol_text(eol: &str) -> &'static str {
    match eol {
        "CRLF" => "\r\n",
        "CR" => "\r",
        _ => "\n",
    }
}

/// An Export or Save on its thread. `Ok` holds a message and the file to read again.
pub type Done = Result<(String, Option<PathBuf>), String>;

/// Writes `cols` of `scan` (rows `view`, or every data row in file order) to `target` through a
/// temp file in the same folder and a rename: a failed write leaves the old file, and a mapped
/// old file stays readable.
pub fn write_file(target: &Path, scan: &Scan, view: Option<&[u32]>, cols: &[usize], fmt: (&str, &str, &str)) -> Result<(), String> {
    let name = target.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
    let tmp = target.with_file_name(format!(".{name}.dsv-reader-tmp"));
    let written = (|| {
        let mut w = BufWriter::new(File::create(&tmp)?);
        scan.write(&mut w, view, cols, fmt.0, fmt.1, fmt.2)?;
        w.flush()?;
        drop(w);
        std::fs::rename(&tmp, target)
    })();
    written.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Save: every data row of `scan`, the columns `cols`, in the file's format `fmt`, to `name` in
/// the folder of `source`. The file's own name overwrites it. Refuses a name with a folder part,
/// a new name in use, and a source whose size or modified time is no longer `read`.
pub fn save(source: &Path, read: &Stamp, name: &str, scan: &Scan, cols: &[usize], fmt: (&str, &str, &str)) -> Result<PathBuf, String> {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
        return Err(format!("\"{name}\" is not a file name"));
    }
    let target = source.with_file_name(name);
    if target == source {
        if stamp(source).ok().as_ref() != Some(read) {
            return Err("The file changed on disk after it was read. Open it again first.".into());
        }
    } else if target.exists() {
        return Err(format!("{name} is already in use"));
    }
    write_file(&target, scan, None, cols, fmt).map_err(|e| format!("Save failed: {e}"))?;
    Ok(target)
}

pub fn delim_name(d: &str) -> String {
    match d {
        "," => "Comma".into(),
        "\t" => "Tab".into(),
        ";" => "Semicolon".into(),
        "|" => "Pipe".into(),
        _ => format!("{d:?}"),
    }
}

pub fn quote_name(q: &str) -> &'static str {
    match q {
        "\"" => "Double \"",
        "'" => "Single '",
        _ => "None",
    }
}

pub fn enc_name(label: &str) -> &'static str {
    ENCODINGS.iter().find(|e| e.0 == label).map_or("UTF-8", |e| e.1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsv::{Doc, Opts};
    use std::sync::atomic::Ordering;

    #[test]
    fn settings_round_trip_and_bad_lines() {
        let s = Settings { font: Some("/System/Library/Fonts/Menlo.ttc".into()), size: 15.5, pane_bottom: true, pane_size: 280.0, pane_height: 333.0, split: 0.3, theme: Theme::Dark, zebra: false };
        assert_eq!(Settings::parse(&s.text()), s);
        assert_eq!(Settings::parse(""), Settings::default());
        assert_eq!(Settings::parse("size=big\nfont=\nx"), Settings::default());
        assert_eq!(Settings::parse("size=99").size, 32.0);
        assert_eq!(Settings::parse("theme=light").theme, Theme::Light);
        assert_eq!(Settings::parse("theme=purple").theme, Theme::System);
        assert_eq!([Settings::parse("zebra=off").zebra, Settings::parse("zebra=on").zebra, Settings::parse("zebra=x").zebra], [false, true, true]);
        assert_eq!([Settings::parse("split=2").split, Settings::parse("split=0").split, Settings::parse("split=x").split], [0.85, 0.15, 0.5]);
    }

    #[test]
    fn pane_height_is_its_own_setting() {
        // The Viewer below the grid keeps its height; the right panes keep their width.
        let s = Settings::parse("pane=bottom\npane_size=413\npane_height=250");
        assert_eq!((s.pane_bottom, s.pane_size, s.pane_height), (true, 413.0, 250.0));
        assert_eq!(Settings::parse("pane_size=413").pane_height, 280.0, "an older file, the Viewer right: the default height");
        assert_eq!(Settings::parse("pane=bottom\npane_size=413").pane_height, 413.0, "an older file, the Viewer below: its old size");
        assert_eq!(Settings::parse("pane=bottom\npane_size=413\npane_height=x").pane_height, 280.0, "a line it cannot read: the default");
        assert_eq!([Settings::parse("pane_height=1").pane_height, Settings::parse("pane_height=x").pane_height], [120.0, 280.0]);
    }

    #[test]
    fn family_of_a_font_file() {
        // Menlo ships with macOS as a collection; its first font is Menlo Regular.
        let b = std::fs::read("/System/Library/Fonts/Menlo.ttc").unwrap();
        assert_eq!(family(&b).as_deref(), Some("Menlo"));
        assert_eq!(family(include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf")).as_deref(), Some("IBM Plex Sans"));
        assert_eq!(family(b"not a font"), None);
    }

    #[test]
    fn save_writes_through_a_temp_file_and_refuses_what_it_must() {
        let dir = std::env::temp_dir().join(format!("dsv-reader-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("a.csv");
        std::fs::write(&src, "name;note\r\nAda;'x;y'\r\nBo;z\r\n").unwrap();
        let opts = Opts { delim: Some(";".into()), quote: "'".into(), header: Some(true), ..Opts::default() };
        let doc = Doc::open(&src, &opts, || {}).unwrap();
        while !doc.index.done.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        let (scan, read, fmt) = (doc.scan().unwrap(), stamp(&src).unwrap(), (";", "\r\n", "'"));
        // A new name, note first: the qualifier and the record delimiter of the file stay.
        let b = save(&src, &read, "b.csv", &scan, &[1, 0], fmt).unwrap();
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "note;name\r\n'x;y';Ada\r\nz;Bo\r\n");
        assert!(save(&src, &read, "b.csv", &scan, &[1, 0], fmt).unwrap_err().contains("in use"));
        assert!(save(&src, &read, "x/b.csv", &scan, &[0], fmt).unwrap_err().contains("not a file name"));
        // Overwrite: refused after a change on disk; done when nothing changed.
        let changed = Stamp { len: read.len + 1, ..read.clone() };
        assert!(save(&src, &changed, "a.csv", &scan, &[0], fmt).unwrap_err().contains("changed"));
        save(&src, &read, "a.csv", &scan, &[0], fmt).unwrap();
        assert_eq!(std::fs::read_to_string(&src).unwrap(), "name\r\nAda\r\nBo\r\n");
        assert_eq!(doc.row(1), ["Bo", "z"], "the open file still reads its old bytes");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2, "no temp file is left");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
