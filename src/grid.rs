//! The grid, painted by hand. Only the rows and columns on screen are parsed, shaped, and
//! painted: gpui-component's `DataTable` builds a layout node per cell and drew a page scroll in
//! p95 21.7 ms. gpui keeps shaped lines from the last frame, so a still grid shapes nothing new.
//! The scroll position is in rows (f64): 6.55 M rows × 24 px is past what f32 pixels hold exactly.
//! The paint records where everything went (`Layout`); the mouse reads it back to hit-test.

use crate::dsv::{Dir, Doc, Dtype, Family, Kind, Order, Reading, Sel, Show, SortKey, arrange, family, is_zero, show};
use gpui_kit::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

/// Cell padding of the design's table: 12 px at the sides, 6 px above and below.
pub const PAD: f32 = 12.0;
const PAD_Y: f32 = 6.0;
const BAR: f32 = 10.0;
const START_W: f32 = 140.0;
pub const MIN_W: f32 = 24.0;
/// A press on a column name moves the column once the mouse goes this far.
const MOVE_AFTER: f32 = 4.0;

/// Colors of the toolbar canvas (design B), light and dark.
#[derive(Clone, Copy)]
pub struct Tokens {
    pub bg: Hsla,
    pub bar: Hsla,
    pub ink: Hsla,
    pub muted: Hsla,
    pub line: Hsla,
    pub line_soft: Hsla,
    pub thumb: Hsla,
    /// The design's `--lineStrong` and the 3 px focus ring.
    pub line_strong: Hsla,
    pub ring: Hsla,
    /// The row that Go to row marked.
    pub mark: Hsla,
    /// Fields and popovers.
    pub field: Hsla,
    pub hover: Hsla,
    pub accent: Hsla,
    pub accent_hover: Hsla,
    /// Selected cells.
    pub sel: Hsla,
    /// The column names: a step darker than the toolbar's bar.
    pub head: Hsla,
    /// Every second row of the view, under the cells; `bg` when zebra rows are off.
    pub zebra: Hsla,
    /// The row-number column: between `bg` and `head`.
    pub nums: Hsla,
    /// The text of a date or a date-and-time cell.
    pub date: Hsla,
}

impl Tokens {
    pub fn light() -> Tokens {
        Tokens {
            bg: rgb(0xFFFFFF).into(),
            bar: rgb(0xF6F7F9).into(),
            ink: rgb(0x1B1E23).into(),
            muted: rgb(0x5F6670).into(),
            line: rgb(0xD9DCE1).into(),
            line_soft: rgb(0xE9EBEE).into(),
            thumb: rgb(0xB8BDC5).into(),
            line_strong: rgb(0xB8BDC5).into(),
            ring: hsla(0.608, 0.74, 0.47, 0.18),
            mark: rgb(0xFFF4D6).into(),
            field: rgb(0xFFFFFF).into(),
            hover: rgb(0xEBEDF0).into(),
            accent: rgb(0x1F5FD1).into(),
            accent_hover: rgb(0x174AA6).into(),
            sel: hsla(0.608, 0.74, 0.47, 0.12),
            head: rgb(0xECEEF1).into(),
            zebra: rgb(0xF8F9FA).into(),
            nums: rgb(0xF2F3F5).into(),
            date: rgb(0x0F6E62).into(),
        }
    }

    /// Soft Dark: low glare, no pure black or white. Ink on every surface is 7.3:1 or more,
    /// muted 4.5:1 or more.
    pub fn dark() -> Tokens {
        Tokens {
            bg: rgb(0x1E2127).into(),
            bar: rgb(0x23272E).into(),
            ink: rgb(0xC9CED6).into(),
            muted: rgb(0x9AA2AD).into(),
            line: rgb(0x2F343C).into(),
            line_soft: rgb(0x2A2F37).into(),
            thumb: rgb(0x3A404A).into(),
            line_strong: rgb(0x3A404A).into(),
            ring: rgba(0x5A7FB84D).into(),
            mark: rgb(0x3D3522).into(),
            field: rgb(0x2A2F37).into(),
            hover: rgb(0x323843).into(),
            accent: rgb(0x4A6FA5).into(),
            accent_hover: rgb(0x5679B0).into(),
            sel: rgba(0x5A7FB838).into(),
            head: rgb(0x252A31).into(),
            zebra: rgb(0x22262C).into(),
            nums: rgb(0x21252B).into(),
            date: rgb(0x7DBFB0).into(),
        }
    }
}

/// 1234567 → "1,234,567".
/// "1 column", "2 columns", "1,000 columns".
pub fn count(n: usize, word: &str) -> String {
    format!("{} {word}{}", sep(n), if n == 1 { "" } else { "s" })
}

pub fn sep(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// A column's type and cell format, set in its menu. They reset when the file closes.
#[derive(Clone, Debug, PartialEq)]
pub struct ColFmt {
    pub kind: Kind,
    pub show: Show,
}

impl Default for ColFmt {
    fn default() -> Self {
        ColFmt { kind: Kind::Auto, show: Show::Plain }
    }
}

/// Whether the format `s` can show a cell of dtype `t`.
pub fn fits(s: &Show, t: Dtype) -> bool {
    match s {
        Show::Plain => true,
        Show::Number { .. } | Show::Percent { .. } | Show::Currency { .. } => family(t) == Family::Number,
        Show::Duration { .. } => matches!(family(t), Family::Number | Family::Time),
        Show::Date(_) | Show::DateTime(..) => family(t) == Family::Date,
        Show::Time(_) => matches!(t, Dtype::Time | Dtype::DateTime),
        Show::YesNo { .. } => t == Dtype::Bool,
    }
}

/// A character's glyph in the grid font, or in the font CoreText falls back to, and its advance.
type Glyph = (FontId, GlyphId, f32);

/// The grid font's glyphs by character, kept for the life of the grid. None: the character
/// needs CoreText's full shaping.
type Glyphs = Rc<RefCell<HashMap<char, Option<Glyph>>>>;

/// Where the last paint put things, in window pixels: what the mouse hit-tests against.
#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub x0: f32,
    pub y0: f32,
    pub right: f32,
    pub bottom: f32,
    pub body_top: f32,
    pub cells_left: f32,
    pub rh: f32,
    pub head: f32,
    /// Column edges from the first shown column, before the horizontal scroll.
    pub xs: Vec<f32>,
    pub top: f64,
    pub left: f32,
    pub n: usize,
    pub vis: f64,
    pub max_top: f64,
    pub max_left: f32,
    pub vthumb: f32,
    pub hthumb: f32,
}

/// What a point is over.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    /// A cell: view row and shown column.
    Cell(usize, usize),
    /// A row number.
    Row(usize),
    /// A column name, its sort mark, or its right edge.
    Head(usize),
    Sort(usize),
    Edge(usize),
    VBar,
    HBar,
    None,
}

impl Layout {
    pub fn x_of(&self, k: usize) -> f32 {
        self.cells_left + self.xs[k] - self.left
    }

    /// The shown column under `x`, if any.
    pub fn col_at(&self, x: f32) -> Option<usize> {
        let k = self.xs.partition_point(|&v| v <= x - self.cells_left + self.left);
        (x >= self.cells_left && k >= 1 && k < self.xs.len()).then(|| k - 1)
    }

    /// The view row under `y`, if any.
    pub fn row_at(&self, y: f32) -> Option<usize> {
        let r = self.top + ((y - self.body_top) / self.rh) as f64;
        (y >= self.body_top && r >= 0.0 && (r as usize) < self.n).then_some(r as usize)
    }

    /// How far a selection drag at `p` scrolls in one frame: rows down (negative: up) and points
    /// right (negative: left); none inside the cells. Faster the farther out: a row a frame at
    /// 20 px, at most 10. A drag over row numbers moves rows only.
    pub fn edge_scroll(&self, p: Point<Pixels>, rows_only: bool) -> (f64, f32) {
        let (x, y) = (f32::from(p.x), f32::from(p.y));
        let past = |v: f32, lo: f32, hi: f32| {
            if v > hi {
                v - hi
            } else if v < lo {
                v - lo
            } else {
                0.0
            }
        };
        let (dy, dx) = (past(y, self.body_top, self.bottom), if rows_only { 0.0 } else { past(x, self.cells_left, self.right) });
        let dy = dy as f64;
        let rows = if dy == 0.0 { 0.0 } else { dy.signum() * (dy.abs() / 20.0).clamp(0.2, 10.0) };
        let pts = if dx == 0.0 { 0.0 } else { dx.signum() * dx.abs().clamp(2.0, 60.0) };
        (rows, pts)
    }

    pub fn hit(&self, p: Point<Pixels>) -> Hit {
        let (x, y) = (f32::from(p.x), f32::from(p.y));
        if self.xs.is_empty() || x < self.x0 || y < self.y0 || x > self.right + BAR || y > self.bottom + BAR {
            return Hit::None;
        }
        if x >= self.right {
            return if y >= self.body_top && y < self.bottom { Hit::VBar } else { Hit::None };
        }
        if y >= self.bottom {
            return if x >= self.cells_left { Hit::HBar } else { Hit::None };
        }
        if y < self.body_top {
            if x < self.cells_left {
                return Hit::None;
            }
            // An edge first: 4 px either side of it.
            let at = x - self.cells_left + self.left;
            if let Some(k) = (1..self.xs.len()).find(|&k| (self.xs[k] - at).abs() <= 4.0) {
                return Hit::Edge(k - 1);
            }
            return match self.col_at(x) {
                Some(k) if x >= self.x_of(k + 1) - PAD - self.head => Hit::Sort(k),
                Some(k) => Hit::Head(k),
                None => Hit::None,
            };
        }
        match (self.row_at(y), x < self.cells_left) {
            (Some(r), true) => Hit::Row(r),
            (Some(r), false) => self.col_at(x).map_or(Hit::None, |k| Hit::Cell(r, k)),
            _ => Hit::None,
        }
    }
}

/// A mouse drag in progress.
#[derive(Clone, Copy, Debug)]
pub enum Drag {
    /// Over cells or row numbers: the far corner follows the mouse.
    Select {
        rows: bool,
    },
    /// A column edge: the column and its width and the mouse x at the press.
    Resize {
        c: usize,
        x: f32,
        w: f32,
    },
    /// A column name: its shown position, the press x, and whether it moves yet.
    Move {
        from: usize,
        x: f32,
        moving: bool,
        gap: Option<usize>,
    },
    /// A scroll bar thumb: the press position and the scroll then.
    VThumb {
        y: f32,
        top: f64,
    },
    HThumb {
        x: f32,
        left: f32,
    },
}

/// What a mouse press asks of the app.
#[derive(Clone, Copy, Debug)]
pub enum Act {
    /// A sort mark: the column, and whether Shift was down.
    Sort(usize, bool),
    /// A double-click on a column edge: fit the column.
    Fit(usize),
}

/// What the grid keeps between frames. The paint writes back the clamped scroll, the rows that
/// fit, and the layout, so those are shared with the paint closure.
pub struct Grid {
    pub doc: Rc<Doc>,
    /// The rows on screen, in order, as data row numbers. None: every row, in file order.
    pub view: Option<Arc<Vec<u32>>>,
    /// Column order, as column numbers. A hidden column keeps its place.
    pub order: Vec<usize>,
    pub widths: Vec<f32>,
    pub hidden: Vec<bool>,
    pub fmts: Vec<ColFmt>,
    pub sel: Option<Sel>,
    /// The data row that Go to row marked.
    pub mark: Option<usize>,
    pub top: Rc<Cell<f64>>,
    pub left: Rc<Cell<f32>>,
    /// Rows that fit on screen, from the last paint.
    pub vis: Rc<Cell<f64>>,
    pub layout: Rc<RefCell<Layout>>,
    pub drag: Option<Drag>,
    /// The mouse in a selection drag, from the window: it can leave the grid.
    pub drag_at: Option<Point<Pixels>>,
    /// The mouse is over a column edge: the cursor says so.
    pub on_edge: bool,
    /// The column menu: the column, and where it opened.
    pub menu: Option<(usize, Point<Pixels>)>,
    pub font: Font,
    pub size: f32,
    glyphs: Glyphs,
    /// The row numbers' glyphs: IBM Plex Sans SemiBold, as the column names.
    num_glyphs: Glyphs,
}

/// One frame's paint: what it reads, and where it reports.
struct Frame {
    doc: Rc<Doc>,
    view: Option<Arc<Vec<u32>>>,
    /// The shown columns, in order, as column numbers.
    shown: Vec<usize>,
    mark: Option<usize>,
    sel: Option<Sel>,
    sort: Vec<SortKey>,
    shows: Vec<Show>,
    /// A column name being dragged: the gap it would drop in.
    drop: Option<usize>,
    widths: Vec<f32>,
    top: Rc<Cell<f64>>,
    left: Rc<Cell<f32>>,
    vis: Rc<Cell<f64>>,
    layout: Rc<RefCell<Layout>>,
    font: Font,
    size: f32,
    rh: f32,
    glyphs: Glyphs,
    num_glyphs: Glyphs,
    t: Tokens,
    /// Set to the end of the paint when it drew rows.
    drew: Rc<Cell<Option<Instant>>>,
}

impl Grid {
    pub fn new(doc: Doc, font: Font, size: f32) -> Grid {
        Grid {
            doc: Rc::new(doc),
            view: None,
            order: Vec::new(),
            widths: Vec::new(),
            hidden: Vec::new(),
            fmts: Vec::new(),
            sel: None,
            mark: None,
            top: Rc::default(),
            left: Rc::default(),
            vis: Rc::default(),
            layout: Rc::default(),
            drag: None,
            drag_at: None,
            on_edge: false,
            menu: None,
            font,
            size,
            glyphs: Rc::default(),
            num_glyphs: Rc::default(),
        }
    }

    /// Adds columns up to the file's count, at the end, shown.
    pub fn fit_columns(&mut self) {
        while self.widths.len() < self.doc.names.len() {
            self.order.push(self.widths.len());
            self.hidden.push(false);
            self.widths.push(START_W);
            self.fmts.push(ColFmt::default());
        }
    }

    /// A new grid font: the glyph cache starts again.
    pub fn set_font(&mut self, font: Font, size: f32) {
        (self.font, self.size, self.glyphs, self.num_glyphs) = (font, size, Rc::default(), Rc::default());
    }

    /// The shown columns, in order.
    pub fn shown(&self) -> Vec<usize> {
        self.order.iter().copied().filter(|&c| !self.hidden.get(c).copied().unwrap_or(false)).collect()
    }

    pub fn reset_order(&mut self) {
        self.order = (0..self.widths.len()).collect();
    }

    /// Rows in the view.
    pub fn len(&self) -> usize {
        self.view.as_ref().map_or(self.doc.rows(), |v| v.len())
    }

    /// The row height for the font size.
    pub fn row_h(&self) -> f32 {
        (self.size * 1.25).round() + 2.0 * PAD_Y
    }

    /// The canvas that paints the grid.
    pub fn canvas(&mut self, t: Tokens, sort: &[SortKey], drew: Rc<Cell<Option<Instant>>>) -> impl IntoElement + use<> {
        self.fit_columns();
        let drop = match self.drag {
            Some(Drag::Move { moving: true, gap, .. }) => gap,
            _ => None,
        };
        let f = Frame {
            doc: self.doc.clone(),
            view: self.view.clone(),
            shown: self.shown(),
            mark: self.mark,
            sel: self.sel,
            sort: sort.to_vec(),
            shows: self.fmts.iter().map(|f| f.show.clone()).collect(),
            drop,
            widths: self.widths.clone(),
            top: self.top.clone(),
            left: self.left.clone(),
            vis: self.vis.clone(),
            layout: self.layout.clone(),
            font: self.font.clone(),
            size: self.size,
            rh: self.row_h(),
            glyphs: self.glyphs.clone(),
            num_glyphs: self.num_glyphs.clone(),
            t,
            drew,
        };
        canvas(|_, _, _| {}, move |bounds, _, window, cx| f.paint(bounds, window, cx)).size_full()
    }

    /// The wheel: rows down and points across.
    pub fn wheel(&mut self, e: &ScrollWheelEvent) {
        let rh = self.row_h();
        let d = e.delta.pixel_delta(px(rh));
        self.top.set(self.top.get() - (f32::from(d.y) / rh) as f64);
        self.left.set(self.left.get() - f32::from(d.x));
    }

    /// A left press. Selects, starts a drag, or asks for a sort or a fit.
    pub fn press(&mut self, e: &MouseDownEvent) -> Option<Act> {
        let l = self.layout.borrow().clone();
        let shift = e.modifiers.shift;
        let last = l.xs.len().saturating_sub(2);
        match l.hit(e.position) {
            Hit::Cell(r, k) => {
                self.sel = Some(match (shift, self.sel) {
                    (true, Some(s)) => Sel { fr: r, fc: k, ..s },
                    _ => Sel::at(r, k),
                });
                self.drag = Some(Drag::Select { rows: false });
            }
            Hit::Row(r) => {
                self.sel = Some(match (shift, self.sel) {
                    (true, Some(s)) => Sel { fr: r, fc: last, ac: 0, ..s },
                    _ => Sel { ar: r, ac: 0, fr: r, fc: last },
                });
                self.drag = Some(Drag::Select { rows: true });
            }
            Hit::Head(k) => self.drag = Some(Drag::Move { from: k, x: f32::from(e.position.x), moving: false, gap: None }),
            Hit::Sort(k) => return Some(Act::Sort(self.shown()[k], shift)),
            Hit::Edge(k) => {
                let c = self.shown()[k];
                if e.click_count == 2 {
                    return Some(Act::Fit(c));
                }
                self.drag = Some(Drag::Resize { c, x: f32::from(e.position.x), w: self.widths[c] });
            }
            Hit::VBar => {
                let y = f32::from(e.position.y);
                let span = (l.bottom - l.body_top - l.vthumb).max(1.0);
                let at = l.body_top + span * (l.top / l.max_top.max(1.0)) as f32;
                if y < at {
                    self.top.set(l.top - l.vis);
                } else if y > at + l.vthumb {
                    self.top.set(l.top + l.vis);
                } else {
                    self.drag = Some(Drag::VThumb { y, top: l.top });
                }
            }
            Hit::HBar => {
                let x = f32::from(e.position.x);
                let width = l.right - l.cells_left;
                let span = (width - l.hthumb).max(1.0);
                let at = l.cells_left + span * (l.left / l.max_left.max(1.0));
                if x < at {
                    self.left.set(l.left - width);
                } else if x > at + l.hthumb {
                    self.left.set(l.left + width);
                } else {
                    self.drag = Some(Drag::HThumb { x, left: l.left });
                }
            }
            Hit::None => {}
        }
        None
    }

    /// The mouse moved. True when the grid must paint again.
    pub fn moved(&mut self, e: &MouseMoveEvent) -> bool {
        let l = self.layout.borrow().clone();
        let (x, y) = (f32::from(e.position.x), f32::from(e.position.y));
        let edge = matches!(l.hit(e.position), Hit::Edge(_)) || matches!(self.drag, Some(Drag::Resize { .. }));
        let changed = edge != self.on_edge;
        self.on_edge = edge;
        let Some(drag) = self.drag else { return changed };
        match drag {
            Drag::Select { rows } => {
                self.drag_at = Some(e.position);
                self.extend_to(&l, e.position, rows);
            }
            Drag::Resize { c, x: x0, w } => self.widths[c] = (w + x - x0).max(MIN_W),
            Drag::Move { from, x: x0, moving, .. } => {
                let moving = moving || (x - x0).abs() >= MOVE_AFTER;
                let at = x - l.cells_left + l.left;
                let gap = (0..l.xs.len() - 1).filter(|&k| (l.xs[k] + l.xs[k + 1]) / 2.0 < at).count();
                self.drag = Some(Drag::Move { from, x: x0, moving, gap: Some(gap) });
            }
            Drag::VThumb { y: y0, top } => {
                let span = (l.bottom - l.body_top - l.vthumb).max(1.0);
                self.top.set(top + ((y - y0) / span) as f64 * l.max_top);
            }
            Drag::HThumb { x: x0, left } => {
                let span = (l.right - l.cells_left - l.hthumb).max(1.0);
                self.left.set(left + (x - x0) / span * l.max_left);
            }
        }
        true
    }

    /// A selection drag: the far corner goes to the cell nearest `p` in the cells on screen.
    fn extend_to(&mut self, l: &Layout, p: Point<Pixels>, rows: bool) {
        let Some(s) = self.sel else { return };
        let (x, y) = (f32::from(p.x), f32::from(p.y));
        let r = l.row_at(y.clamp(l.body_top, l.bottom - 1.0)).unwrap_or(s.fr);
        let k = if rows { s.fc } else { l.col_at(x.clamp(l.cells_left, l.right - 1.0)).unwrap_or(s.fc) };
        self.sel = Some(Sel { fr: r, fc: k, ..s });
    }

    /// Each frame of a selection drag with the mouse past the cells: scroll toward it, and the
    /// far corner follows. True when it scrolled: the next frame scrolls again.
    pub fn edge_scroll(&mut self) -> bool {
        let (Some(Drag::Select { rows }), Some(p)) = (self.drag, self.drag_at) else { return false };
        let l = self.layout.borrow().clone();
        let (dr, dx) = l.edge_scroll(p, rows);
        if dr == 0.0 && dx == 0.0 {
            return false;
        }
        self.extend_to(&l, p, rows);
        // The paint clamps the scroll to the rows and columns there are.
        self.top.set((self.top.get() + dr).clamp(0.0, l.max_top));
        self.left.set((self.left.get() + dx).clamp(0.0, l.max_left));
        (dr != 0.0 && (l.top > 0.0 || dr > 0.0) && (l.top < l.max_top || dr < 0.0)) || (dx != 0.0 && (l.left > 0.0 || dx > 0.0) && (l.left < l.max_left || dx < 0.0))
    }

    /// The left button came up: a column drop, or a click on a name that selects the column.
    pub fn release(&mut self, shift: bool) {
        self.drag_at = None;
        if let Some(Drag::Move { from, moving, gap, .. }) = self.drag.take() {
            let shown = self.shown();
            if moving {
                if let Some(g) = gap {
                    let pos = |c: usize| self.order.iter().position(|&x| x == c).unwrap_or(0);
                    let to = if g < shown.len() { pos(shown[g]) } else { self.order.len() };
                    self.order = arrange(&self.order, pos(shown[from]), to);
                    self.sel = None;
                }
            } else if self.len() > 0 {
                let n = self.len();
                self.sel = Some(match (shift, self.sel) {
                    (true, Some(s)) => Sel { ar: 0, fr: n - 1, fc: from, ..s },
                    _ => Sel { ar: 0, ac: from, fr: n - 1, fc: from },
                });
            }
        }
    }

    /// A key move: `dr` rows and `dc` columns, extending the selection with Shift.
    pub fn step(&mut self, dr: isize, dc: isize, extend: bool) {
        let (n, cols) = (self.len(), self.shown().len());
        if n == 0 || cols == 0 {
            return;
        }
        self.sel = Some(self.sel.unwrap_or_default().step(dr, dc, extend, n, cols));
        self.reveal();
    }

    pub fn page(&self) -> isize {
        self.vis.get().floor().max(1.0) as isize
    }

    pub fn select_all(&mut self) {
        let (n, cols) = (self.len(), self.shown().len());
        if n > 0 && cols > 0 {
            self.sel = Some(Sel { ar: 0, ac: 0, fr: n - 1, fc: cols - 1 });
        }
    }

    /// Scrolls so that the far corner of the selection shows.
    pub fn reveal(&mut self) {
        let (Some(s), l) = (self.sel, self.layout.borrow().clone()) else { return };
        let (r, vis) = (s.fr as f64, l.vis.max(1.0));
        let top = self.top.get();
        if r < top {
            self.top.set(r);
        } else if r + 1.0 > top + vis.floor() {
            self.top.set(r + 1.0 - vis.floor());
        }
        if s.fc + 1 < l.xs.len() {
            let (x0, x1, width) = (l.xs[s.fc], l.xs[s.fc + 1], l.right - l.cells_left);
            let left = self.left.get();
            if x0 < left {
                self.left.set(x0);
            } else if x1 > left + width {
                self.left.set(x1 - width);
            }
        }
    }
}

impl Frame {
    fn shape(&self, window: &Window, text: &str, font: &Font, color: Hsla) -> ShapedLine {
        self.shape_at(window, text, font, self.size, color)
    }

    fn shape_at(&self, window: &Window, text: &str, font: &Font, size: f32, color: Hsla) -> ShapedLine {
        let run = TextRun { len: text.len(), font: font.clone(), color, background_color: None, underline: None, strikethrough: None };
        window.text_system().shape_line(SharedString::from(text.to_owned()), px(size), &[run], None)
    }

    fn glyph(&self, window: &Window, ch: char, (font, size, cache): (&Font, f32, &Glyphs)) -> Option<Glyph> {
        if let Some(g) = cache.borrow().get(&ch) {
            return *g;
        }
        let line = self.shape_at(window, ch.encode_utf8(&mut [0; 4]), font, size, self.t.ink);
        let g = match line.runs.as_slice() {
            [run] if run.glyphs.len() == 1 => Some((run.font_id, run.glyphs[0].id, f32::from(line.width()))),
            _ => None,
        };
        cache.borrow_mut().insert(ch, g);
        g
    }

    /// A cell's text, glyph by glyph from the cache: CoreText shapes about 10 µs a cell, so a
    /// page of new rows missed a 165 Hz frame. False when a character needs full shaping.
    // Limit: no kerning; grid fonts are mostly monospace. A proportional font sets a little loose.
    #[allow(clippy::too_many_arguments)]
    fn quick(&self, window: &mut Window, text: &str, face: (&Font, f32, &Glyphs), color: Hsla, (l, r, y): (f32, f32, f32), right: bool, base: f32) -> bool {
        let mut glyphs = Vec::with_capacity(text.len());
        for ch in text.chars() {
            // Latin and punctuation only: other scripts combine and reorder, which CoreText does.
            if !(ch < '\u{300}' || ('\u{2000}'..'\u{2100}').contains(&ch)) {
                return false;
            }
            match self.glyph(window, ch, face) {
                Some(g) => glyphs.push((ch, g)),
                None => return false,
            }
        }
        let width: f32 = glyphs.iter().map(|g| g.1.2).sum();
        let room = r - l;
        // 0.5 px of slack, as in `text`: the glyphs one by one can add up a hair wider than the
        // shaped text that sized the box (the row numbers' "0,000,000", a fitted column).
        let ell = (width > room + 0.5).then(|| self.glyph(window, '…', face)).flatten();
        let (count, shown) = match ell {
            None => (glyphs.len(), width),
            Some(e) => {
                let (mut k, mut x) = (0, 0.0);
                while k < glyphs.len() && x + glyphs[k].1.2 <= room - e.2 {
                    x += glyphs[k].1.2;
                    k += 1;
                }
                (k, x + e.2)
            }
        };
        let mut x = if right { r - shown } else { l };
        let size = px(face.1);
        for &(ch, (font, id, adv)) in glyphs[..count].iter().chain(ell.map(|e| ('…', e)).iter()) {
            if ch != ' ' {
                window.paint_glyph(point(px(x), px(y + base)), font, id, size, color).ok();
            }
            x += adv;
        }
        true
    }

    /// A body cell's text in `l..r` at row top `y`, left or right aligned, cut with "…" when it
    /// does not fit: glyph by glyph when it can, else shaped by CoreText.
    #[allow(clippy::too_many_arguments)]
    fn cell(&self, window: &mut Window, cx: &mut App, text: &str, color: Hsla, (l, r, y): (f32, f32, f32), right: bool, base: f32) {
        if !self.quick(window, text, (&self.font, self.size, &self.glyphs), color, (l, r, y), right, base) {
            self.text(window, cx, text, (&self.font, self.size, color), (l, r, y, self.rh), right);
        }
    }

    /// One line of text in `l..r`, centered in the line box at `y` of height `lh`, left or right
    /// aligned, cut with "…" when it does not fit.
    fn text(&self, window: &mut Window, cx: &mut App, text: &str, (font, size, color): (&Font, f32, Hsla), (l, r, y, lh): (f32, f32, f32, f32), right: bool) {
        let mut line = self.shape_at(window, text, font, size, color);
        let room = r - l;
        // 0.5 px of slack: a box made to the text's own width can compare a hair short.
        if f32::from(line.width()) > room + 0.5 {
            let ell = self.shape_at(window, "…", font, size, color);
            let cut = line.index_for_x(px((room - f32::from(ell.width())).max(0.0))).unwrap_or(text.len());
            line = self.shape_at(window, &format!("{}…", &text[..cut]), font, size, color);
        }
        let w = f32::from(line.width());
        let x = if right { r - w } else { l };
        line.paint(point(px(x), px(y)), px(lh), TextAlign::Left, None, window, cx).ok();
    }

    fn paint(&self, b: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let (t, doc) = (self.t, &self.doc);
        let n = self.view.as_ref().map_or(doc.rows(), |v| v.len());
        let shown = &self.shown;
        let rh = self.rh;
        // The header: 600 weight IBM Plex Sans a size below the cells, 7 px above and below.
        let head = (self.size * 0.92 * 2.0).round() / 2.0;
        let hh = (head * 1.3).round() + 14.0;
        let zero = self.shape(window, "0", &self.font, t.ink);
        let glyph = f32::from(zero.width());
        let base = (rh - f32::from(zero.ascent) - f32::from(zero.descent)) / 2.0 + f32::from(zero.ascent);
        // Row numbers in the column names' face: IBM Plex Sans SemiBold at the header size.
        let num_font = Font { weight: FontWeight::SEMIBOLD, ..font("IBM Plex Sans") };
        let nine = self.shape_at(window, &sep(doc.rows().max(1)).replace(|c: char| c.is_ascii_digit(), "0"), &num_font, head, t.ink);
        let num_base = (rh - f32::from(nine.ascent) - f32::from(nine.descent)) / 2.0 + f32::from(nine.ascent);
        // At least as wide as the corner's "#": a file of 1–9 rows cut it to "…".
        let hash = f32::from(self.shape_at(window, "#", &num_font, head, t.muted).width());
        let num_w = f32::from(nine.width()).max(glyph).max(hash) + 2.0 * PAD;
        let (x0, y0, w, h) = (f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height));
        let rect = |l: f32, t: f32, r: f32, bo: f32| Bounds::from_corners(point(px(l), px(t)), point(px(r), px(bo)));
        // Head, body, and the cells right of the row numbers; a scroll bar on the right and below.
        let (right, bottom) = (x0 + w - BAR, y0 + h - BAR);
        let (body_top, cells_left) = (y0 + hh, x0 + num_w);
        let vis = ((bottom - body_top) / rh).max(1.0) as f64;
        self.vis.set(vis);
        let mut xs = Vec::with_capacity(shown.len() + 1);
        xs.push(0.0f32);
        for &c in shown {
            xs.push(xs[xs.len() - 1] + self.widths[c]);
        }
        let max_top = (n as f64 - vis.floor()).max(0.0);
        let max_left = (xs[xs.len() - 1] - (right - cells_left)).max(0.0);
        let top = self.top.get().clamp(0.0, max_top);
        let left = self.left.get().clamp(0.0, max_left);
        self.top.set(top);
        self.left.set(left);
        let vtrack = (bottom - body_top).max(1.0);
        let vthumb = (vtrack * (vis / n.max(1) as f64).min(1.0) as f32).max(24.0).min(vtrack);
        let htrack = (right - cells_left).max(1.0);
        let hthumb = (htrack * (htrack / xs[xs.len() - 1].max(1.0)).min(1.0)).max(24.0).min(htrack);
        let layout = Layout { x0, y0, right, bottom, body_top, cells_left, rh, head, xs, top, left, n, vis, max_top, max_left, vthumb, hthumb };
        let (first_k, last_k) = match shown.len() {
            0 => (1, 0),
            len => (layout.col_at(cells_left).unwrap_or(0), layout.col_at(right - 1.0).unwrap_or(len - 1)),
        };
        let x_of = |k: usize| layout.x_of(k);
        let bounds = self.sel.map(|s| s.bounds());
        let in_cols = |k: usize| bounds.is_some_and(|(_, l, _, r)| (l..=r).contains(&k));
        // Whole rows: every shown column. Whole columns: every row (below, with the header).
        let whole_rows = bounds.is_some_and(|(_, l, _, r)| l == 0 && r + 1 == shown.len());

        window.paint_quad(fill(b, t.bg));
        // The row numbers sit on the header color, as the column names do.
        window.paint_quad(fill(rect(x0, body_top, cells_left, bottom), t.nums));
        let first = top as usize;
        let dy = ((top - first as f64) * rh as f64) as f32;
        // A row is parsed up to the last column on screen: a wide file costs no more.
        let need = shown.get(first_k..=last_k).and_then(|ks| ks.iter().max()).map_or(0, |m| m + 1);
        let cells = rect(cells_left, body_top, right, bottom);
        let nums = rect(x0, body_top, cells_left, bottom);
        let mut ys = Vec::new();
        for i in 0.. {
            let (r, y) = (first + i, body_top + i as f32 * rh - dy);
            if r >= n || y > bottom {
                break;
            }
            let d = self.view.as_ref().map_or(r, |v| v[r] as usize);
            let in_rows = bounds.is_some_and(|(t, _, b, _)| (t..=b).contains(&r));
            window.with_content_mask(Some(ContentMask { bounds: rect(x0, body_top, right, bottom) }), |window| {
                // By place in the view, so a sort or a filter keeps them even.
                if r % 2 == 1 && t.zebra != t.bg {
                    window.paint_quad(fill(rect(cells_left, y, right, y + rh), t.zebra));
                }
                if self.mark == Some(d) {
                    window.paint_quad(fill(rect(x0, y, right, y + rh), t.mark));
                }
                if in_rows {
                    // A row number in the selection; a second layer when its whole row is selected.
                    window.paint_quad(fill(rect(x0, y, cells_left, y + rh), t.sel));
                    if whole_rows {
                        window.paint_quad(fill(rect(x0, y, cells_left, y + rh), t.sel));
                    }
                    if let Some((_, l, _, r)) = bounds {
                        let (a, z) = (x_of(l.max(first_k)).max(cells_left), x_of((r + 1).min(last_k + 1)));
                        window.paint_quad(fill(rect(a, y, z, y + rh), t.sel));
                    }
                }
            });
            let fields = doc.row_upto(d, need);
            window.with_content_mask(Some(ContentMask { bounds: cells }), |window| {
                for (k, &c) in shown.iter().enumerate().take(last_k + 1).skip(first_k) {
                    let raw = fields.get(c).map_or("", String::as_str);
                    let dt = doc.dtypes.get(c).copied().unwrap_or(Dtype::String);
                    let rd = doc.reads.get(c).copied().unwrap_or(Reading { comma: false, order: Order::Ymd, short_year: false });
                    let shown_as = show(raw, dt, rd, self.shows.get(c).unwrap_or(&Show::Plain));
                    let value = shown_as.as_deref().unwrap_or(raw);
                    if value.is_empty() {
                        continue;
                    }
                    let numeric = matches!(dt, Dtype::Int | Dtype::Float | Dtype::Time);
                    let color = match dt {
                        Dtype::Date | Dtype::DateTime => t.date,
                        _ if is_zero(dt, rd, raw) => t.muted,
                        _ => t.ink,
                    };
                    self.cell(window, cx, value, color, (x_of(k) + PAD, x_of(k + 1) - PAD, y), numeric, base);
                }
            });
            window.with_content_mask(Some(ContentMask { bounds: nums }), |window| {
                // Dark in the selection, as the column names are.
                let (num, color) = (sep(d + 1), if in_rows { t.ink } else { t.muted });
                if !self.quick(window, &num, (&num_font, head, &self.num_glyphs), color, (x0 + PAD, cells_left - PAD, y), true, num_base) {
                    self.text(window, cx, &num, (&num_font, head, color), (x0 + PAD, cells_left - PAD, y, rh), true);
                }
            });
            ys.push(y + rh);
        }
        // A line under each row, after all the row fills.
        window.with_content_mask(Some(ContentMask { bounds: rect(x0, body_top, right, bottom) }), |window| {
            for y in ys {
                window.paint_quad(fill(rect(x0, y - 1.0, right, y), t.line_soft));
            }
        });
        // The active cell: a 2 px accent frame.
        if let Some(s) = self.sel {
            let (r, k) = (s.ar as f64 - top, s.ac);
            if k < shown.len() && r > -1.0 && r < vis + 1.0 {
                let y = body_top + (r * rh as f64) as f32;
                let (l, rr) = (x_of(k), x_of(k + 1));
                window.with_content_mask(Some(ContentMask { bounds: cells }), |window| {
                    for e in [rect(l, y, rr, y + 2.0), rect(l, y + rh - 2.0, rr, y + rh), rect(l, y, l + 2.0, y + rh), rect(rr - 2.0, y, rr, y + rh)] {
                        window.paint_quad(fill(e, t.accent));
                    }
                });
            }
        }
        if self.drew.get().is_none() && n > 0 {
            self.drew.set(Some(Instant::now()));
        }

        // The header: names and sort marks on the bar color, a line below. No column lines, as in the design.
        window.paint_quad(fill(rect(x0, y0, x0 + w, body_top), t.head));
        let head_font = Font { weight: FontWeight::SEMIBOLD, ..font("IBM Plex Sans") };
        let all_rows = bounds.is_some_and(|(t, _, b, _)| t == 0 && b + 1 == n);
        window.with_content_mask(Some(ContentMask { bounds: rect(cells_left, y0, right, body_top) }), |window| {
            for (k, &c) in shown.iter().enumerate().take(last_k + 1).skip(first_k) {
                let (l, r) = (x_of(k), x_of(k + 1));
                // The names of the selected columns, as a spreadsheet marks them; a second layer for whole columns.
                if in_cols(k) {
                    window.paint_quad(fill(rect(l, y0, r, body_top), t.sel));
                    if all_rows {
                        window.paint_quad(fill(rect(l, y0, r, body_top), t.sel));
                    }
                }
                let (mark, color) = match self.sort.iter().position(|s| s.col == c) {
                    Some(p) => {
                        let arrow = if self.sort[p].dir == Dir::Asc { "▲" } else { "▼" };
                        (if self.sort.len() > 1 { format!("{arrow}{}", p + 1) } else { arrow.to_string() }, t.accent)
                    }
                    None => ("↕".to_string(), t.line_strong),
                };
                let mark_w = head * if self.sort.len() > 1 { 1.8 } else { 1.0 };
                self.text(window, cx, &doc.names[c], (&head_font, head, if in_cols(k) { t.ink } else { t.muted }), (l + PAD, r - PAD - mark_w - 4.0, y0, hh), false);
                self.text(window, cx, &mark, (&head_font, head * 0.85, color), (r - PAD - mark_w, r - PAD, y0, hh), true);
            }
        });
        self.text(window, cx, "#", (&head_font, head, t.muted), (x0 + PAD, cells_left - PAD, y0, hh), true);
        // A line under the column names and one right of the row numbers: the two headers.
        window.paint_quad(fill(rect(x0, body_top - 1.0, x0 + w, body_top), t.line));
        window.paint_quad(fill(rect(cells_left - 1.0, y0, cells_left, bottom), t.line));
        // Where a dragged column name would drop.
        if let Some(g) = self.drop {
            let x = x_of(g.min(shown.len()));
            window.with_content_mask(Some(ContentMask { bounds: rect(cells_left, y0, right, bottom) }), |window| {
                window.paint_quad(fill(rect(x - 1.0, y0, x + 1.0, bottom), t.accent));
            });
        }

        // Scroll bars.
        if max_top > 0.0 {
            let y = body_top + (vtrack - vthumb) * (top / max_top) as f32;
            window.paint_quad(fill(rect(right + 2.0, y, right + BAR - 2.0, y + vthumb), t.thumb).corner_radii(px(3.0)));
        }
        if max_left > 0.0 {
            let x = cells_left + (htrack - hthumb) * (left / max_left);
            window.paint_quad(fill(rect(x, bottom + 2.0, x + hthumb, bottom + BAR - 2.0), t.thumb).corner_radii(px(3.0)));
        }
        *self.layout.borrow_mut() = layout;
    }
}

#[cfg(test)]
mod tests {
    // Named, not `super::*`: gpui-kit's glob brings a `test` macro that shadows Rust's.
    use super::{Hit, Layout, Tokens};
    use gpui_kit::{Hsla, Rgba, point, px};

    fn layout() -> Layout {
        // Header 0..30, rows of 30 from y 30; row numbers 0..50; columns 100 wide from x 50.
        Layout {
            x0: 0.0,
            y0: 0.0,
            right: 400.0,
            bottom: 300.0,
            body_top: 30.0,
            cells_left: 50.0,
            rh: 30.0,
            head: 12.0,
            xs: vec![0.0, 100.0, 200.0, 300.0],
            top: 10.0,
            left: 0.0,
            n: 1000,
            vis: 9.0,
            max_top: 991.0,
            max_left: 0.0,
            vthumb: 24.0,
            hthumb: 350.0,
        }
    }

    #[test]
    fn a_drag_past_the_cells_scrolls_faster_the_farther_out() {
        let l = layout();
        let at = |x: f32, y: f32, rows: bool| l.edge_scroll(point(px(x), px(y)), rows);
        assert_eq!(at(200.0, 150.0, false), (0.0, 0.0), "inside the cells");
        assert_eq!(at(200.0, 340.0, false), (2.0, 0.0), "40 px below: 2 rows a frame");
        assert_eq!(at(200.0, 301.0, false), (0.2, 0.0), "just below: slow");
        assert_eq!(at(200.0, 5000.0, false), (10.0, 0.0), "far below: at most 10");
        assert_eq!(at(200.0, 10.0, false), (-1.0, 0.0), "over the column names: up");
        assert_eq!(at(430.0, 150.0, false), (0.0, 30.0), "right of the cells");
        assert_eq!(at(20.0, 150.0, false), (0.0, -30.0), "over the row numbers: left");
        assert_eq!(at(430.0, 340.0, true), (2.0, 0.0), "a row drag moves rows only");
    }
    #[test]
    fn the_mouse_hits_what_the_paint_put_there() {
        let l = layout();
        let at = |x: f32, y: f32| l.hit(point(px(x), px(y)));
        assert_eq!(at(60.0, 45.0), Hit::Cell(10, 0), "first row on screen is view row 10");
        assert_eq!(at(260.0, 75.0), Hit::Cell(11, 2));
        assert_eq!(at(20.0, 75.0), Hit::Row(11));
        assert_eq!(at(80.0, 15.0), Hit::Head(0));
        assert_eq!(at(160.0, 15.0), Hit::Head(1));
        assert_eq!(at(147.0, 15.0), Hit::Edge(0), "4 px either side of the edge at x 150");
        assert_eq!(at(153.0, 15.0), Hit::Edge(0));
        assert_eq!(at(145.0, 15.0), Hit::Sort(0), "5 px from the edge is the sort mark");
        assert_eq!(at(230.0, 15.0), Hit::Sort(1), "the last PAD + head px of a name");
        assert_eq!(at(405.0, 100.0), Hit::VBar);
        assert_eq!(at(200.0, 305.0), Hit::HBar);
        assert_eq!(at(20.0, 15.0), Hit::None);
        let mut short = l.clone();
        short.n = 12;
        assert_eq!(short.hit(point(px(60.0), px(100.0))), Hit::None, "below the last row");
    }
    #[test]
    fn soft_dark_text_reads() {
        // WCAG contrast: 4.5:1 is the AA level for body text.
        let lum = |c: Hsla| {
            let c = Rgba::from(c);
            let f = |x: f32| if x <= 0.03928 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) };
            0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b)
        };
        let ratio = |a: Hsla, b: Hsla| (lum(a).max(lum(b)) + 0.05) / (lum(a).min(lum(b)) + 0.05);
        let t = Tokens::dark();
        for (fg, name) in [(t.ink, "ink"), (t.muted, "muted")] {
            for bg in [t.bg, t.bar, t.head, t.field, t.hover] {
                assert!(ratio(fg, bg) >= 4.5, "{name} on {bg:?}: {:.2}", ratio(fg, bg));
            }
        }
        // The cell text on the cell fills, in both themes: dates, zeros (muted), and the rest.
        for t in [Tokens::light(), Tokens::dark()] {
            for (fg, name) in [(t.date, "date"), (t.muted, "muted"), (t.ink, "ink")] {
                for bg in [t.bg, t.zebra, t.nums] {
                    assert!(ratio(fg, bg) >= 4.5, "{name} on {bg:?}: {:.2}", ratio(fg, bg));
                }
            }
        }
    }
}
