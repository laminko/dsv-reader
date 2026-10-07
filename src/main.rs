//! The window, on gpui-kit: toolbar B, the grid, the side pane, and the status bar.
//!
//! dsv-reader [PATH]
//! DSV_BENCH=1 prints open → first rows, and quits when the index is done.
//! DSV_SCROLL=1 with it first scrolls one page a frame for 300 frames (DSV_FRAMES sets more),
//! in a 1600 × 1000 window, as the egui app does, and prints the draw time.
//! DSV_SEARCH=text then searches and prints the scan time and rows; DSV_GOTO=row goes to a row.
//! DSV_THEME=light|dark starts in that theme without saving it, for screenshots; DSV_SIZE=WxH sets the window size.
//! DSV_VIEWER=0 starts with the Record viewer closed; DSV_HOLD=1 keeps the window open after the bench report.
//! DSV_OPEN=parse|scope|columns|font|save opens that popover or the Save dialog, for screenshots.
//! DSV_PROFILE=1000|all|auto opens the Profile and waits for it; DSV_DISTINCT=exact|approx sets its choice.
//! It profiles every column; DSV_CARDS=screen profiles only the cards on screen, as the app does.
//! DSV_TOP=column then filters on that column's most common Profile value, as a click on it does;
//! DSV_TOP=4,8 adds one for each column, joined by and; DSV_TOP=4|8 joins them by or; 9:3 is bar 3
//! of column 9's histogram.
//! DSV_SORT=column sorts on it with the search; DSV_COPY=rows times Copy of the first rows, all
//! columns, as TSV (the clipboard is left alone).
//! DSV_MODIFY=1 hides the last column, so the order is "modified" (to test the quit question).
//! DSV_PICK=path sends a file as Open would; DSV_OPEN_IN=current|new answers "Open in" for it.

mod file;
mod grid;
mod mac;
mod menu;
mod pane;
mod toolbar;

use dsv_reader as dsv;

use dsv::{Dir, Doc, Filter, Kind, Opts, Sel, Show, SortKey, floor_index, predicate, to_tsv};
use file::{Done, Settings, Stamp, Theme, enc_name, eol_text, save, stamp, write_file};
use gpui_kit::base::TextSelection;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, ThemeMode};
use gpui_kit::*;
use grid::{Act, Drag, Grid, Hit, MIN_W, PAD, Tokens, count, fits, sep};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();

actions!(
    dsv_reader,
    [
        OpenFile,
        FocusSearch,
        FocusGoto,
        ExportFile,
        MoveUp,
        MoveDown,
        MoveLeft,
        MoveRight,
        ExtendUp,
        ExtendDown,
        ExtendLeft,
        ExtendRight,
        GridPageUp,
        GridPageDown,
        ExtendPageUp,
        ExtendPageDown,
        SelectAll,
        CopyCells,
        ClearSelection,
        Quit
    ]
);

/// The panes of the side pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    Viewer,
    Profile,
}

/// The Record viewer's tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Cell,
    Record,
}

/// Copy takes this many rows at most; Export takes more.
const COPY_MAX: usize = 1_000_000;

/// Scroll frames; DSV_FRAMES sets more, for a profiler.
fn scroll_frames() -> usize {
    std::env::var("DSV_FRAMES").ok().and_then(|f| f.parse().ok()).unwrap_or(300)
}

/// The grid font: a font file, or Menlo for the built-in font. None: the file cannot be read.
fn grid_font(path: Option<&Path>, cx: &mut App) -> Option<Font> {
    let Some(p) = path else { return Some(font("Menlo")) };
    let bytes = std::fs::read(p).ok()?;
    let name = file::family(&bytes)?;
    cx.text_system().add_fonts(vec![Cow::Owned(bytes)]).ok()?;
    Some(font(name))
}

/// A search on its thread. Setting `stop` ends it early.
struct Job {
    stop: Arc<AtomicBool>,
    started: Instant,
    _task: Task<()>,
}

enum Bench {
    /// Frames waited for the window to be in front: a window behind gets throttled frames.
    Wait(usize),
    Scroll {
        frames: usize,
        last: Option<Instant>,
        gaps: Vec<f64>,
    },
    Done,
}

pub struct Reader {
    pub settings: Settings,
    /// Font files on this Mac, for the font menu.
    pub fonts: Vec<PathBuf>,
    pub grid: Option<Grid>,
    pub opts: Opts,
    /// The "other" delimiter box, the search, and Go to row.
    pub other: Entity<InputState>,
    pub search: Entity<InputState>,
    pub goto: Entity<InputState>,
    /// The Save dialog: 0 overwrites the open file, 1 writes a new file named `save_name`.
    pub save_new: Entity<usize>,
    pub save_name: Entity<InputState>,
    /// Search in these columns; empty: all columns.
    pub scope: Vec<usize>,
    pub sort: Vec<SortKey>,
    /// Filters: a row must match all of them. `filter_rows` holds their text boxes.
    pub filters: Vec<Filter>,
    pub filter_rows: Vec<pane::FilterRow>,
    pub viewer: bool,
    pub profile: bool,
    /// The panes that are on, in the order they were turned on: the first one stays on top.
    pub panes: Vec<Pane>,
    pub tab: Tab,
    pub prof: pane::Prof,
    /// A pane group's edge is being dragged: below the grid or not, the mouse, and the size at
    /// the press.
    pub pane_drag: Option<(bool, f32, f32)>,
    /// The line between the two panes is being dragged: the mouse and the split at the press.
    pub split_drag: Option<(f32, f32)>,
    /// The side pane's height in the last frame.
    pub pane_h: Rc<Cell<f32>>,
    /// The longest time between two frames while a profile ran, in ms.
    prof_gap: f64,
    last_frame: Option<Instant>,
    job: Option<Job>,
    /// The search changed: scan again once the index is done.
    dirty: bool,
    pub scan_ms: Option<f64>,
    stamp: Option<Stamp>,
    writing: Option<Task<()>>,
    /// A Save wrote over the open file: read it again.
    reload: Option<PathBuf>,
    pub note: Option<String>,
    pub error: Option<String>,
    /// The open popover, by id.
    pub pop: Option<&'static str>,
    /// A choice menu is open (its popover then ignores the press that lands on it).
    pub menu_open: bool,
    /// The popover a press outside closed, and when.
    pub pop_closed: Option<(&'static str, Instant)>,
    /// DSV_OPEN=save: the Save dialog waits for the first rows.
    ask_save_first: bool,
    /// Files that wait for the "Open in" answer, oldest first.
    pub ask_open: VecDeque<PathBuf>,
    /// The quit question shows, or was answered with Quit.
    pub asking_quit: bool,
    pub quit_ok: bool,
    /// DSV_PICK: a file that arrives as Open would send it, after the first rows.
    pick: Option<PathBuf>,
    focus: FocusHandle,
    /// The grid has the keys: arrows, Page Up and Down, ⌘A, ⌘C, Esc.
    grid_focus: FocusHandle,
    _subs: Vec<Subscription>,
    opened: Option<Instant>,
    pub first: Option<f64>,
    /// The end of the first paint with rows.
    drew: Rc<Cell<Option<Instant>>>,
    bench: Option<Bench>,
    bench_searched: bool,
    bench_goto: Option<f64>,
    bench_copy: Option<(f64, usize)>,
    bench_profiled: bool,
    /// DSV_TOP: the value it filtered on.
    bench_top: Option<String>,
    /// Render → end of the grid paint, in ms, one entry a frame.
    draws: Rc<RefCell<Vec<f64>>>,
}

impl Reader {
    fn new(path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Reader {
        let other = cx.new(|cx| InputState::new(window, cx).placeholder("other").validate(|s, _| s.chars().count() <= 1));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search all columns"));
        let goto = cx.new(|cx| InputState::new(window, cx).placeholder("Go to row"));
        let save_name = cx.new(|cx| InputState::new(window, cx).placeholder("new-name.csv"));
        let subs = vec![
            cx.subscribe_in(&search, window, |this: &mut Reader, _, e: &InputEvent, _, cx| {
                if matches!(e, InputEvent::Change) {
                    this.dirty = true;
                    this.rescan(cx);
                }
            }),
            cx.subscribe_in(&goto, window, |this: &mut Reader, _, e: &InputEvent, _, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    this.go_to(cx);
                }
            }),
            cx.subscribe_in(&other, window, |this: &mut Reader, st, e: &InputEvent, window, cx| {
                let v = st.read(cx).value().to_string();
                if matches!(e, InputEvent::Change) && !v.is_empty() {
                    this.set_opts(Opts { delim: Some(v), ..this.opts.clone() }, window, cx);
                }
            }),
        ];
        let mut r = Reader {
            settings: Settings::load(),
            fonts: file::font_files(),
            grid: None,
            opts: Opts::default(),
            other,
            search,
            goto,
            save_new: cx.new(|_| 0),
            save_name,
            scope: Vec::new(),
            sort: Vec::new(),
            filters: Vec::new(),
            filter_rows: Vec::new(),
            // The Viewer opens at each launch, as in the egui app.
            viewer: std::env::var("DSV_VIEWER").as_deref() != Ok("0"),
            profile: false,
            panes: if std::env::var("DSV_VIEWER").as_deref() == Ok("0") { Vec::new() } else { vec![Pane::Viewer] },
            tab: Tab::Cell,
            prof: pane::Prof::default(),
            pane_drag: None,
            split_drag: None,
            pane_h: Rc::default(),
            prof_gap: 0.0,
            last_frame: None,
            job: None,
            dirty: false,
            scan_ms: None,
            stamp: None,
            writing: None,
            reload: None,
            note: None,
            error: None,
            pop: std::env::var("DSV_OPEN").ok().and_then(|p| ["parse", "scope", "columns", "font"].into_iter().find(|x| *x == p)),
            ask_save_first: std::env::var("DSV_OPEN").as_deref() == Ok("save"),
            ask_open: VecDeque::new(),
            asking_quit: false,
            quit_ok: false,
            pick: std::env::var_os("DSV_PICK").map(PathBuf::from),
            menu_open: false,
            pop_closed: None,
            focus: cx.focus_handle(),
            grid_focus: cx.focus_handle(),
            _subs: subs,
            opened: None,
            first: None,
            drew: Rc::default(),
            bench: std::env::var_os("DSV_BENCH").is_some().then_some(Bench::Wait(0)),
            bench_searched: false,
            bench_goto: None,
            bench_copy: None,
            bench_profiled: false,
            bench_top: None,
            draws: Rc::default(),
        };
        if let Some(p) = path {
            r.open(p, window, cx);
        }
        r
    }

    /// A new file: the read options and the search go back to their defaults.
    pub fn open(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.opts = Opts::default();
        self.other.update(cx, |s, cx| s.set_value("", window, cx));
        self.search.update(cx, |s, cx| s.set_value("", window, cx));
        self.load(path, window, cx);
    }

    /// Reads the file with the current options. The search columns and the hidden columns clear.
    fn load(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.grid = None; // stops the old index thread
        self.stop_job();
        (self.scope, self.sort, self.filters, self.filter_rows, self.scan_ms, self.note) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), None, None);
        self.prof.stop();
        (self.prof.dirty, self.prof.distinct, self.prof.cols, self.prof.work) = (true, None, None, Default::default());
        self.dirty = true;
        let t = Instant::now();
        match Doc::open(&path, &self.opts, || {}) {
            Ok(doc) => {
                let name = path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
                window.set_window_title(&format!("{name} — DSV Reader"));
                let font = grid_font(self.settings.font.as_deref(), cx).unwrap_or_else(|| font("Menlo"));
                self.grid = Some(Grid::new(doc, font, self.settings.size));
                if let (Some(g), Some(_)) = (&mut self.grid, std::env::var_os("DSV_MODIFY")) {
                    g.fit_columns(); // the columns come with the first paint
                    if let Some(h) = g.hidden.last_mut() {
                        *h = true;
                    }
                }
                self.stamp = stamp(&path).ok();
                (self.opened, self.first, self.error) = (Some(t), None, None);
                self.drew.set(None);
            }
            Err(e) => self.error = Some(format!("{}: {e}", path.display())),
        }
        cx.notify();
    }

    /// New read options: the file is read again with them.
    pub fn set_opts(&mut self, opts: Opts, window: &mut Window, cx: &mut Context<Self>) {
        if opts == self.opts {
            return;
        }
        self.opts = opts;
        if let Some(path) = self.grid.as_ref().map(|g| g.doc.path.clone()) {
            self.load(path, window, cx);
        }
    }

    pub fn pick_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: None });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(p) = paths.into_iter().next()
            {
                this.update_in(cx, |r, window, cx| r.arrive(p, window, cx)).ok();
            }
        })
        .detach();
    }

    /// A file from Open, a drop, or Finder. With a file already open, it waits for "Open in".
    pub fn arrive(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.grid.is_none() && self.ask_open.is_empty() {
            return self.open(path, window, cx);
        }
        self.ask_open.push_back(path);
        if self.ask_open.len() == 1 {
            self.ask_open_in(window, cx);
        }
    }

    /// The "Open in" answer for `path`, the oldest waiting file: this window (false), a new
    /// instance of the app (true), or None to skip it. Then the next waiting file asks.
    pub fn answer_open(&mut self, path: &Path, new: Option<bool>, window: &mut Window, cx: &mut Context<Self>) {
        if self.ask_open.front().map(PathBuf::as_path) != Some(path) {
            return; // answered already
        }
        let path = self.ask_open.pop_front().unwrap_or_default();
        match new {
            Some(false) => self.open(path, window, cx),
            Some(true) => {
                let name = path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
                self.note = Some(match new_instance(&path) {
                    Ok(()) => format!("Opened {name} in a new window"),
                    Err(e) => format!("New window failed: {e}"),
                });
            }
            None => {}
        }
        // After the answered dialog has closed.
        cx.defer_in(window, |r, window, cx| r.ask_open_in(window, cx));
        cx.notify();
    }

    /// Takes the files that Finder sent and a stopped quit. Tells AppKit whether a quit must ask.
    fn take_opened(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for p in std::mem::take(&mut *mac::OPENED.lock().unwrap()) {
            self.arrive(p, window, cx);
        }
        if mac::ASK_QUIT.swap(false, Ordering::Relaxed) {
            self.ask_quit(window, cx);
        }
        mac::MODIFIED.store(self.modified() && !self.quit_ok, Ordering::Relaxed);
    }

    fn stop_job(&mut self) {
        if let Some(j) = self.job.take() {
            j.stop.store(true, Ordering::Relaxed);
        }
    }

    /// Starts a scan for the search, filters, and sort once the index is done. A running scan stops.
    pub fn rescan(&mut self, cx: &mut Context<Self>) {
        let Some(scan) = self.dirty.then(|| self.grid.as_ref()?.doc.scan()).flatten() else { return };
        self.dirty = false;
        self.stop_job();
        let text = self.search.read(cx).value().to_string();
        let filtered = self.filters.iter().any(|f| predicate(f, scan.dtype(f.col), scan.read(f.col)).is_some());
        let grid = self.grid.as_mut().unwrap();
        if text.is_empty() && !filtered && self.sort.is_empty() {
            self.prof.dirty |= self.prof.filters && grid.view.is_some();
            (grid.view, grid.mark, self.scan_ms) = (None, None, None);
            return cx.notify();
        }
        let (stop, scope, filters, sort) = (Arc::new(AtomicBool::new(false)), self.scope.clone(), self.filters.clone(), self.sort.clone());
        let s = stop.clone();
        let found = cx.background_spawn(async move {
            let hits = scan.query(&text, &scope, &filters, &s);
            if sort.is_empty() || s.load(Ordering::Relaxed) { hits } else { scan.sort(&hits, &sort) }
        });
        let task = cx.spawn(async move |this, cx| {
            let hits = found.await;
            this.update(cx, |r, cx| {
                let Some(job) = r.job.take_if(|j| !j.stop.load(Ordering::Relaxed)) else { return };
                r.scan_ms = Some(job.started.elapsed().as_secs_f64() * 1e3);
                if let Some(g) = &mut r.grid {
                    (g.view, g.mark, g.sel) = (Some(Arc::new(hits)), None, None);
                    g.top.set(0.0);
                }
                r.prof.dirty |= r.prof.filters;
                cx.notify();
            })
            .ok();
        });
        self.job = Some(Job { stop, started: Instant::now(), _task: task });
        cx.notify();
    }

    /// Moves a row number to the top and marks it. In searched rows, the nearest row above it.
    fn go_to(&mut self, cx: &mut Context<Self>) {
        let sorted = !self.sort.is_empty();
        let Some(g) = &mut self.grid else { return };
        let text = self.goto.read(cx).value().trim().replace(',', "");
        let total = g.doc.rows();
        let Some(r) = text.parse::<usize>().ok().filter(|&r| r >= 1 && r <= total) else {
            self.note = Some(format!("Go to row: type a number from 1 to {}", sep(total)));
            return cx.notify();
        };
        let pos = match &g.view {
            None => Some(r - 1),
            Some(v) if !sorted => floor_index(v, (r - 1) as u32),
            Some(v) => v.iter().position(|&x| x as usize == r - 1),
        };
        let Some(pos) = pos else {
            self.note = Some(format!("Row {} is not in the rows you see", sep(r)));
            return cx.notify();
        };
        g.top.set(pos as f64);
        g.mark = Some(g.view.as_ref().map_or(pos, |v| v[pos] as usize));
        g.sel = Some(Sel::at(pos, g.sel.map_or(0, |s| s.ac)));
        self.note = None;
        cx.notify();
    }

    pub fn set_font(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        let Some(f) = grid_font(path.as_deref(), cx) else {
            self.note = Some(format!("{}: this font cannot be read", path.unwrap_or_default().display()));
            return cx.notify();
        };
        self.settings.font = path;
        self.settings.save();
        if let Some(g) = &mut self.grid {
            g.set_font(f, self.settings.size);
        }
        cx.notify();
    }

    pub fn set_size(&mut self, size: f32, cx: &mut Context<Self>) {
        self.settings.size = size.clamp(*file::SIZES.start(), *file::SIZES.end());
        self.settings.save();
        if let Some(g) = &mut self.grid {
            let f = g.font.clone();
            g.set_font(f, self.settings.size);
        }
        cx.notify();
    }

    pub fn set_zebra(&mut self, on: bool, cx: &mut Context<Self>) {
        self.settings.zebra = on;
        self.settings.save();
        cx.notify();
    }

    pub fn set_theme(&mut self, theme: Theme, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.theme = theme;
        self.settings.save();
        apply_theme(theme, window, cx);
        cx.notify();
    }

    /// The column order or the hidden columns differ from the file.
    pub fn modified(&self) -> bool {
        self.grid.as_ref().is_some_and(|g| g.order.iter().enumerate().any(|(i, &c)| i != c) || g.hidden.contains(&true))
    }

    /// Click: this column ascending, then descending, then off. Shift+click: add it as the
    /// next level, then descending, then remove it.
    pub fn click_sort(&mut self, c: usize, shift: bool, cx: &mut Context<Self>) {
        let at = self.sort.iter().position(|k| k.col == c);
        match (shift, at) {
            (false, Some(0)) if self.sort.len() == 1 => {
                if self.sort[0].dir == Dir::Asc {
                    self.sort[0].dir = Dir::Desc;
                } else {
                    self.sort.clear();
                }
            }
            (false, _) => self.sort = vec![SortKey { col: c, dir: Dir::Asc }],
            (true, None) => self.sort.push(SortKey { col: c, dir: Dir::Asc }),
            (true, Some(p)) => {
                if self.sort[p].dir == Dir::Asc {
                    self.sort[p].dir = Dir::Desc;
                } else {
                    self.sort.remove(p);
                }
            }
        }
        self.dirty = true;
        self.rescan(cx);
    }

    /// The selection as TSV, with or without the column names, or a note why not.
    fn copy_text(&self, names: bool) -> Result<(String, usize), String> {
        let Some(g) = &self.grid else { return Err("No file".into()) };
        let Some(s) = g.sel else { return Err("Select cells to copy".into()) };
        let (top, _, bottom, _) = s.bounds();
        let rows = bottom - top + 1;
        if rows > COPY_MAX {
            return Err(format!("Copy takes {} rows at most", sep(COPY_MAX)));
        }
        Ok((to_tsv(&g.doc.cells(g.view.as_deref().map(Vec::as_slice), &g.shown(), s, names)), rows))
    }

    pub fn copy(&mut self, names: bool, cx: &mut Context<Self>) {
        self.note = Some(match self.copy_text(names) {
            Ok((text, rows)) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                format!("Copied {}", count(rows, "row"))
            }
            Err(e) => e,
        });
        cx.notify();
    }

    /// A double-click on a column edge: fit to the longest values on screen and the name.
    fn fit(&mut self, c: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(g) = &mut self.grid else { return };
        let l = g.layout.borrow().clone();
        let (first, n) = (l.top as usize, g.len());
        let rows: Vec<usize> = (first..(first + l.vis as usize + 1).min(n)).map(|r| g.view.as_ref().map_or(r, |v| v[r] as usize)).collect();
        let ts = window.text_system();
        let width = |s: &str, f: &Font, size: f32| {
            let run = TextRun { len: s.len(), font: f.clone(), color: black(), background_color: None, underline: None, strikethrough: None };
            f32::from(ts.shape_line(SharedString::from(s.to_owned()), px(size), &[run], None).width())
        };
        let cells = g.doc.longest(&rows, c, 10).iter().map(|s| width(s, &g.font, g.size)).fold(0.0, f32::max);
        let head_font = Font { weight: FontWeight::SEMIBOLD, ..font("IBM Plex Sans") };
        let name = width(&g.doc.names[c], &head_font, l.head) + l.head + 4.0;
        g.widths[c] = (cells.max(name) + 2.0 * PAD).clamp(MIN_W, 800.0);
        cx.notify();
    }

    /// The column menu's type: the column reads again as that type, and a scan sorts and
    /// filters again with it.
    pub fn set_kind(&mut self, c: usize, kind: Kind, cx: &mut Context<Self>) {
        let Some(g) = &mut self.grid else { return };
        // Between frames nothing else holds the document: the last paint has dropped its copy.
        let Some(doc) = Rc::get_mut(&mut g.doc) else {
            self.note = Some("The grid is busy; try again".into());
            return cx.notify();
        };
        doc.set_kind(c, kind);
        let ty = doc.dtypes[c];
        g.fmts[c].kind = kind;
        if !fits(&g.fmts[c].show, ty) {
            g.fmts[c].show = Show::Plain;
        }
        self.dirty = true;
        self.prof.dirty = true;
        self.rescan(cx);
        cx.notify();
    }

    pub fn set_show(&mut self, c: usize, show: Show, cx: &mut Context<Self>) {
        if let Some(f) = self.grid.as_mut().and_then(|g| g.fmts.get_mut(c)) {
            f.show = show;
        }
        cx.notify();
    }

    /// A key moves the active cell. Text dragged in the Viewer is cleared: ⌘C then copies the new cell.
    fn step(&mut self, dr: isize, dc: isize, extend: bool, window: &mut Window, cx: &mut Context<Self>) {
        TextSelection::clear(window, cx);
        if let Some(g) = &mut self.grid {
            let dr = if dr.abs() == 2 { dr.signum() * g.page() } else { dr };
            g.step(dr, dc, extend);
        }
        cx.notify();
    }

    /// The grid, its mouse and keys, and the column menu.
    fn grid_view(&mut self, t: Tokens, cx: &mut Context<Self>) -> AnyElement {
        let menu = self.column_menu(t, cx);
        let (sort, drew) = (self.sort.clone(), self.drew.clone());
        // Zebra rows off: their fill is the background, and the paint skips it.
        let t = if self.settings.zebra { t } else { Tokens { zebra: t.bg, ..t } };
        let Some(g) = &mut self.grid else { return div().into_any_element() };
        let canvas = g.canvas(t, &sort, drew);
        let cursor = match (g.on_edge, g.drag) {
            (true, _) => CursorStyle::ResizeLeftRight,
            (_, Some(Drag::Move { moving: true, .. })) => CursorStyle::ClosedHand,
            _ => CursorStyle::Arrow,
        };
        let up = |r: &mut Reader, e: &MouseUpEvent, _: &mut Window, cx: &mut Context<Reader>| {
            if let Some(g) = &mut r.grid {
                g.release(e.modifiers.shift);
            }
            cx.notify();
        };
        div()
            .id("grid")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .track_focus(&self.grid_focus)
            .key_context("Grid")
            .cursor(cursor)
            .on_scroll_wheel(cx.listener(|r, e: &ScrollWheelEvent, _, cx| {
                if let Some(g) = &mut r.grid {
                    g.wheel(e);
                }
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|r, e: &MouseDownEvent, window, cx| {
                    r.grid_focus.focus(window, cx);
                    match r.grid.as_mut().and_then(|g| g.press(e)) {
                        Some(Act::Sort(c, shift)) => r.click_sort(c, shift, cx),
                        Some(Act::Fit(c)) => r.fit(c, window, cx),
                        None => {}
                    }
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|r, e: &MouseDownEvent, _, cx| {
                    let Some(g) = &mut r.grid else { return };
                    let hit = g.layout.borrow().hit(e.position);
                    if let Hit::Head(k) | Hit::Sort(k) | Hit::Edge(k) = hit {
                        g.menu = Some((g.shown()[k], e.position));
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(|r, e: &MouseMoveEvent, _, cx| {
                if r.grid.as_mut().is_some_and(|g| g.moved(e)) {
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(up))
            .on_action(cx.listener(|r, _: &MoveUp, window, cx| r.step(-1, 0, false, window, cx)))
            .on_action(cx.listener(|r, _: &MoveDown, window, cx| r.step(1, 0, false, window, cx)))
            .on_action(cx.listener(|r, _: &MoveLeft, window, cx| r.step(0, -1, false, window, cx)))
            .on_action(cx.listener(|r, _: &MoveRight, window, cx| r.step(0, 1, false, window, cx)))
            .on_action(cx.listener(|r, _: &ExtendUp, window, cx| r.step(-1, 0, true, window, cx)))
            .on_action(cx.listener(|r, _: &ExtendDown, window, cx| r.step(1, 0, true, window, cx)))
            .on_action(cx.listener(|r, _: &ExtendLeft, window, cx| r.step(0, -1, true, window, cx)))
            .on_action(cx.listener(|r, _: &ExtendRight, window, cx| r.step(0, 1, true, window, cx)))
            .on_action(cx.listener(|r, _: &GridPageUp, window, cx| r.step(-2, 0, false, window, cx)))
            .on_action(cx.listener(|r, _: &GridPageDown, window, cx| r.step(2, 0, false, window, cx)))
            .on_action(cx.listener(|r, _: &ExtendPageUp, window, cx| r.step(-2, 0, true, window, cx)))
            .on_action(cx.listener(|r, _: &ExtendPageDown, window, cx| r.step(2, 0, true, window, cx)))
            .on_action(cx.listener(|r, _: &SelectAll, window, cx| {
                TextSelection::clear(window, cx);
                if let Some(g) = &mut r.grid {
                    g.select_all();
                }
                cx.notify();
            }))
            // Text dragged in the Viewer copies as text: a field press gives the grid the focus, so ⌘C comes here.
            .on_action(cx.listener(|r, _: &CopyCells, window, cx| {
                if !TextSelection::has_selection(window, cx) {
                    return r.copy(false, cx);
                }
                let text = TextSelection::selected_text(window, cx);
                r.note = Some(format!("Copied {}", count(text.chars().count(), "character")));
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                cx.notify();
            }))
            .on_action(cx.listener(|r, _: &ClearSelection, window, cx| {
                TextSelection::clear(window, cx);
                if let Some(g) = &mut r.grid {
                    (g.sel, g.menu) = (None, None);
                }
                cx.notify();
            }))
            .child(canvas)
            .children(menu)
            .into_any_element()
    }

    /// Export: the rows you see and the shown columns, as a UTF-8 CSV file. The open file does not change.
    pub fn start_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(g) = &self.grid else { return };
        let Some(scan) = g.doc.scan() else {
            self.note = Some("Export waits for the index".into());
            return cx.notify();
        };
        let source = g.doc.path.clone();
        let stem = source.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
        let dir = source.parent().map(Path::to_path_buf).unwrap_or_default();
        let rx = cx.prompt_for_new_path(&dir, Some(&format!("{stem}-export.csv")));
        let (view, cols) = (g.view.clone(), g.shown());
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            let same = |p: &Path| std::fs::canonicalize(p).ok();
            if same(&path).is_some() && same(&path) == same(&source) {
                this.update(cx, |r, cx| {
                    r.note = Some("Export does not write over the open file. Use Save.".into());
                    cx.notify();
                })
                .ok();
                return;
            }
            let rows = view.as_ref().map_or(scan.rows(), |v| v.len());
            this.update(cx, |r, cx| {
                r.spawn_write(cx, move || {
                    write_file(&path, &scan, view.as_deref().map(Vec::as_slice), &cols, (",", "\n", "\""))
                        .map(|_| (format!("Exported {} to {}", count(rows, "row"), path.display()), None))
                        .map_err(|e| format!("Export failed: {e}"))
                })
            })
            .ok();
        })
        .detach();
    }

    /// Save, from the dialog: overwrite the open file, or write a new file in its folder.
    pub fn start_save(&mut self, cx: &mut Context<Self>) {
        let (Some(g), Some(read)) = (&self.grid, self.stamp.clone()) else { return };
        let Some(scan) = g.doc.scan() else {
            self.note = Some("Save waits for the index".into());
            return cx.notify();
        };
        let source = g.doc.path.clone();
        let own = source.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
        let name = if *self.save_new.read(cx) == 1 { self.save_name.read(cx).value().trim().to_string() } else { own };
        let cols = g.shown();
        let fmt = (g.doc.delim.clone(), eol_text(g.doc.eol), g.doc.quote.clone());
        self.spawn_write(cx, move || {
            let target = save(&source, &read, &name, &scan, &cols, (&fmt.0, fmt.1, &fmt.2))?;
            let reload = (target == source).then(|| target.clone());
            Ok((format!("Saved {}", target.display()), reload))
        });
    }

    /// Runs an Export or Save on a background thread. A Save over the open file reads it again.
    fn spawn_write(&mut self, cx: &mut Context<Self>, work: impl FnOnce() -> Done + Send + 'static) {
        let done = cx.background_spawn(async move { work() });
        self.writing = Some(cx.spawn(async move |this, cx| {
            let done = done.await;
            this.update(cx, |r, cx| {
                r.writing = None;
                match done {
                    Ok((msg, reload)) => (r.note, r.reload) = (Some(msg), reload),
                    Err(e) => r.note = Some(e),
                }
                cx.notify();
            })
            .ok();
        }));
        self.note = Some("Writing…".into());
        cx.notify();
    }

    /// The status bar text.
    pub fn status(&self) -> String {
        if let Some(e) = &self.error {
            return e.clone();
        }
        let Some(g) = &self.grid else { return "No file".into() };
        let doc = &g.doc;
        let done = doc.index.done.load(Ordering::Acquire);
        let total = doc.rows();
        let rows = if !done {
            let at = doc.index.starts.lock().unwrap().last().copied().unwrap_or(0);
            format!("{}, indexing {:.0}%", count(total, "row"), at as f64 * 100.0 / doc.data.len().max(1) as f64)
        } else {
            match &g.view {
                Some(v) if v.len() != total => format!("{} of {}", sep(v.len()), count(total, "row")),
                _ => count(total, "row"),
            }
        };
        let mut parts = vec![
            rows,
            count(doc.names.len(), "column"),
            format!("delimiter {}", file::delim_name(&doc.delim)),
            format!("records {}", doc.eol),
            enc_name(doc.encoding).to_string(),
            format!("header {}: {}", if doc.header { "on" } else { "off" }, doc.reason),
        ];
        if done {
            parts.push(format!("indexed in {} ms", doc.index.ms.load(Ordering::Relaxed)));
            let bad = doc.index.bad.load(Ordering::Relaxed);
            if bad > 0 {
                parts.push(format!("{} could not be read: try another encoding", count(bad, "character")));
            }
        }
        if self.job.is_some() {
            parts.push("scanning…".into());
        } else if let Some(ms) = self.scan_ms {
            parts.push(format!("scan {ms:.0} ms"));
        }
        if let Some(ms) = self.first {
            parts.push(format!("first rows in {ms:.0} ms"));
        }
        if self.modified() {
            parts.push("modified".into());
        }
        if let Some(note) = &self.note {
            parts.push(note.clone());
        }
        parts.join(" · ")
    }

    fn bench(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(step), Some(g)) = (&mut self.bench, &mut self.grid) else { return };
        let done = g.doc.index.done.load(Ordering::Acquire);
        window.request_animation_frame();
        match step {
            Bench::Wait(n) if done && self.first.is_some() && (window.is_window_active() || *n > 300) => {
                if std::env::var_os("DSV_SCROLL").is_some() {
                    self.draws.borrow_mut().clear();
                    g.widths.fill(76.0); // as the egui app's DSV_SCROLL, with the Viewer closed
                    (self.viewer, self.panes) = (false, Vec::new());
                    *step = Bench::Scroll { frames: 0, last: None, gaps: Vec::with_capacity(scroll_frames()) };
                } else {
                    *step = Bench::Done;
                }
            }
            Bench::Wait(n) => {
                *n += 1;
                if *n % 30 == 0 && !window.is_window_active() {
                    window.activate_window();
                }
            }
            Bench::Scroll { frames, last, gaps } => {
                let now = Instant::now();
                if let Some(l) = last.replace(now) {
                    gaps.push((now - l).as_secs_f64() * 1e3);
                }
                if *frames < scroll_frames() {
                    *frames += 1;
                    g.top.set(g.top.get() + g.vis.get().floor().max(1.0));
                    return;
                }
                let pct = |v: &mut Vec<f64>| {
                    v.sort_by(f64::total_cmp);
                    format!("p50 {:.1} · p95 {:.1} · max {:.1} ms", v[v.len() / 2], v[v.len() * 95 / 100], v[v.len() - 1])
                };
                let mut draws = std::mem::take(&mut *self.draws.borrow_mut());
                draws.remove(0); // the frame before the first step
                eprintln!(
                    "bench: scroll {} frames, {} rows a frame, top row {:.0}, window active {} · draw {} · frame gap {}",
                    scroll_frames(),
                    g.vis.get().floor(),
                    g.top.get(),
                    window.is_window_active(),
                    pct(&mut draws),
                    pct(gaps)
                );
                *step = Bench::Done;
            }
            Bench::Done => {}
        }
        if matches!(self.bench, Some(Bench::Done)) {
            self.bench_done(window, cx);
        }
    }

    /// After the scroll: DSV_SEARCH, then DSV_GOTO, then the report, and the window closes.
    fn bench_done(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !std::mem::replace(&mut self.bench_searched, true) {
            let text = std::env::var("DSV_SEARCH").ok();
            let sort = std::env::var("DSV_SORT").ok().and_then(|c| c.parse::<usize>().ok());
            if let Some(text) = &text {
                self.search.update(cx, |s, cx| s.set_value(text.clone(), window, cx));
            }
            if let Some(col) = sort {
                self.sort = vec![SortKey { col, dir: Dir::Asc }];
            }
            if text.is_some() || sort.is_some() {
                self.dirty = true;
                return self.rescan(cx);
            }
        }
        if self.job.is_some() {
            return;
        }
        if let (Ok(row), None) = (std::env::var("DSV_GOTO"), self.bench_goto) {
            self.goto.update(cx, |s, cx| s.set_value(row, window, cx));
            let t = Instant::now();
            self.go_to(cx);
            self.bench_goto = Some(t.elapsed().as_secs_f64() * 1e3);
        }
        if let (Some(rows), None) = (std::env::var("DSV_COPY").ok().and_then(|r| r.parse::<usize>().ok()), self.bench_copy) {
            if let Some(g) = &mut self.grid {
                let cols = g.shown().len();
                g.sel = Some(Sel { ar: 0, ac: 0, fr: rows.min(g.len()).saturating_sub(1), fc: cols.saturating_sub(1) });
            }
            let t = Instant::now();
            let bytes = self.copy_text(false).map_or(0, |c| c.0.len());
            self.bench_copy = Some((t.elapsed().as_secs_f64() * 1e3, bytes));
        }
        if let (Ok(rows), false) = (std::env::var("DSV_PROFILE"), self.bench_profiled) {
            self.bench_profiled = true;
            if !self.profile {
                self.toggle_pane(Pane::Profile, cx);
            }
            self.prof.all = match rows.as_str() {
                "all" => Some(true),
                "1000" => Some(false),
                _ => None,
            };
            self.prof.every = std::env::var("DSV_CARDS").as_deref() != Ok("screen");
            self.prof.distinct = match std::env::var("DSV_DISTINCT").as_deref() {
                Ok("exact") => Some(dsv::Distinct::Exact),
                Ok("approx") => Some(dsv::Distinct::Approx),
                _ => None,
            };
            (self.prof.dirty, self.prof_gap) = (true, 0.0);
            return;
        }
        if self.profile && (self.prof.dirty || self.prof.running()) {
            return;
        }
        if let (Ok(spec), None) = (std::env::var("DSV_TOP"), &self.bench_top) {
            // "4,8": a filter on each column's top value, joined by and; "4|8": joined by or.
            let out = self.prof.merged();
            let seps: Vec<char> = spec.chars().filter(|c| matches!(c, ',' | '|')).collect();
            let mut tops = Vec::new();
            for (k, token) in spec.split([',', '|']).enumerate() {
                // "9:3": bar 3 of column 9's histogram, as a click on it does.
                if let Some((c, i)) = token.split_once(':').and_then(|(c, i)| Some((c.parse::<usize>().ok()?, i.parse::<usize>().ok()?))) {
                    let ty = self.grid.as_ref().and_then(|g| g.doc.dtypes.get(c).copied()).unwrap_or(dsv::Dtype::String);
                    let Some(p) = out.iter().find(|p| p.col == c) else { continue };
                    if let Some(f) = pane::bar_filter(c, ty, p.span.unwrap_or_default(), i) {
                        tops.push(format!("bar {i} of column {c} counts {} rows", p.hist.get(i).copied().unwrap_or(0)));
                        self.add_filter(Some(f), window, cx);
                    }
                    continue;
                }
                let Some(col) = token.parse::<usize>().ok() else { continue };
                let Some(v) = out.iter().find(|p| p.col == col).and_then(|p| p.top.first().map(|v| v.0.clone())) else { continue };
                self.filter_value(col, v.clone(), window, cx);
                if let (true, Some(f)) = (k > 0 && seps.get(k - 1) == Some(&'|'), self.filters.last_mut()) {
                    f.or = true;
                }
                tops.push(v);
            }
            (self.bench_top, self.dirty) = (Some(tops.join(", ")), true);
            return self.rescan(cx);
        }
        if self.job.is_some() || self.dirty {
            return;
        }
        let Some(g) = &self.grid else { return };
        let index_ms = g.doc.index.ms.load(Ordering::Relaxed);
        let scan = self.scan_ms.map_or(String::new(), |ms| format!(" · scan {ms:.0} ms, {} rows", g.len()));
        let goto = self.bench_goto.map_or(String::new(), |ms| format!(" · go to row {ms:.2} ms, top {:.0}, mark {:?}", g.top.get(), g.mark));
        let copy = self.bench_copy.map_or(String::new(), |(ms, bytes)| format!(" · copy {ms:.1} ms, {bytes} bytes"))
            + &self.prof.done_ms().filter(|_| self.bench_profiled).map_or(String::new(), |ms| {
                let (first, (done, ..)) = (self.prof.first_ms().unwrap_or(f64::NAN), self.prof.progress());
                format!(
                    " · profile {ms:.0} ms, first card {first:.0} ms, {done} of {} columns, {} cards on screen, {} rows, longest frame gap {:.1} ms",
                    self.prof.order.len(),
                    self.prof.visible,
                    self.prof.rows,
                    self.prof_gap
                )
            });
        let name = g.doc.path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
        let top = self.bench_top.as_ref().map_or(String::new(), |v| format!(" · top value {v:?}: filters {:?}", self.filters));
        eprintln!("bench: {name} · open → first rows {:.1} ms · index {index_ms} ms{scan}{goto}{copy}{top}", self.first.unwrap_or(f64::NAN));
        self.bench = None;
        if std::env::var_os("DSV_HOLD").is_none() {
            window.remove_window();
        }
    }
}

/// Starts another instance of this app on `path`: one instance shows one file. The test hooks
/// (DSV_*, but not DSV_BENCH) do not pass on.
fn new_instance(path: &Path) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.arg(path);
    for (k, _) in std::env::vars().filter(|(k, _)| k.starts_with("DSV_") && k != "DSV_BENCH") {
        cmd.env_remove(k);
    }
    let mut child = cmd.spawn()?;
    std::thread::spawn(move || child.wait()); // reaps it when it ends
    Ok(())
}

/// The design's colors on gpui-component's theme, light or dark, and IBM Plex Sans.
fn apply_theme(theme: Theme, window: &mut Window, cx: &mut App) {
    // AppKit's answer, not gpui's: gpui updates its copy of the appearance a frame later.
    let dark = mac::set_appearance(match theme {
        Theme::Dark => Some(true),
        Theme::Light => Some(false),
        Theme::System => None,
    });
    use gpui_kit::component::Theme as Kit;
    Kit::change(if dark { ThemeMode::Dark } else { ThemeMode::Light }, Some(window), cx);
    let t = if dark { Tokens::dark() } else { Tokens::light() };
    Kit::update(cx, |k| {
        // Menus, dialogs, and checkboxes size in rem: 14 px. The toolbar sets its own pixels.
        (k.font_family, k.font_size, k.radius, k.radius_lg) = ("IBM Plex Sans".into(), px(14.), px(6.), px(10.));
        let c = &mut k.colors;
        (c.background, c.foreground, c.muted_foreground) = (t.bg, t.ink, t.muted);
        (c.border, c.input, c.popover, c.popover_foreground) = (t.line, t.line, t.field, t.ink);
        (c.primary, c.primary_hover, c.primary_active, c.primary_foreground) = (t.accent, t.accent_hover, t.accent_hover, rgb(0xFFFFFF).into());
        (c.button_primary, c.button_primary_hover, c.button_primary_active) = (t.accent, t.accent_hover, t.accent_hover);
        c.button_primary_foreground = rgb(0xFFFFFF).into();
        (c.accent, c.accent_foreground, c.ring) = (t.hover, t.ink, t.accent);
    });
}

impl Render for Reader {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t0 = Instant::now();
        if let Some(p) = self.reload.take() {
            self.load(p, window, cx);
        }
        if let (None, Some(at), Some(opened)) = (self.first, self.drew.get(), self.opened) {
            let ms = (at - opened).as_secs_f64() * 1e3;
            let since_start = START.get().map_or(0.0, |s| (at - *s).as_secs_f64() * 1e3);
            eprintln!("open → first rows {ms:.1} ms · process start → first rows {since_start:.1} ms");
            self.first = Some(ms);
            if std::mem::take(&mut self.ask_save_first) {
                self.ask_save(window, cx);
            }
            if let Some(p) = self.pick.take() {
                self.arrive(p, window, cx);
            }
        }
        self.take_opened(window, cx);
        if self.grid.as_mut().is_some_and(|g| g.edge_scroll()) {
            window.request_animation_frame();
        }
        let indexing = self.grid.as_ref().is_some_and(|g| !g.doc.index.done.load(Ordering::Acquire));
        if indexing || (self.grid.is_some() && self.first.is_none()) {
            window.request_animation_frame();
        }
        self.rescan(cx);
        let done = self.grid.as_ref().is_some_and(|g| g.doc.index.done.load(Ordering::Acquire));
        if self.profile && self.prof.dirty && done {
            self.start_profile(cx);
        }
        let now = Instant::now();
        if let (true, Some(last)) = (self.prof.running(), self.last_frame) {
            self.prof_gap = self.prof_gap.max((now - last).as_secs_f64() * 1e3);
        }
        self.last_frame = Some(now);
        if self.prof.running() {
            window.request_animation_frame(); // the progress line moves; the gaps are measured
        }
        self.bench(window, cx);
        let t = if cx.theme().is_dark() { Tokens::dark() } else { Tokens::light() };
        let draws = self.draws.clone();
        let end = canvas(|_, _, _| {}, move |_, _, _, _| draws.borrow_mut().push(t0.elapsed().as_secs_f64() * 1e3));
        let toolbar = self.toolbar(t, window, cx);
        let filters = self.filter_bar(t, window, cx);
        let (right, below) = self.side_panes(t, window, cx);
        let body = match &self.grid {
            Some(_) => {
                let grid = self.grid_view(t, cx);
                // The grid with the Viewer below it in one column; the right panes beside both.
                let column = div().flex().flex_col().flex_1().min_h_0().min_w_0().overflow_hidden().child(grid).children(below);
                div().flex().flex_row().flex_1().min_h_0().min_w_0().overflow_hidden().child(column).children(right).into_any_element()
            }
            None => div()
                .id("empty")
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(t.muted)
                .child("Drop a CSV, TSV, or other delimited file here, or press ⌘O, or Double Click to open.")
                .on_click(cx.listener(|r, e: &ClickEvent, window, cx| {
                    if e.click_count() == 2 {
                        r.pick_file(window, cx);
                    }
                }))
                .into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.bg)
            .key_context("Reader")
            .track_focus(&self.focus)
            .on_action(cx.listener(|r, _: &OpenFile, window, cx| r.pick_file(window, cx)))
            .on_action(cx.listener(|r, _: &FocusSearch, window, cx| r.search.update(cx, |s, cx| s.focus(window, cx))))
            .on_action(cx.listener(|r, _: &FocusGoto, window, cx| r.goto.update(cx, |s, cx| s.focus(window, cx))))
            .on_action(cx.listener(|r, _: &ExportFile, window, cx| r.start_export(window, cx)))
            .on_drop(cx.listener(|r, paths: &ExternalPaths, window, cx| {
                for p in paths.paths().to_vec() {
                    r.arrive(p, window, cx);
                }
            }))
            .on_mouse_move(cx.listener(|r, e: &MouseMoveEvent, window, cx| {
                // A selection drag that left the grid: the frames scroll toward the mouse.
                if let Some(g) = r.grid.as_mut().filter(|g| matches!(g.drag, Some(Drag::Select { .. }))) {
                    g.drag_at = Some(e.position);
                    return cx.notify();
                }
                if let Some((at, split)) = r.split_drag {
                    r.settings.split = (split + (f32::from(e.position.y) - at) / r.pane_h.get().max(1.0)).clamp(0.15, 0.85);
                    return cx.notify();
                }
                let Some((bottom, at, size)) = r.pane_drag else { return };
                let (now, whole) = if bottom { (e.position.y, window.bounds().size.height) } else { (e.position.x, window.bounds().size.width) };
                // The pane is right of or below the edge: a drag toward the grid makes it larger.
                let size = (size - (f32::from(now) - at)).clamp(160.0, (f32::from(whole) - 240.0).max(160.0));
                if bottom {
                    r.settings.pane_height = size;
                } else {
                    r.settings.pane_size = size;
                }
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|r, _: &MouseUpEvent, _, cx| {
                    if r.pane_drag.take().is_some() | r.split_drag.take().is_some() {
                        r.settings.save();
                        cx.notify();
                    }
                }),
            )
            .child(toolbar)
            .children(filters)
            .child(body)
            .child(
                div()
                    .h(px(28.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .px(px(12.))
                    .bg(t.bar)
                    .border_t_1()
                    .border_color(t.line)
                    .text_size(px(12.))
                    .text_color(t.muted)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(self.status()),
            )
            .child(end.absolute().size_0())
    }
}

/// `dsv-reader profile FILE [--columns NAME,NAME] [--first N] [--approx]`: the Profile as JSON on
/// stdout, with no window. Exit 0; 1 for a bad argument or a column that is not in the file; 2
/// when the file cannot be read.
fn profile_cli(args: &[String]) -> i32 {
    const USAGE: &str = "usage: dsv-reader profile FILE [--columns NAME,NAME] [--first N] [--approx]";
    let (mut file, mut names, mut first, mut distinct) = (None, None, None, dsv::Distinct::Exact);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--columns" => match it.next() {
                Some(v) => names = Some(v.split(',').map(str::to_string).collect::<Vec<_>>()),
                None => return usage(USAGE),
            },
            "--first" => match it.next().and_then(|n| n.parse::<usize>().ok()) {
                Some(n) => first = Some(n),
                None => return usage(USAGE),
            },
            "--approx" => distinct = dsv::Distinct::Approx,
            _ if file.is_none() && !a.starts_with("--") => file = Some(PathBuf::from(a)),
            _ => return usage(USAGE),
        }
    }
    let Some(file) = file else { return usage(USAGE) };
    let doc = match Doc::open(&file, &Opts::default(), || {}) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{}: {e}", file.display());
            return 2;
        }
    };
    while !doc.index.done.load(Ordering::Acquire) {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    // Every column by number, so two columns with one name both come out.
    let mut cols: Vec<usize> = if names.is_none() { (0..doc.names.len()).collect() } else { Vec::new() };
    for n in names.unwrap_or_default() {
        match doc.names.iter().position(|x| *x == n) {
            Some(c) => cols.push(c),
            None => {
                eprintln!("{}: no column {n:?}", file.display());
                return 1;
            }
        }
    }
    println!("{}", dsv::profile_json(&doc, &cols, first, distinct));
    0
}

fn usage(text: &str) -> i32 {
    eprintln!("{text}");
    1
}

fn main() {
    START.set(Instant::now()).ok();
    if std::env::args().nth(1).as_deref() == Some("profile") {
        let code = profile_cli(&std::env::args().skip(2).collect::<Vec<_>>());
        std::io::Write::flush(&mut std::io::stdout()).ok();
        std::process::exit(code);
    }
    let path = std::env::args_os().nth(1).map(PathBuf::from);
    let scroll = std::env::var_os("DSV_SCROLL").is_some();
    let app = gpui_kit::application().with_assets(gpui_kit::assets::AllAssets);
    // Finder's files (a double-click, Open With, a drop on the Dock icon). At launch they come
    // before the window exists; the window takes them on its next frame.
    app.on_open_urls(|urls| {
        mac::OPENED.lock().unwrap().extend(urls.iter().filter_map(|u| mac::file_path(u)));
        mac::wake();
    });
    app.run(move |cx| {
        gpui_kit::init(cx);
        mac::install();
        // ⌘Q quits through AppKit, which asks `mac` first.
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.set_menus([Menu { name: "DSV Reader".into(), items: vec![MenuItem::action("Quit DSV Reader", Quit)], disabled: false }]);
        let plex: [&'static [u8]; 3] =
            [include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf"), include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf"), include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf")];
        cx.text_system().add_fonts(plex.into_iter().map(Cow::Borrowed).collect()).ok();
        cx.bind_keys([
            KeyBinding::new("cmd-o", OpenFile, Some("Reader")),
            KeyBinding::new("cmd-f", FocusSearch, Some("Reader")),
            KeyBinding::new("cmd-g", FocusGoto, Some("Reader")),
            KeyBinding::new("cmd-s", ExportFile, Some("Reader")),
            KeyBinding::new("up", MoveUp, Some("Grid")),
            KeyBinding::new("down", MoveDown, Some("Grid")),
            KeyBinding::new("left", MoveLeft, Some("Grid")),
            KeyBinding::new("right", MoveRight, Some("Grid")),
            KeyBinding::new("shift-up", ExtendUp, Some("Grid")),
            KeyBinding::new("shift-down", ExtendDown, Some("Grid")),
            KeyBinding::new("shift-left", ExtendLeft, Some("Grid")),
            KeyBinding::new("shift-right", ExtendRight, Some("Grid")),
            KeyBinding::new("pageup", GridPageUp, Some("Grid")),
            KeyBinding::new("pagedown", GridPageDown, Some("Grid")),
            KeyBinding::new("shift-pageup", ExtendPageUp, Some("Grid")),
            KeyBinding::new("shift-pagedown", ExtendPageDown, Some("Grid")),
            KeyBinding::new("cmd-a", SelectAll, Some("Grid")),
            KeyBinding::new("cmd-c", CopyCells, Some("Grid")),
            KeyBinding::new("escape", ClearSelection, Some("Grid")),
            KeyBinding::new("cmd-q", Quit, None),
        ]);
        // In front, as a Finder launch would be: macOS throttles the frames of a window behind.
        cx.activate(true);
        // Exit at once when the window closes. AppKit's Touch Bar finder throws an NSRangeException
        // on the next display cycle after the window's view goes away, and the app aborts (the
        // egui app had the same). The settings are written on each change.
        cx.on_window_closed(|_, _| std::process::exit(0)).detach();
        // 1440 wide, as the design canvas: toolbar B's search box gets its room.
        let size = if scroll { size(px(1600.), px(1000.)) } else { size(px(1440.), px(900.)) };
        let size = std::env::var("DSV_SIZE")
            .ok()
            .and_then(|s| {
                let (w, h) = s.split_once('x')?;
                Some(gpui_kit::size(px(w.parse().ok()?), px(h.parse().ok()?)))
            })
            .unwrap_or(size);
        let opts = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size, cx))),
            titlebar: Some(TitlebarOptions { title: Some("DSV Reader".into()), ..Default::default() }),
            // The toolbar fits down to this width (`toolbar::overflowed`).
            window_min_size: Some(gpui_kit::size(px(640.), px(400.))),
            ..Default::default()
        };
        let (_, view) = gpui_kit::open_window(opts, cx, |window, cx| {
            let theme = match std::env::var("DSV_THEME").as_deref() {
                Ok("light") => Theme::Light,
                Ok("dark") => Theme::Dark,
                _ => Settings::load().theme,
            };
            apply_theme(theme, window, cx);
            let view = cx.new(|cx| Reader::new(path, window, cx));
            let focus = view.read(cx).focus.clone();
            focus.focus(window, cx);
            // The close button: with an unsaved column order, ask first.
            let v = view.clone();
            window.on_window_should_close(cx, move |window, cx| {
                let ask = v.read(cx).modified() && !v.read(cx).quit_ok;
                if ask {
                    v.update(cx, |r, cx| r.ask_quit(window, cx));
                }
                !ask
            });
            view
        })
        .expect("open the window");
        let (app, view) = (cx.to_async(), view.downgrade());
        mac::set_wake(move || {
            let view = view.clone();
            app.spawn(async move |cx| view.update(cx, |_, cx| cx.notify()).ok()).detach();
        });
    });
}
