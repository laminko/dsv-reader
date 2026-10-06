//! The side pane (the Record viewer and the Profile) and the filter rows, in the toolbar's
//! style. The pane sits right of the grid or below it, at the size kept in the settings; with
//! both panes on, they split it, and the one turned on first stays on top.

use crate::dsv::{Distinct, Doc, Dtype, Family, Filter, Op, Order, Profile, Reading, Values, arity, bin_range, date_label, family, ops, show};
use crate::grid::{Tokens, count, sep};
use crate::toolbar::{bare_input, btn, caption, choose, field_box, focused, icon, icon_btn, outline, panel};
use crate::{Pane, Reader, Tab};
use gpui_kit::assets::IconName;
use gpui_kit::base::SelectableText;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{Disableable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// A statistic: up to 4 decimals, with thousands separators in the whole part.
pub fn stat(x: f64) -> String {
    if !x.is_finite() || x.abs() >= 1e18 {
        return format!("{x}");
    }
    let s = format!("{:.4}", x.abs());
    let s = s.trim_end_matches('0').trim_end_matches('.');
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    let sign = if x < 0.0 && s != "0" { "-" } else { "" };
    let whole = sep(whole.parse().unwrap_or(0));
    if frac.is_empty() { format!("{sign}{whole}") } else { format!("{sign}{whole}.{frac}") }
}

/// Up to this many rows, the Profile takes all of them unless told otherwise: it is fast.
pub const AUTO_ALL: usize = 100_000;

/// When Exact must hold millions of values, Approximate is the recommended choice.
pub fn recommended(rows: usize) -> Distinct {
    if rows > 1_000_000 { Distinct::Approx } else { Distinct::Exact }
}

/// The Profile's choices, its results so far, and its job.
pub struct Prof {
    /// All rows, or the first 1,000; None: all when the rows are at most AUTO_ALL.
    pub all: Option<bool>,
    /// The rows that search and filters leave.
    pub filters: bool,
    /// None follows the recommendation.
    pub distinct: Option<Distinct>,
    /// The columns to profile; None is every column, in display order.
    pub cols: Option<Vec<usize>>,
    /// Filled by the job's two threads, one column after the other: the basic statistics, and
    /// the value counts (`Scan::values`), which hold the most memory, one column at a time.
    pub out: Arc<Mutex<Vec<Profile>>>,
    pub counts: Arc<Mutex<Vec<Values>>>,
    /// Milliseconds, when both threads are done; and when the first card's statistics came.
    pub ms: Arc<Mutex<Option<f64>>>,
    pub first_ms: Arc<Mutex<Option<f64>>>,
    pub rows: usize,
    /// The rows there were to profile: more than `rows` when it took the first 1,000.
    pub total: usize,
    pub ncols: usize,
    stop: Option<Arc<AtomicBool>>,
    _task: Option<(Task<()>, Option<Task<()>>)>,
    /// A choice, the rows, or a type changed: profile again.
    pub dirty: bool,
}

impl Default for Prof {
    fn default() -> Self {
        Prof {
            all: None,
            filters: true,
            distinct: None,
            cols: None,
            out: Arc::default(),
            counts: Arc::default(),
            ms: Arc::default(),
            first_ms: Arc::default(),
            rows: 0,
            total: 0,
            ncols: 0,
            stop: None,
            _task: None,
            dirty: true,
        }
    }
}

impl Prof {
    /// All rows, by the choice, or else when there are at most AUTO_ALL: their counts then match a click's filter.
    pub fn all_rows(&self, total: usize) -> bool {
        self.all.unwrap_or(total <= AUTO_ALL)
    }

    pub fn running(&self) -> bool {
        self._task.is_some() && self.ms.lock().unwrap().is_none()
    }

    pub fn done_ms(&self) -> Option<f64> {
        *self.ms.lock().unwrap()
    }

    /// The columns whose basic statistics are in, with their value counts when those are in too.
    pub fn merged(&self) -> Vec<Profile> {
        let counts = self.counts.lock().unwrap();
        let mut out = self.out.lock().unwrap().clone();
        for (p, v) in out.iter_mut().filter(|p| !p.counted).zip(counts.iter()) {
            p.count(v.clone());
        }
        out
    }

    pub fn stop(&mut self) {
        if let Some(s) = self.stop.take() {
            s.store(true, Ordering::Relaxed);
        }
        self._task = None;
    }
}

/// The text boxes of one filter row.
pub struct FilterRow {
    pub a: Entity<InputState>,
    pub b: Entity<InputState>,
    _subs: [Subscription; 2],
}

fn hint(f: Family) -> &'static str {
    match f {
        Family::Date => "2024-01-31",
        Family::Number => "1.5",
        _ => "text",
    }
}

/// The step between the values a column reads to: 1 for whole numbers, 1000 ms for dates (they
/// read to the second), 0 for decimals.
fn step(ty: Dtype) -> f64 {
    match ty {
        Dtype::Int => 1.0,
        Dtype::Date => 1000.0,
        _ => 0.0,
    }
}

/// A value of a number or date column as a filter box takes it back, exactly.
fn value_text(x: f64, ty: Dtype) -> String {
    match ty {
        Dtype::Date => date_label(x as i64),
        Dtype::Int => format!("{}", x as i64),
        _ => format!("{x}"),
    }
}

/// The filter "between" the lowest and highest value of bar `i`; None for a bar that holds none.
pub fn bar_filter(col: usize, ty: Dtype, span: (f64, f64), i: usize) -> Option<Filter> {
    let (lo, hi) = bin_range(span, i, step(ty));
    (lo <= hi).then(|| Filter { col, op: Op::Between, a: value_text(lo, ty), b: value_text(hi, ty), or: false })
}

/// The bars of a histogram, lowest values on the left, the lowest and highest under them.
/// A click on a bar with rows filters to them; its tooltip names its range.
fn histogram(p: &Profile, ty: Dtype, (lo, hi): (String, String), t: Tokens, cx: &mut Context<Reader>) -> Div {
    let max = p.hist.iter().copied().max().unwrap_or(1).max(1) as f32;
    let (col, span) = (p.col, p.span.unwrap_or_default());
    v_flex()
        .gap(px(4.))
        .child(h_flex().h(px(44.)).items_end().gap(px(2.)).children(p.hist.iter().enumerate().map(|(i, &n)| {
            let f = (n > 0).then(|| bar_filter(col, ty, span, i)).flatten();
            let tip = f.as_ref().map(|f| format!("{} – {} · {} rows · click to filter", f.a, f.b, sep(n)));
            // The bar's whole column takes the click: a short bar stays easy to hit.
            div()
                .id((SharedString::from(format!("bar-{col}")), i))
                .flex_1()
                .h_full()
                .flex()
                .items_end()
                .child(div().w_full().h(px((n as f32 / max * 44.0).max(if n > 0 { 1.0 } else { 0.0 }))).rounded_t(px(2.)).bg(t.accent.opacity(0.55)))
                .when_some(f.zip(tip), |d, (f, tip)| {
                    d.cursor_pointer().hover(|s| s.bg(t.hover)).tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)).on_click(cx.listener(
                        move |r, _: &ClickEvent, window, cx| {
                            if !r.filters.contains(&f) {
                                r.add_filter(Some(f.clone()), window, cx);
                            }
                        },
                    ))
                })
        })))
        .child(h_flex().justify_between().text_size(px(11.)).text_color(t.muted).child(lo).child(hi))
}

/// The top values as bars, most common first. A click on one filters the rows to it.
fn top_bars(col: usize, top: &[(String, usize)], approx: bool, t: Tokens, cx: &mut Context<Reader>) -> Div {
    let max = top.first().map_or(1, |x| x.1).max(1) as f32;
    v_flex().gap(px(1.)).children(top.iter().enumerate().map(|(i, (v, n))| {
        let value = v.clone();
        h_flex()
            .id((SharedString::from(format!("top-{col}")), i))
            // The hover box reaches 6 px past the text on each side; the text stays in line.
            .px(px(6.))
            .mx(px(-6.))
            .py(px(2.))
            .rounded(px(4.))
            .cursor_pointer()
            .hover(|s| s.bg(t.hover))
            .on_click(cx.listener(move |r, _: &ClickEvent, window, cx| r.filter_value(col, value.clone(), window, cx)))
            .gap(px(8.))
            .text_size(px(12.))
            .child(div().w(px(130.)).flex_none().truncate().text_color(t.ink).child(v.clone()))
            .child(div().flex_1().h(px(8.)).rounded(px(2.)).bg(t.line_soft).child(div().h_full().w(relative(*n as f32 / max)).rounded(px(2.)).bg(t.accent.opacity(0.55))))
            .child(div().w(px(84.)).flex_none().text_right().text_color(t.muted).child(format!("{}{}", if approx { "about " } else { "" }, sep(*n))))
    }))
}

/// One line of a column's statistics: the name, the value, and its share of the rows.
/// Every second line sits on the bar color, so a long table reads across.
fn stat_row(i: usize, (label, value, pct): (String, String, Option<f64>), t: Tokens) -> Div {
    h_flex()
        .px(px(8.))
        .py(px(4.))
        .gap(px(8.))
        .rounded(px(4.))
        .text_size(px(12.))
        .when(i % 2 == 1, |d| d.bg(t.bar))
        .child(div().w(px(104.)).flex_none().text_color(t.muted).child(label))
        .child(div().flex_1().min_w_0().truncate().text_right().text_color(t.ink).child(value))
        .child(div().w(px(48.)).flex_none().text_right().text_color(t.muted).child(pct.map_or(String::new(), |p| format!("{p:.1}%"))))
}

/// A length of time in ms as its two largest units: "8 days 23 hours", "41 minutes 9 seconds";
/// a year or more in years and days: "25.0 years (9,128 days)".
fn span_text(ms: f64) -> String {
    let s = (ms / 1000.0).round() as u64;
    if s >= 365 * 86_400 {
        return format!("{:.1} years ({} days)", s as f64 / (365.2425 * 86_400.0), sep((s / 86_400) as usize));
    }
    let units = [(s / 86_400, "day"), (s / 3600 % 24, "hour"), (s / 60 % 60, "minute"), (s % 60, "second")];
    let parts: Vec<String> = units.iter().skip_while(|u| u.0 == 0).take(2).filter(|u| u.0 > 0).map(|&(n, u)| format!("{n} {u}{}", if n == 1 { "" } else { "s" })).collect();
    if parts.is_empty() { "0 seconds".into() } else { parts.join(" ") }
}

/// One column of the Profile pane: a table of its statistics, then its value distribution.
fn profile_card(doc: &Doc, p: &Profile, sampled: bool, t: Tokens, cx: &mut Context<Reader>) -> Div {
    let ty = doc.dtypes.get(p.col).copied().unwrap_or(Dtype::String);
    let pct = |k: usize| if p.rows == 0 { 0.0 } else { k as f64 * 100.0 / p.rows as f64 };
    let about = if p.approx { "about " } else { "" };
    let row = |label: &str, value: String, share: Option<usize>| (label.to_string(), value, share.map(pct));
    let full = p.rows - p.empty;
    let mut groups = vec![vec![row("Count", sep(p.rows), None), row("Empty", sep(p.empty), Some(p.empty)), row("Not empty", sep(full), Some(full))]];
    let exact_only = || "— (Exact only)".to_string();
    let most = match p.top.first() {
        Some((_, 1)) if !p.approx => "— (every value appears once)".to_string(),
        Some((v, n)) => format!("{v} ({about}{})", sep(*n)),
        None => "—".to_string(),
    };
    // The value counts come from a second thread: "…" until they are in.
    let wait = || "…".to_string();
    let mut counts = if p.counted {
        vec![
            row("Distinct", format!("{about}{}", sep(p.distinct)), None),
            row("Unique", p.unique.map_or_else(exact_only, sep), None),
            row("Repeated", p.unique.map_or_else(exact_only, |u| sep(p.distinct.saturating_sub(u))), None),
            row("Most common", most, None),
        ]
    } else {
        ["Distinct", "Unique", "Repeated", "Most common"].map(|l| row(l, wait(), None)).to_vec()
    };
    if ty != Dtype::String {
        counts.push(row("Not readable", sep(p.unreadable), Some(p.unreadable)));
    }
    groups.push(counts);
    match ty {
        Dtype::String => {
            let mut g = Vec::new();
            if let Some((lo, hi)) = &p.range {
                g.extend([row("Min", lo.clone(), None), row("Max", hi.clone(), None)]);
            }
            if let Some((a, m, b)) = p.len {
                g.extend([row("Shortest", format!("{a} characters"), None), row("Longest", format!("{b} characters"), None), row("Mean length", format!("{m:.1}"), None)]);
            }
            groups.push(g);
        }
        Dtype::Int | Dtype::Float => {
            if let Some(s) = p.num {
                groups.push(vec![
                    row("Min", stat(s.min), None),
                    row("Max", stat(s.max), None),
                    row("Mean", stat(s.mean), None),
                    row("Median", stat(s.median), None),
                    row("Std dev", stat(s.std), None),
                    row("Sum", stat(s.sum), None),
                ]);
            }
        }
        Dtype::Date => {
            if let Some((lo, hi)) = p.span {
                groups.push(vec![row("Earliest", date_label(lo as i64), None), row("Latest", date_label(hi as i64), None), row("Span", span_text(hi - lo), None)]);
            }
        }
        Dtype::Bool => {
            let count = |w: &str| p.top.iter().filter(|(v, _)| v.eq_ignore_ascii_case(w)).map(|v| v.1).sum::<usize>();
            let (yes, no) = (count("true"), count("false"));
            groups.push(match p.counted {
                true => vec![row("True", format!("{about}{}", sep(yes)), Some(yes)), row("False", format!("{about}{}", sep(no)), Some(no))],
                false => vec![row("True", wait(), None), row("False", wait(), None)],
            });
        }
    }
    let mut card = v_flex().gap(px(8.)).py(px(12.)).border_b_1().border_color(t.line_soft).child(
        h_flex()
            .gap(px(8.))
            .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink).child(doc.names.get(p.col).cloned().unwrap_or_default()))
            .child(div().text_size(px(11.)).text_color(t.muted).child(ty.name())),
    );
    for g in groups.into_iter().filter(|g| !g.is_empty()) {
        card = card.child(v_flex().children(g.into_iter().enumerate().map(|(i, r)| stat_row(i, r, t))));
    }
    let caption = |s: &str| div().pt(px(4.)).text_size(px(11.)).text_color(t.muted).child(s.to_string());
    // A sample's counts are of the first 1,000 rows; a click's filter takes all rows.
    let (bars, values) = match (sampled, p.approx) {
        (true, true) => ("Value distribution in the first 1,000 rows · a click filters all rows", "Value distribution in the first 1,000 rows (estimates) · a click filters all rows"),
        (true, false) => ("Value distribution in the first 1,000 rows · a click filters all rows", "Value distribution in the first 1,000 rows · a click filters all rows"),
        (false, true) => ("Value distribution · click a bar to filter", "Value distribution (estimates) · click one to filter"),
        (false, false) => ("Value distribution · click a bar to filter", "Value distribution · click one to filter"),
    };
    match (ty, p.span) {
        (Dtype::Date, Some((lo, hi))) => card = card.child(caption(bars)).child(histogram(p, ty, (date_label(lo as i64), date_label(hi as i64)), t, cx)),
        (Dtype::Int | Dtype::Float, Some((lo, hi))) => card = card.child(caption(bars)).child(histogram(p, ty, (stat(lo), stat(hi)), t, cx)),
        _ => {}
    }
    if matches!(ty, Dtype::String | Dtype::Bool) && !p.counted {
        card = card.child(caption("Value distribution · counting…"));
    }
    if matches!(ty, Dtype::String | Dtype::Bool) && !p.top.is_empty() {
        card = card.child(caption(values)).child(top_bars(p.col, &p.top, p.approx, t, cx));
    }
    card
}

/// The filter for the rows with a top value: equals it, is empty, or is true or false.
fn value_filter(col: usize, ty: Dtype, value: String) -> Filter {
    let op = match family(ty) {
        Family::Bool if value.trim().eq_ignore_ascii_case("true") => Op::True,
        Family::Bool => Op::False,
        _ if value.is_empty() => Op::Empty,
        _ => Op::Is,
    };
    Filter { col, op, a: if arity(op) > 0 { value } else { String::new() }, b: String::new(), or: false }
}

/// A small choice chip, 24 px, the accent border when chosen.
fn chip(id: impl Into<ElementId>, on: bool, label: impl Into<SharedString>, t: Tokens) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(26.))
        .px(px(10.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .border_1()
        .border_color(if on { t.accent } else { t.line })
        .bg(if on { t.sel } else { t.field })
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(if on { t.accent } else { t.ink })
        .hover(|s| s.border_color(t.accent))
        .child(label.into())
}

/// A pane's head: its tabs or title at the left, its buttons at the right.
fn pane_head(left: impl IntoElement, right: impl IntoElement, t: Tokens) -> Div {
    h_flex().h(px(44.)).flex_none().px(px(12.)).gap(px(8.)).justify_between().border_b_1().border_color(t.line).bg(t.bar).child(left).child(right)
}

impl Reader {
    /// A Viewer or Profile button: on, or off. A pane turned on goes after the ones already on.
    pub fn toggle_pane(&mut self, p: Pane, cx: &mut Context<Self>) {
        let on = match p {
            Pane::Viewer => &mut self.viewer,
            Pane::Profile => &mut self.profile,
        };
        *on = !*on;
        if *on {
            self.panes.push(p);
        } else {
            self.panes.retain(|x| *x != p);
        }
        if p == Pane::Profile && self.profile {
            self.prof.dirty = true;
        }
        cx.notify();
    }

    /// Starts the Profile for the current choices, once the index is done. A running one stops.
    pub fn start_profile(&mut self, cx: &mut Context<Self>) {
        let Some(g) = &self.grid else { return };
        let Some(scan) = g.doc.scan() else { return };
        self.prof.dirty = false;
        self.prof.stop();
        let view = if self.prof.filters { g.view.clone() } else { None };
        let total = view.as_ref().map_or(scan.rows(), |v| v.len());
        let limit = (!self.prof.all_rows(total)).then_some(1000);
        let rows = total.min(limit.unwrap_or(usize::MAX));
        self.prof.total = total;
        let distinct = self.prof.distinct.unwrap_or(recommended(rows));
        let cols = self.prof.cols.clone().unwrap_or_else(|| g.order.clone());
        let stop = Arc::new(AtomicBool::new(false));
        (self.prof.out, self.prof.counts, self.prof.ms, self.prof.first_ms, self.prof.rows, self.prof.ncols, self.prof.stop) =
            (Arc::default(), Arc::default(), Arc::default(), Arc::default(), rows, cols.len(), Some(stop.clone()));
        // Two threads, each over every column in order: the basic statistics come fast, so the
        // cards show early; the Exact value counts follow, one column's memory at a time (Approx
        // counts are small and come with the basic ones). The main thread is not woken per
        // column, so a frame in progress does not hold up the next one. Frames show the
        // results. The last thread to finish sets the time.
        let exact = distinct == Distinct::Exact;
        let (t, left) = (Instant::now(), Arc::new(AtomicUsize::new(if exact { 2 } else { 1 })));
        let finish = move |left: &AtomicUsize, ms: &Mutex<Option<f64>>| {
            if left.fetch_sub(1, Ordering::AcqRel) == 1 {
                *ms.lock().unwrap() = Some(t.elapsed().as_secs_f64() * 1e3);
            }
        };
        let (out, ms, first, l, s, sc, v, cs) = (self.prof.out.clone(), self.prof.ms.clone(), self.prof.first_ms.clone(), left.clone(), stop.clone(), scan.clone(), view.clone(), cols.clone());
        let basic = cx.background_spawn(async move {
            for c in cs {
                let p = sc.basic(v.as_deref().map(Vec::as_slice), limit, c, distinct, &s);
                if s.load(Ordering::Relaxed) {
                    return;
                }
                out.lock().unwrap().push(p);
                first.lock().unwrap().get_or_insert(t.elapsed().as_secs_f64() * 1e3);
            }
            finish(&l, &ms);
        });
        let (counts, ms) = (self.prof.counts.clone(), self.prof.ms.clone());
        let values = exact.then(|| {
            cx.background_spawn(async move {
                for c in cols {
                    let v = scan.values(view.as_deref().map(Vec::as_slice), limit, c, distinct, &stop);
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    counts.lock().unwrap().push(v);
                }
                finish(&left, &ms);
            })
        });
        self.prof._task = Some((basic, values));
        cx.notify();
    }

    /// The side pane, right of the grid or below it, with a line to drag its size; or nothing.
    pub fn side_pane(&mut self, t: Tokens, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.panes.is_empty() || self.grid.is_none() {
            return None;
        }
        let bottom = self.settings.pane_bottom;
        let panes: Vec<AnyElement> = self.panes.clone().into_iter().map(|p| self.pane(p, t, window, cx)).collect();
        let n = panes.len();
        // With both panes: the upper one takes the `split` share, and a line between them drags it.
        let edge = div()
            .id("split-edge")
            .flex_none()
            .h(px(5.))
            .w_full()
            .flex()
            .items_center()
            .cursor(CursorStyle::ResizeUpDown)
            .child(div().h(px(1.)).w_full().bg(if self.split_drag.is_some() { t.accent } else { t.line }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|r, e: &MouseDownEvent, _, cx| {
                    r.split_drag = Some((f32::from(e.position.y), r.settings.split));
                    cx.notify();
                }),
            );
        let mut body = v_flex().size_full().relative();
        let mut edge = Some(edge);
        for (i, p) in panes.into_iter().enumerate() {
            let d = div().min_h_0().overflow_hidden().child(p);
            if i == 1 {
                body = body.children(edge.take());
            }
            body = body.child(if i == 0 && n == 2 { d.flex_basis(relative(self.settings.split)).flex_shrink_0() } else { d.flex_1() });
        }
        // Its height, for a drag on the line: a share of it per pixel.
        let h = self.pane_h.clone();
        let body = body.child(canvas(move |b, _, _| h.set(f32::from(b.size.height)), |_, _, _, _| {}).absolute().size_full());
        // A 1 px line to see, in a 5 px strip to grab.
        let line = div().bg(if self.pane_drag.is_some() { t.accent } else { t.line }).when(bottom, |d| d.h(px(1.)).w_full()).when(!bottom, |d| d.w(px(1.)).h_full());
        let handle = div()
            .id("pane-edge")
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .when(bottom, |d| d.h(px(5.)).w_full().cursor(CursorStyle::ResizeUpDown))
            .when(!bottom, |d| d.w(px(5.)).h_full().cursor(CursorStyle::ResizeLeftRight))
            .child(line)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |r, e: &MouseDownEvent, _, cx| {
                    let at = if r.settings.pane_bottom { e.position.y } else { e.position.x };
                    r.pane_drag = Some((f32::from(at), r.settings.pane_size));
                    cx.notify();
                }),
            );
        // A narrow window keeps 240 px for the grid, as a drag does; the saved size stays.
        let whole = f32::from(if bottom { window.viewport_size().height } else { window.viewport_size().width });
        let size = px(self.settings.pane_size.min((whole - 240.0).max(160.0)));
        let pane = div().flex_none().bg(t.bg).overflow_hidden().when(bottom, |d| d.h(size).w_full()).when(!bottom, |d| d.w(size).h_full()).child(body);
        Some(if bottom { v_flex().w_full().flex_none().child(handle).child(pane).into_any_element() } else { h_flex().h_full().flex_none().child(handle).child(pane).into_any_element() })
    }

    fn pane(&mut self, p: Pane, t: Tokens, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match p {
            Pane::Viewer => self.viewer_pane(t, cx).into_any_element(),
            Pane::Profile => self.profile_pane(t, window, cx).into_any_element(),
        }
    }

    /// The Record viewer: the whole active cell, or the active row as a form.
    fn viewer_pane(&mut self, t: Tokens, cx: &mut Context<Self>) -> Div {
        let tab = self.tab;
        let tabs = h_flex()
            .gap(px(6.))
            .child(chip("tab-cell", tab == Tab::Cell, "Cell", t).on_click(cx.listener(|r, _: &ClickEvent, _, cx| (r.tab = Tab::Cell, cx.notify()).1)))
            .child(chip("tab-record", tab == Tab::Record, "Record", t).on_click(cx.listener(|r, _: &ClickEvent, _, cx| (r.tab = Tab::Record, cx.notify()).1)));
        // The pane's side as an icon: the panel it would move to.
        let (side_icon, side_tip) = if self.settings.pane_bottom { (IconName::PanelRight, "Move right") } else { (IconName::PanelBottom, "Move below") };
        let buttons = h_flex()
            .gap(px(2.))
            .child(icon_btn("pane-side", side_icon, side_tip, t).w(px(28.)).h(px(26.)).on_click(cx.listener(|r, _: &ClickEvent, _, cx| {
                r.settings.pane_bottom = !r.settings.pane_bottom;
                r.settings.save();
                cx.notify();
            })))
            .child(icon_btn("viewer-close", IconName::X, "Close the Record viewer", t).w(px(28.)).h(px(26.)).on_click(cx.listener(|r, _: &ClickEvent, _, cx| r.toggle_pane(Pane::Viewer, cx))));
        let head = pane_head(tabs, buttons, t);
        let pane = v_flex().size_full().child(head);
        let empty = |s: &str| div().p(px(16.)).text_size(px(12.5)).text_color(t.muted).child(s.to_string());
        let Some(g) = &self.grid else { return pane };
        let Some(s) = g.sel else { return pane.child(empty("Click a cell to see it here.")) };
        let shown = g.shown();
        let n = g.len();
        if s.ar >= n || shown.is_empty() {
            return pane;
        }
        let d = g.view.as_ref().map_or(s.ar, |v| v[s.ar] as usize);
        let row = g.doc.row(d);
        let mono = g.font.clone();
        let size = px(g.size);
        // Selectable values. An id per row and column: another cell starts with no selection.
        let stride = g.doc.names.len().max(1);
        // A cell as the grid shows it: its format, or the text of the file.
        let value = |c: usize| {
            let raw = row.get(c).cloned().unwrap_or_default();
            let ty = g.doc.dtypes.get(c).copied().unwrap_or(Dtype::String);
            let rd = g.doc.reads.get(c).copied().unwrap_or(Reading { comma: false, order: Order::Ymd });
            let shown_as = g.fmts.get(c).and_then(|f| show(&raw, ty, rd, &f.show));
            (raw, shown_as, ty)
        };
        match tab {
            Tab::Cell => {
                let c = shown[s.ac.min(shown.len() - 1)];
                let (v, shown_as, _) = value(c);
                pane.child(
                    v_flex()
                        .id("viewer-cell")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .p(px(16.))
                        .gap(px(8.))
                        .child(
                            h_flex()
                                .gap(px(8.))
                                .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink).child(g.doc.names[c].clone()))
                                .child(div().text_size(px(12.)).text_color(t.muted).child(format!("row {} · {} characters", sep(d + 1), sep(v.chars().count())))),
                        )
                        .when_some(shown_as, |d, s| d.child(div().text_size(px(12.)).text_color(t.muted).child(format!("Shown as {s}"))))
                        .child(
                            div()
                                .p(px(10.))
                                .rounded(px(6.))
                                .border_1()
                                .border_color(t.line)
                                .bg(t.field)
                                .font(mono)
                                .text_size(size)
                                .text_color(t.ink)
                                .child(SelectableText::new(("cell-text", (d * stride + c) as u64), v).selection_color(t.accent.opacity(0.25))),
                        ),
                )
            }
            Tab::Record => {
                // First, Previous, Next, Last: one group of icon buttons.
                let go = |id: &'static str, name: IconName, tip: &'static str, to: usize, off: bool| {
                    icon_btn(id, name, tip, t).w(px(30.)).h(px(28.)).disabled(off).when(off, |b| b.opacity(0.4)).on_click(cx.listener(move |r, _: &ClickEvent, _, cx| {
                        if let Some(g) = &mut r.grid {
                            g.sel = Some(crate::dsv::Sel::at(to, s.ac));
                            g.reveal();
                        }
                        cx.notify();
                    }))
                };
                let (at_first, at_last) = (s.ar == 0, s.ar + 1 >= n);
                let nav = h_flex()
                    .px(px(16.))
                    .py(px(10.))
                    .gap(px(10.))
                    .flex_none()
                    .border_b_1()
                    .border_color(t.line_soft)
                    .child(
                        h_flex()
                            .p(px(1.))
                            .gap(px(1.))
                            .rounded(px(7.))
                            .border_1()
                            .border_color(t.line)
                            .bg(t.field)
                            .child(go("rec-first", IconName::ChevronFirst, "First row", 0, at_first))
                            .child(go("rec-prev", IconName::ChevronLeft, "Previous row", s.ar.saturating_sub(1), at_first))
                            .child(go("rec-next", IconName::ChevronRight, "Next row", (s.ar + 1).min(n - 1), at_last))
                            .child(go("rec-last", IconName::ChevronLast, "Last row", n - 1, at_last)),
                    )
                    .child(
                        v_flex()
                            .child(div().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink).child(format!("Row {}", sep(d + 1))))
                            .child(div().text_size(px(11.)).text_color(t.muted).child(format!("{} of {} rows", sep(s.ar + 1), sep(n)))),
                    );
                // The form: a label column (name, then type) and the value in a read-only field.
                let fields = v_flex().id("viewer-record").flex_1().min_h_0().overflow_y_scroll().px(px(16.)).py(px(12.)).gap(px(8.)).children(shown.iter().enumerate().map(|(k, &c)| {
                    let (raw, shown_as, ty) = value(c);
                    let active = k == s.ac;
                    let text = shown_as.unwrap_or(raw);
                    let label = v_flex()
                        .w(px(112.))
                        .flex_none()
                        .pt(px(6.))
                        .gap(px(1.))
                        .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(if active { t.accent } else { t.ink }).truncate().child(g.doc.names[c].clone()))
                        .child(div().text_size(px(10.5)).text_color(t.muted).child(ty.name()));
                    let field = div()
                        .id(("rec-field", c))
                        .flex_1()
                        .min_w_0()
                        .min_h(px(30.))
                        .max_h(px(140.))
                        .overflow_y_scroll()
                        .px(px(10.))
                        .py(px(5.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(if active { t.accent } else { t.line })
                        .bg(t.field)
                        .font(mono.clone())
                        .text_size(size)
                        .when(text.is_empty(), |d| d.text_color(t.line_strong).child("—"))
                        .when(!text.is_empty(), |el| el.text_color(t.ink).child(SelectableText::new(("rec-text", (d * stride + c) as u64), text).selection_color(t.accent.opacity(0.25))));
                    h_flex().items_start().gap(px(12.)).child(label).child(field)
                }));
                pane.child(nav).child(fields)
            }
        }
    }

    /// The Profile: statistics for each column, over the first 1,000 rows or all rows, with or
    /// without the search and filters.
    fn profile_pane(&mut self, t: Tokens, _window: &mut Window, cx: &mut Context<Self>) -> Div {
        let Some(g) = &self.grid else { return div() };
        let filtered = if self.prof.filters { g.view.as_ref().map(|v| v.len()) } else { None };
        let total = filtered.unwrap_or(g.doc.rows());
        let all = self.prof.all_rows(total);
        let rows = if all { total } else { total.min(1000) };
        let best = recommended(rows);
        let distinct = self.prof.distinct.unwrap_or(best);
        let head = pane_head(
            div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink).child("Profile"),
            icon_btn("profile-close", IconName::X, "Close the Profile", t).w(px(28.)).h(px(26.)).on_click(cx.listener(|r, _: &ClickEvent, _, cx| r.toggle_pane(Pane::Profile, cx))),
            t,
        );
        let cols_label = self.prof.cols.as_ref().map_or("All columns".to_string(), |v| count(v.len(), "column"));
        let open = self.pop == Some("prof-cols");
        let list: Vec<(usize, String, bool)> = if open { g.order.iter().map(|&c| (c, g.doc.names[c].clone(), self.prof.cols.as_ref().is_none_or(|v| v.contains(&c)))).collect() } else { Vec::new() };
        let cols_pop = panel(self.popover("prof-cols", cx), t, 240.)
            .anchor(Anchor::TopRight)
            .offset(px(6.))
            .trigger(outline("prof-cols-btn", t).h(px(26.)).px(px(8.)).child(cols_label).dropdown_caret(true))
            .child(
                v_flex()
                    .gap(px(6.))
                    .child(caption("Profile columns", t))
                    .child(btn("prof-cols-all", t).h(px(26.)).px(px(8.)).child("All columns").on_click(cx.listener(|r, _: &ClickEvent, _, cx| {
                        (r.prof.cols, r.prof.dirty) = (None, true);
                        cx.notify();
                    })))
                    .child(div().id("prof-cols-list").max_h(px(360.)).overflow_y_scroll().children(list.into_iter().map(|(c, name, on)| {
                        Checkbox::new(("prof-col", c)).label(name).checked(on).py(px(3.)).on_change(cx.listener(move |r, on: &bool, _, cx| {
                            let order = r.grid.as_ref().map_or(Vec::new(), |g| g.order.clone());
                            let mut v = r.prof.cols.take().unwrap_or_else(|| order.clone());
                            if *on {
                                v.push(c);
                            } else {
                                v.retain(|&x| x != c);
                            }
                            v.sort_by_key(|x| order.iter().position(|o| o == x));
                            (r.prof.cols, r.prof.dirty) = (Some(v), true);
                            cx.notify();
                        }))
                    }))),
            );
        let set_all = |v: bool| {
            cx.listener(move |r: &mut Reader, _: &ClickEvent, _, cx| {
                if r.prof.all != Some(v) {
                    (r.prof.all, r.prof.dirty) = (Some(v), true);
                }
                cx.notify();
            })
        };
        let set_distinct = |d: Distinct| {
            cx.listener(move |r: &mut Reader, _: &ClickEvent, _, cx| {
                (r.prof.distinct, r.prof.dirty) = (Some(d), true);
                cx.notify();
            })
        };
        let label = |d: Distinct, name: &str| if d == best { format!("{name} (recommended)") } else { name.to_string() };
        let row_label = |s: &str| div().w(px(56.)).flex_none().text_size(px(12.)).text_color(t.muted).child(s.to_string());
        let controls = v_flex()
            .px(px(16.))
            .py(px(12.))
            .gap(px(8.))
            .flex_none()
            .border_b_1()
            .border_color(t.line_soft)
            .child(
                h_flex()
                    .gap(px(6.))
                    .child(row_label("Rows"))
                    .child(chip("prof-first", !all, if total > AUTO_ALL { "First 1,000 (recommended)" } else { "First 1,000" }, t).on_click(set_all(false)))
                    .child(chip("prof-all", all, if total <= AUTO_ALL { "All (recommended)" } else { "All" }, t).on_click(set_all(true))),
            )
            // On a line of its own: the chips with "(recommended)" take the room.
            .child(h_flex().gap(px(6.)).child(row_label("")).child(Checkbox::new("prof-filters").label("Use search and filters").checked(self.prof.filters).on_change(cx.listener(
                |r, on: &bool, _, cx| {
                    (r.prof.filters, r.prof.dirty) = (*on, true);
                    cx.notify();
                },
            ))))
            .child(
                h_flex()
                    .gap(px(6.))
                    .child(row_label("Distinct"))
                    .child(chip("prof-exact", distinct == Distinct::Exact, label(Distinct::Exact, "Exact"), t).on_click(set_distinct(Distinct::Exact)))
                    .child(chip("prof-approx", distinct == Distinct::Approx, label(Distinct::Approx, "Approximate"), t).on_click(set_distinct(Distinct::Approx))),
            )
            .child(h_flex().gap(px(6.)).child(row_label("Columns")).child(cols_pop))
            .when(distinct == Distinct::Exact && rows > 1_000_000, |d| {
                d.child(
                    div()
                        .text_size(px(12.))
                        .text_color(t.muted)
                        .child("Exact holds 16 bytes a row while it counts a column (about 100 MB for 6.5 million rows), and it takes longer than Approximate."),
                )
            })
            .when(distinct == Distinct::Approx, |d| d.child(div().text_size(px(12.)).text_color(t.muted).child("Approximate: the distinct count and the top values are estimates (about 1% error).")))
            .child(div().text_size(px(12.)).text_color(t.muted).child(match (self.prof._task.is_some(), self.prof.done_ms()) {
                (false, _) => "The profile starts when the index is done.".to_string(),
                (true, Some(ms)) if self.prof.rows < self.prof.total => {
                    format!("the first {} of {} · {} · {ms:.0} ms", sep(self.prof.rows), count(self.prof.total, "row"), count(self.prof.ncols, "column"))
                }
                (true, Some(ms)) => format!("{} · {} · {ms:.0} ms", count(self.prof.rows, "row"), count(self.prof.ncols, "column")),
                (true, None) => format!(
                    "{} · statistics {} of {} · value counts {} of {}…",
                    count(self.prof.rows, "row"),
                    self.prof.out.lock().unwrap().len(),
                    self.prof.ncols,
                    self.prof.counts.lock().unwrap().len(),
                    self.prof.ncols
                ),
            }));
        let doc = g.doc.clone();
        let out = self.prof.merged();
        let sampled = self.prof.rows < self.prof.total;
        let cards = v_flex().id("profile-cards").flex_1().min_h_0().overflow_y_scroll().px(px(16.)).children(out.iter().map(|p| profile_card(&doc, p, sampled, t, cx)));
        v_flex().size_full().child(head).child(controls).child(cards)
    }

    /// A Profile top value, clicked: a filter for the rows with that value. Not twice.
    pub fn filter_value(&mut self, col: usize, value: String, window: &mut Window, cx: &mut Context<Self>) {
        let ty = self.grid.as_ref().and_then(|g| g.doc.dtypes.get(col).copied()).unwrap_or(Dtype::String);
        let f = value_filter(col, ty, value);
        if !self.filters.contains(&f) {
            self.add_filter(Some(f), window, cx);
        }
    }

    /// Filter adds a row: `f`, or the first column and its first operator. The search runs again.
    pub fn add_filter(&mut self, f: Option<Filter>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(g) = &self.grid else { return };
        let f = f.unwrap_or_else(|| Filter { col: 0, op: ops(family(g.doc.dtypes.first().copied().unwrap_or(Dtype::String)))[0].0, a: String::new(), b: String::new(), or: false });
        let fam = family(g.doc.dtypes.get(f.col).copied().unwrap_or(Dtype::String));
        let a = cx.new(|cx| InputState::new(window, cx).placeholder(hint(fam)));
        let b = cx.new(|cx| InputState::new(window, cx).placeholder(hint(fam)));
        let sub = |st: &Entity<InputState>, second: bool, window: &mut Window, cx: &mut Context<Self>| {
            cx.subscribe_in(st, window, move |r: &mut Reader, st, e: &InputEvent, _, cx| {
                if !matches!(e, InputEvent::Change) {
                    return;
                }
                let v = st.read(cx).value().to_string();
                let at = r.filter_rows.iter().position(|f| if second { &f.b == st } else { &f.a == st });
                if let Some(f) = at.and_then(|i| r.filters.get_mut(i)) {
                    if second {
                        f.b = v
                    } else {
                        f.a = v
                    }
                    r.dirty = true;
                    r.rescan(cx);
                }
            })
        };
        let subs = [sub(&a, false, window, cx), sub(&b, true, window, cx)];
        // Both boxes show the filter's values: a bar's filter has a second one.
        a.update(cx, |s, cx| s.set_value(f.a.clone(), window, cx));
        b.update(cx, |s, cx| s.set_value(f.b.clone(), window, cx));
        self.filters.push(f);
        self.filter_rows.push(FilterRow { a, b, _subs: subs });
        self.dirty = true;
        self.rescan(cx);
        cx.notify();
    }

    /// The filter rows under the toolbar: column, operator, one or two values, remove.
    pub fn filter_bar(&mut self, t: Tokens, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let g = self.grid.as_ref()?;
        if self.filters.is_empty() {
            return None;
        }
        let view = cx.entity();
        let names = g.doc.names.clone();
        let dtypes = g.doc.dtypes.clone();
        let mut bar = v_flex().px(px(12.)).py(px(8.)).gap(px(6.)).flex_none().bg(t.bar).border_b_1().border_color(t.line);
        let rows = self.filters.len();
        for (i, (f, row)) in self.filters.iter().zip(&self.filter_rows).enumerate() {
            let fam = family(dtypes.get(f.col).copied().unwrap_or(Dtype::String));
            let list = ops(fam);
            let op_label = list.iter().find(|o| o.0 == f.op).map_or("", |o| o.1).to_string();
            let cols: Vec<(String, usize)> = names.iter().cloned().zip(0..).collect();
            let col = div().w(px(180.)).child(choose(("f-col", i), (false, names.get(f.col).cloned().unwrap_or_default()), cols, f.col, view.clone(), t, move |r: &mut Reader, c, _, cx| {
                if let Some(f) = r.filters.get_mut(i) {
                    let fam = r.grid.as_ref().map_or(Family::String, |g| family(g.doc.dtypes.get(c).copied().unwrap_or(Dtype::String)));
                    f.col = c;
                    if !ops(fam).iter().any(|o| o.0 == f.op) {
                        f.op = ops(fam)[0].0;
                    }
                    r.dirty = true;
                    r.rescan(cx);
                }
            }));
            let op_items: Vec<(String, crate::dsv::Op)> = list.iter().map(|o| (o.1.to_string(), o.0)).collect();
            let op = div().w(px(170.)).child(choose(("f-op", i), (false, op_label), op_items, f.op, view.clone(), t, move |r: &mut Reader, op, _, cx| {
                if let Some(f) = r.filters.get_mut(i) {
                    f.op = op;
                    r.dirty = true;
                    r.rescan(cx);
                }
            }));
            // The first row's funnel, or the "and" / "or" that joins this row to the ones before it.
            // The first row's funnel, alone when it is the only row; with more rows, in a slot as
            // wide as the "and" / "or" word below it, so the columns line up.
            let join = if i == 0 && rows == 1 {
                icon(IconName::Funnel, 14., t.muted).into_any_element()
            } else if i == 0 {
                div().w(px(34.)).flex_none().flex().justify_center().child(icon(IconName::Funnel, 14., t.muted)).into_any_element()
            } else {
                outline(("f-join", i), t)
                    .w(px(34.))
                    .px_0()
                    .justify_center()
                    .when(f.or, |b| b.text_color(t.accent).border_color(t.accent))
                    .child(if f.or { "or" } else { "and" })
                    .tooltip("Click for and / or. And binds first: a and b or c is (a and b) or c")
                    .on_click(cx.listener(move |r, _: &ClickEvent, _, cx| {
                        if let Some(f) = r.filters.get_mut(i) {
                            f.or = !f.or;
                            r.dirty = true;
                            r.rescan(cx);
                        }
                        cx.notify();
                    }))
                    .into_any_element()
            };
            let mut line = h_flex().gap(px(8.)).child(join).child(col).child(op);
            if arity(f.op) >= 1 {
                line = line.child(field_box(t, focused(&row.a, window, cx)).w(px(180.)).child(bare_input(&row.a).flex_1()));
            }
            if arity(f.op) == 2 {
                line = line.child(div().text_size(px(12.)).text_color(t.muted).child("and")).child(field_box(t, focused(&row.b, window, cx)).w(px(180.)).child(bare_input(&row.b).flex_1()));
            }
            line = line.child(icon_btn(("f-remove", i), IconName::X, "Remove this filter", t).w(px(30.)).on_click(cx.listener(move |r, _: &ClickEvent, _, cx| {
                if i < r.filters.len() {
                    r.filters.remove(i);
                    r.filter_rows.remove(i);
                    r.dirty = true;
                    r.rescan(cx);
                }
                cx.notify();
            })));
            bar = bar.child(line);
        }
        Some(bar.into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::value_filter;
    use crate::dsv::{Dtype, Op};

    #[test]
    fn statistics_read_with_separators_and_spans_in_two_units() {
        use super::{span_text, stat};
        assert_eq!(
            [stat(1890822.27594), stat(-3275000.5), stat(21451253275000.0), stat(0.00004), stat(-0.00004), stat(2.5)],
            ["1,890,822.2759", "-3,275,000.5", "21,451,253,275,000", "0", "0", "2.5"]
        );
        assert_eq!(
            [span_text(0.0), span_text(1000.0), span_text(775_070_000.0), span_text(2_469_000.0), span_text(86_400_000.0), span_text(9128.0 * 86_400_000.0)],
            ["0 seconds", "1 second", "8 days 23 hours", "41 minutes 9 seconds", "1 day", "25.0 years (9,128 days)"]
        );
    }
    #[test]
    fn a_top_value_filters_to_itself() {
        let f = value_filter(2, Dtype::String, "live".into());
        assert_eq!((f.col, f.op, f.a.as_str()), (2, Op::Is, "live"));
        assert_eq!(value_filter(0, Dtype::String, String::new()).op, Op::Empty);
        assert_eq!(value_filter(0, Dtype::Bool, "TRUE".into()).op, Op::True);
        let f = value_filter(0, Dtype::Bool, "false".into());
        assert_eq!((f.op, f.a.as_str()), (Op::False, ""));
    }
}
