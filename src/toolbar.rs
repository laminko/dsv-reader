//! Toolbar B from the design canvas ("B · Compact single row + parsing popover"), to its pixels:
//! a 52 px bar, 12 px side padding, 8 px gaps; 30 px controls in 12.5 px IBM Plex Sans with 6 px
//! corners; 32 × 30 icon buttons; a 460 px parsing popover with 16 px padding and 10 px corners.
//! Decimal and Date order stay in the column menu (step 3). A narrow window moves the controls
//! from the right end into a ⋯ menu (`overflowed`).

use crate::Reader;
use crate::dsv::{DELIMITERS, ENCODINGS, Opts};
use crate::file::{Theme, delim_name, enc_name, font_name, quote_name};
use crate::grid::{Tokens, count};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::radio::{Radio, RadioGroup};
use gpui_kit::component::{ActiveTheme as _, Disableable, Icon, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;

/// Toolbar controls that move into the ⋯ menu as the window narrows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Tool {
    Filter,
    Columns,
    Viewer,
    Copy,
    Profile,
    Font,
    Save,
    Export,
}

/// The toolbar order of the controls that can move.
const ORDER: [Tool; 8] = [Tool::Filter, Tool::Columns, Tool::Viewer, Tool::Copy, Tool::Profile, Tool::Font, Tool::Save, Tool::Export];
/// The order they move in: the icon buttons from the right end, then Filter, Save, Export.
const OVERFLOW: [Tool; 8] = [Tool::Font, Tool::Profile, Tool::Copy, Tool::Viewer, Tool::Columns, Tool::Filter, Tool::Save, Tool::Export];
const ICONS: [Tool; 5] = [Tool::Columns, Tool::Viewer, Tool::Copy, Tool::Profile, Tool::Font];

/// Control height, text size, and corner radius of the design.
const H: f32 = 30.0;
const TEXT: f32 = 12.5;
const R: f32 = 6.0;

pub(crate) fn icon(name: IconName, size: f32, color: Hsla) -> Icon {
    Icon::new(name).size(px(size)).text_color(color)
}

pub(crate) fn chevron(size: f32, t: Tokens) -> Icon {
    icon(IconName::ChevronDown, size, t.muted)
}

/// The text box has the keyboard.
pub(crate) fn focused(e: &Entity<InputState>, window: &Window, cx: &App) -> bool {
    e.read(cx).focus_handle(cx).contains_focused(window, cx)
}

pub(crate) fn ring(t: Tokens) -> BoxShadow {
    BoxShadow { color: t.ring, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(3.), inset: false }
}

/// `.btn`: 30 px, 0 10 px padding, 6 px gap and corners, 500 12.5 px; no border until a variant adds one.
pub(crate) fn btn(id: impl Into<ElementId>, t: Tokens) -> Button {
    Button::new(id).ghost().h(px(H)).px(px(10.)).gap(px(6.)).rounded(px(R)).text_size(px(TEXT)).font_weight(FontWeight::MEDIUM).text_color(t.ink)
}

/// `.btn.outline`: a 1 px line and the field color.
pub(crate) fn outline(id: impl Into<ElementId>, t: Tokens) -> Button {
    btn(id, t).outline().border_1().border_color(t.line).bg(t.field)
}

/// `.btn.icon`: 32 × 30 with a 16 px icon.
pub(crate) fn icon_btn(id: impl Into<ElementId>, name: IconName, tip: &'static str, t: Tokens) -> Button {
    Button::new(id).ghost().w(px(32.)).h(px(H)).rounded(px(R)).child(icon(name, 16., t.muted)).tooltip(tip)
}

/// An icon button that stays pressed while its pane shows.
pub(crate) fn toggle(b: Button, on: bool, t: Tokens) -> Button {
    b.when(on, |b| b.bg(t.hover).border_1().border_color(t.line))
}

/// `.sep`: a 1 × 22 px line.
pub(crate) fn bar_sep(t: Tokens) -> Div {
    div().w(px(1.)).h(px(22.)).flex_none().bg(t.line)
}

/// `.kbd`: 500 10.5 px mono, 1 px 5 px padding, 4 px corners.
pub(crate) fn kbd(keys: &'static str, t: Tokens) -> Div {
    div()
        .flex_none()
        .px(px(5.))
        .py(px(1.))
        .rounded(px(4.))
        .border_1()
        .border_color(t.line)
        .bg(t.bg)
        .font_family("Menlo")
        .text_size(px(10.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.muted)
        .child(keys)
}

/// `.field`: 30 px, 0 6 0 10 px padding, 8 px gap, 6 px corners; the accent and a 3 px ring with focus.
pub(crate) fn field_box(t: Tokens, focused: bool) -> Div {
    h_flex().h(px(H)).pl(px(10.)).pr(px(6.)).gap(px(8.)).rounded(px(R)).border_1().border_color(if focused { t.accent } else { t.line }).bg(t.field).when(focused, |d| d.shadow(vec![ring(t)]))
}

/// A text box inside a `.field`: no border of its own. Its height and text size stay gpui-kit's
/// (28 px and 12.25 px at the 14 px base): a size set from outside put the text and the caret
/// out of step, and the first letter was cut.
pub(crate) fn bare_input(state: &Entity<InputState>) -> Input {
    Input::new(state).appearance(false).px_0()
}

/// `.cap`: 600 10.5 px, upper case, muted.
pub(crate) fn caption(text: &str, t: Tokens) -> Div {
    div().text_size(px(10.5)).font_weight(FontWeight::SEMIBOLD).text_color(t.muted).child(text.to_uppercase())
}

/// A labelled control in the popover: a 12 px muted label 5 px above it.
pub(crate) fn labelled(label: &str, t: Tokens, control: impl IntoElement) -> Div {
    v_flex().flex_1().min_w_0().gap(px(5.)).child(div().text_size(px(12.)).text_color(t.muted).child(label.to_string())).child(control)
}

/// `.dd`: a choice as a 30 px button, "Auto:" muted, the chevron at the right, and a menu that
/// checks the current value.
pub(crate) fn choose<T: Clone + PartialEq + 'static>(
    id: impl Into<ElementId>,
    (auto, label): (bool, String),
    items: Vec<(String, T)>,
    current: T,
    view: Entity<Reader>,
    t: Tokens,
    apply: impl Fn(&mut Reader, T, &mut Window, &mut Context<Reader>) + Clone + 'static,
) -> impl IntoElement {
    let text = h_flex().gap(px(4.)).min_w_0().overflow_hidden().when(auto, |d| d.child(div().font_weight(FontWeight::NORMAL).text_color(t.muted).child("Auto:"))).child(div().truncate().child(label));
    // The caret puts the text at the left edge and itself at the right one, as `.dd` does.
    let watch = view.clone();
    outline(id, t)
        .w_full()
        .pl(px(10.))
        .pr(px(8.))
        .child(text)
        .dropdown_caret(true)
        .dropdown_menu(move |mut menu, _, _| {
            for (name, v) in items.clone() {
                let (view, apply, checked) = (view.clone(), apply.clone(), v == current);
                menu = menu.item(PopupMenuItem::new(name).checked(checked).on_click(move |_, window, cx| {
                    let v = v.clone();
                    view.update(cx, |r, cx| apply(r, v, window, cx));
                }));
            }
            menu.scrollable(true).max_h(px(420.))
        })
        // While a menu is open, a click on it is not a click outside its popover.
        .on_open_change(move |open, _, cx| watch.update(cx, |r, _| r.menu_open = *open))
}

/// A popover panel: 16 px padding, 10 px corners, the design's two shadows.
pub(crate) fn panel(p: Popover, t: Tokens, width: f32) -> Popover {
    let shadow = |y: f32, blur: f32, a: f32| BoxShadow { color: hsla(0.61, 0.33, 0.09, a), offset: point(px(0.), px(y)), blur_radius: px(blur), spread_radius: px(0.), inset: false };
    p.w(px(width)).p(px(16.)).rounded(px(10.)).border_1().border_color(t.line).bg(t.field).shadow(vec![shadow(12., 32., 0.16), shadow(2., 6., 0.08)])
}

impl Reader {
    pub(crate) fn popover(&self, id: &'static str, cx: &mut Context<Self>) -> Popover {
        Popover::new(id).open(self.pop == Some(id)).on_open_change(cx.listener(move |r: &mut Reader, open: &bool, _, cx| {
            // A press outside just closed it: the click on its own button that made the press stays closed.
            if *open && r.pop_closed.is_some_and(|(p, at)| p == id && at.elapsed().as_millis() < 400) {
                return;
            }
            r.pop = open.then_some(id);
            cx.notify();
        }))
    }

    /// The content of a popover whose menus hang past its edge: a press outside it closes it,
    /// unless a menu is open (then the press is on the menu, or closes the menu first).
    pub(crate) fn closes_outside(&self, id: &'static str, content: Div, cx: &mut Context<Self>) -> Div {
        content.on_mouse_down_out(cx.listener(move |r: &mut Reader, _: &MouseDownEvent, _, cx| {
            if r.pop == Some(id) && !r.menu_open {
                (r.pop, r.pop_closed) = (None, Some((id, std::time::Instant::now())));
                cx.notify();
            }
        }))
    }

    pub fn toolbar(&mut self, t: Tokens, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let has = self.grid.is_some();
        let goto_focused = focused(&self.goto, window, cx);
        let (hidden, compact, search_min) = self.overflowed(window);
        let shows = |tool: Tool| !hidden.contains(&tool);
        let filter = shows(Tool::Filter).then(|| {
            btn("filter", t)
                .child(icon(IconName::Funnel, 14., t.muted))
                .child("Filter")
                .tooltip("Add a filter. Filters join with and / or")
                .disabled(!has)
                .when(!has, |b| b.opacity(0.45))
                .on_click(cx.listener(|r, _: &ClickEvent, window, cx| r.add_filter(None, window, cx)))
        });
        let columns = shows(Tool::Columns).then(|| self.columns_button(t, cx));
        let viewer = shows(Tool::Viewer).then(|| {
            toggle(icon_btn("viewer", IconName::PanelRight, "Record viewer", t), self.viewer, t).disabled(!has).on_click(cx.listener(|r, _: &ClickEvent, _, cx| r.toggle_pane(crate::Pane::Viewer, cx)))
        });
        let copy = shows(Tool::Copy).then(|| self.copy_button(t, cx));
        let profile = shows(Tool::Profile).then(|| {
            toggle(icon_btn("profile", IconName::ChartBar, "Profile: statistics for each column", t), self.profile, t)
                .disabled(!has)
                .on_click(cx.listener(|r, _: &ClickEvent, _, cx| r.toggle_pane(crate::Pane::Profile, cx)))
        });
        let font = shows(Tool::Font).then(|| self.font_button(t, cx));
        let save =
            shows(Tool::Save).then(|| outline("save", t).child("Save…").disabled(!has).when(!has, |b| b.opacity(0.45)).on_click(cx.listener(|r, _: &ClickEvent, window, cx| r.ask_save(window, cx))));
        let export = shows(Tool::Export).then(|| {
            btn("export", t)
                .primary()
                .text_color(rgb(0xFFFFFF))
                .child(icon(IconName::Download, 14., rgb(0xFFFFFF).into()))
                .child("Export…")
                .tooltip("The rows you see, as a CSV file (⌘S)")
                .disabled(!has)
                .when(!has, |b| b.opacity(0.45))
                .on_click(cx.listener(|r, _: &ClickEvent, window, cx| r.start_export(window, cx)))
        });
        let more = (!hidden.is_empty()).then(|| self.more_button(hidden.clone(), t, cx));
        h_flex()
            .h(px(52.))
            .flex_none()
            .gap(px(8.))
            .px(px(12.))
            .bg(t.bar)
            .border_b_1()
            .border_color(t.line)
            .child(
                outline("open", t)
                    .child(icon(IconName::FolderOpen, 15., t.muted))
                    .child("Open…")
                    .tooltip("Open a file (⌘O)")
                    .on_click(cx.listener(|r, _: &ClickEvent, window, cx| r.pick_file(window, cx))),
            )
            .child(self.parse_pill(t, compact, window, cx))
            .child(bar_sep(t))
            .child(self.search_field(t, search_min, window, cx))
            .children(filter)
            .child(field_box(t, goto_focused).w(px(128.)).flex_none().child(bare_input(&self.goto).flex_1()).child(kbd("⌘G", t)))
            .when(ICONS.iter().any(|&x| shows(x)), |d| d.child(bar_sep(t)))
            .children(columns)
            .children(viewer)
            .children(copy)
            .children(profile)
            .children(font)
            .when(shows(Tool::Save) || shows(Tool::Export), |d| d.child(bar_sep(t)))
            .children(save)
            .children(export)
            .children(more)
    }

    /// What a narrow window gives up, step by step until the toolbar fits: the controls in
    /// OVERFLOW order go into the ⋯ menu, then the parsing pill shows only its first part, then
    /// the search box's least width drops from 300 to 220 px and its ⌘F hint goes. The box's icon,
    /// hint, and scope take 186 px: 300 leaves its text about 110. Widths are the controls' own
    /// paddings and icons, and their labels as the text system shapes them.
    fn overflowed(&self, window: &Window) -> (Vec<Tool>, bool, f32) {
        let ts = window.text_system();
        let text = |s: &str, weight: FontWeight| {
            let run = TextRun { len: s.len(), font: Font { weight, ..font("IBM Plex Sans") }, color: black(), background_color: None, underline: None, strikethrough: None };
            f32::from(ts.shape_line(SharedString::from(s.to_owned()), px(TEXT), &[run], None).width())
        };
        let label = |s: &str| text(s, FontWeight::MEDIUM);
        let parts = self.pill_parts();
        let dot = text("·", FontWeight::NORMAL);
        // Padding 10 + 8, a 1 px border each side, the 14 px icon and 12 px chevron; 8 px gaps.
        let pill = |n: usize| {
            36.0 + parts.iter().take(n).enumerate().map(|(i, p)| text(p, if i == 0 { FontWeight::MEDIUM } else { FontWeight::NORMAL })).sum::<f32>()
                + (n.saturating_sub(1)) as f32 * dot
                + 16.0 * n as f32
        };
        let n = OVERFLOW.len();
        // A button: 10 px padding and a 1 px border each side; an icon and a 6 px gap.
        let width = |tool: Tool| match tool {
            Tool::Filter => 22.0 + 14.0 + 6.0 + label("Filter"),
            Tool::Save => 22.0 + label("Save…"),
            Tool::Export => 22.0 + 14.0 + 6.0 + label("Export…"),
            _ => 32.0,
        };
        let fits = |k: usize| {
            let gone = &OVERFLOW[..k.min(n)];
            let shows = |tool: &Tool| !gone.contains(tool);
            let pill = pill(if k > n { 1 } else { parts.len() });
            // Open…, the pill, a line, the search box, Go to row: always there.
            let mut w = vec![22.0 + 15.0 + 6.0 + label("Open…"), pill, 1.0, if k > n + 1 { 220.0 } else { 300.0 }, 128.0];
            w.extend(ORDER.iter().filter(|x| shows(x)).map(|&x| width(x)));
            if ICONS.iter().any(shows) {
                w.push(1.0);
            }
            if shows(&Tool::Save) || shows(&Tool::Export) {
                w.push(1.0);
            }
            if k > 0 {
                w.push(32.0);
            }
            24.0 + w.iter().sum::<f32>() + 8.0 * (w.len() - 1) as f32 <= f32::from(window.viewport_size().width)
        };
        let k = (0..n + 2).find(|&k| fits(k)).unwrap_or(n + 2);
        (OVERFLOW[..k.min(n)].to_vec(), k > n, if k > n + 1 { 220.0 } else { 300.0 })
    }

    /// The ⋯ button: the controls that do not fit, as menu items in toolbar order. The menu reads
    /// the state when it opens: a 1,000-column file builds its column list only then.
    fn more_button(&mut self, mut tools: Vec<Tool>, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        tools.sort_by_key(|x| ORDER.iter().position(|o| o == x));
        let view = cx.entity();
        icon_btn("more", IconName::Ellipsis, "More", t).dropdown_menu(move |mut menu, window, cx| {
            let r = view.read(cx);
            let (has, viewer, profile, size, theme, font, zebra) = (r.grid.is_some(), r.viewer, r.profile, r.settings.size, r.settings.theme, r.settings.font.clone(), r.settings.zebra);
            let cols: Vec<(usize, String, bool)> = match (&r.grid, tools.contains(&Tool::Columns)) {
                (Some(g), true) => g.order.iter().map(|&c| (c, g.doc.names[c].clone(), !g.hidden[c])).collect(),
                _ => Vec::new(),
            };
            let fonts: Vec<(String, Option<PathBuf>)> = std::iter::once(("Built-in".to_string(), None)).chain(r.fonts.iter().map(|p| (font_name(p), Some(p.clone())))).collect();
            // A menu item's action on the Reader.
            let act = |f: fn(&mut Reader, &mut Window, &mut Context<Reader>)| {
                let v = view.clone();
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| v.update(cx, |r, cx| f(r, window, cx))
            };
            let mut group = None;
            for &tool in &tools {
                let g = match tool {
                    Tool::Filter => 0,
                    Tool::Save | Tool::Export => 2,
                    _ => 1,
                };
                if group.is_some_and(|x| x != g) {
                    menu = menu.separator();
                }
                group = Some(g);
                menu = match tool {
                    Tool::Filter => menu.item(PopupMenuItem::new("Add a filter").disabled(!has).on_click(act(|r, w, cx| r.add_filter(None, w, cx)))),
                    Tool::Columns => {
                        let (v, cols) = (view.clone(), cols.clone());
                        menu.submenu("Columns", window, cx, move |mut m, _, _| {
                            let (a, b) = (v.clone(), v.clone());
                            m = m
                                .item(PopupMenuItem::new("Show all").on_click(move |_, _, cx| {
                                    a.update(cx, |r, cx| {
                                        if let Some(g) = &mut r.grid {
                                            (g.hidden, g.sel) = (vec![false; g.hidden.len()], None);
                                        }
                                        cx.notify();
                                    })
                                }))
                                .item(PopupMenuItem::new("Reset order").on_click(move |_, _, cx| {
                                    b.update(cx, |r, cx| {
                                        if let Some(g) = &mut r.grid {
                                            g.reset_order();
                                            g.sel = None;
                                        }
                                        cx.notify();
                                    })
                                }))
                                .separator();
                            for (c, name, on) in cols.clone() {
                                let v = v.clone();
                                m = m.item(PopupMenuItem::new(name).checked(on).on_click(move |_, _, cx| {
                                    v.update(cx, |r, cx| {
                                        if let Some(g) = &mut r.grid {
                                            (g.hidden[c], g.sel) = (on, None);
                                        }
                                        cx.notify();
                                    })
                                }));
                            }
                            m.scrollable(true).max_h(px(420.))
                        })
                    }
                    Tool::Viewer => menu.item(PopupMenuItem::new("Record viewer").checked(viewer).disabled(!has).on_click(act(|r, _, cx| r.toggle_pane(crate::Pane::Viewer, cx)))),
                    Tool::Copy => menu
                        .item(PopupMenuItem::new("Copy").disabled(!has).on_click(act(|r, _, cx| r.copy(false, cx))))
                        .item(PopupMenuItem::new("Copy with column names").disabled(!has).on_click(act(|r, _, cx| r.copy(true, cx)))),
                    Tool::Profile => menu.item(PopupMenuItem::new("Profile").checked(profile).disabled(!has).on_click(act(|r, _, cx| r.toggle_pane(crate::Pane::Profile, cx)))),
                    Tool::Font => {
                        let (v, fonts, font) = (view.clone(), fonts.clone(), font.clone());
                        menu.submenu("Font and size", window, cx, move |mut m, window, cx| {
                            let (a, b) = (v.clone(), v.clone());
                            m = m
                                .label(format!("Size {size} pt"))
                                .item(PopupMenuItem::new("Larger").on_click(move |_, _, cx| a.update(cx, |r, cx| r.set_size(size + 1.0, cx))))
                                .item(PopupMenuItem::new("Smaller").on_click(move |_, _, cx| b.update(cx, |r, cx| r.set_size(size - 1.0, cx))))
                                .separator()
                                .label("Theme");
                            for th in Theme::ALL {
                                let v = v.clone();
                                m = m.item(PopupMenuItem::new(th.name()).checked(th == theme).on_click(move |_, window, cx| v.update(cx, |r, cx| r.set_theme(th, window, cx))));
                            }
                            let z = v.clone();
                            m = m.separator().item(PopupMenuItem::new("Zebra rows").checked(zebra).on_click(move |_, _, cx| z.update(cx, |r, cx| r.set_zebra(!zebra, cx))));
                            let (v, fonts, font) = (v.clone(), fonts.clone(), font.clone());
                            m.separator().submenu("Grid font", window, cx, move |mut f, _, _| {
                                for (name, p) in fonts.clone() {
                                    let (v, on) = (v.clone(), p == font);
                                    f = f.item(PopupMenuItem::new(name).checked(on).on_click(move |_, _, cx| v.update(cx, |r, cx| r.set_font(p.clone(), cx))));
                                }
                                f.scrollable(true).max_h(px(420.))
                            })
                        })
                    }
                    Tool::Save => menu.item(PopupMenuItem::new("Save…").disabled(!has).on_click(act(|r, w, cx| r.ask_save(w, cx)))),
                    Tool::Export => menu.item(PopupMenuItem::new("Export…").disabled(!has).on_click(act(|r, w, cx| r.start_export(w, cx)))),
                };
            }
            menu
        })
    }

    /// The parsing pill's parts: delimiter, header, qualifier, encoding.
    fn pill_parts(&self) -> Vec<String> {
        match &self.grid {
            None => vec!["Parsing".into()],
            Some(g) => {
                let d = &g.doc;
                let quote = match d.quote.as_str() {
                    "\"" => "\" quoted",
                    "'" => "' quoted",
                    _ => "not quoted",
                };
                vec![delim_name(&d.delim), if d.header { "Header" } else { "No header" }.into(), quote.into(), enc_name(d.encoding).into()]
            }
        }
    }

    /// `.pill`: "**Comma** · Header · " quoted · UTF-8", and the parsing popover. Compact: "**Comma**".
    fn parse_pill(&mut self, t: Tokens, compact: bool, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mut parts = self.pill_parts();
        if compact {
            parts.truncate(1);
        }
        let open = self.pop == Some("parse");
        let mut pill = outline("parse-pill", t).pl(px(10.)).pr(px(8.)).gap(px(8.)).font_weight(FontWeight::NORMAL).when(open, |b| b.border_color(t.accent).shadow(vec![ring(t)])).child(icon(
            IconName::SlidersHorizontal,
            14.,
            t.muted,
        ));
        for (i, p) in parts.into_iter().enumerate() {
            if i > 0 {
                pill = pill.child(div().text_color(t.line_strong).child("·"));
            }
            pill = pill.child(div().when(i == 0, |d| d.font_weight(FontWeight::MEDIUM)).child(p));
        }
        let pill = pill.child(chevron(12., t));

        let view = cx.entity();
        let o = self.opts.clone();
        let found = self.grid.as_ref().map(|g| (g.doc.delim.clone(), g.doc.header, g.doc.encoding));
        let delims: Vec<(String, Option<String>)> = std::iter::once(("Auto".to_string(), None)).chain(DELIMITERS.iter().map(|d| (delim_name(d), Some(d.to_string())))).collect();
        let delim_now = match (&o.delim, &found) {
            (None, Some(f)) => (true, delim_name(&f.0)),
            (None, None) => (false, "Auto".into()),
            (Some(d), _) => (false, delim_name(d)),
        };
        let headers = vec![("Auto".to_string(), None), ("On".to_string(), Some(true)), ("Off".to_string(), Some(false))];
        let header_now = match (o.header, &found) {
            (None, Some(f)) => (true, if f.1 { "On".into() } else { "Off".into() }),
            (None, None) => (false, "Auto".into()),
            (Some(h), _) => (false, if h { "On".into() } else { "Off".into() }),
        };
        let quotes: Vec<(String, String)> = ["\"", "'", ""].iter().map(|q| (quote_name(q).to_string(), q.to_string())).collect();
        let encs: Vec<(String, Option<&'static str>)> = std::iter::once(("Auto".to_string(), None)).chain(ENCODINGS.iter().map(|(l, n)| (n.to_string(), Some(*l)))).collect();
        let enc_now = match (o.encoding, &found) {
            (None, Some(f)) => (true, enc_name(f.2).to_string()),
            (None, None) => (false, "Auto".into()),
            (Some(e), _) => (false, enc_name(e).to_string()),
        };
        let other_focused = focused(&self.other, window, cx);
        let content =
            v_flex()
                .gap(px(14.))
                .child(h_flex().justify_between().child(caption("Parsing", t)).child(
                    Button::new("reset").ghost().h(px(24.)).px(px(8.)).rounded(px(R)).text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(t.accent).child("Reset to auto").on_click(
                        cx.listener(|r, _: &ClickEvent, window, cx| {
                            r.other.update(cx, |s, cx| s.set_value("", window, cx));
                            r.set_opts(Opts::default(), window, cx);
                        }),
                    ),
                ))
                .child(
                    v_flex()
                        .gap(px(12.))
                        .child(
                            h_flex()
                                .gap(px(14.))
                                .items_start()
                                .child(labelled(
                                    "Delimiter",
                                    t,
                                    h_flex()
                                        .gap(px(6.))
                                        .child(div().flex_1().min_w_0().child(choose("delim", delim_now, delims, o.delim.clone(), view.clone(), t, |r: &mut Reader, d, w, cx| {
                                            r.set_opts(Opts { delim: d, ..r.opts.clone() }, w, cx)
                                        })))
                                        .child(field_box(t, other_focused).w(px(58.)).flex_none().pr(px(10.)).child(bare_input(&self.other).flex_1())),
                                ))
                                .child(labelled(
                                    "Header row",
                                    t,
                                    choose("header", header_now, headers, o.header, view.clone(), t, |r: &mut Reader, h, w, cx| r.set_opts(Opts { header: h, ..r.opts.clone() }, w, cx)),
                                )),
                        )
                        .child(
                            h_flex()
                                .gap(px(14.))
                                .items_start()
                                .child(labelled(
                                    "Text qualifier",
                                    t,
                                    choose("quote", (false, quote_name(&o.quote).to_string()), quotes, o.quote.clone(), view.clone(), t, |r: &mut Reader, q, w, cx| {
                                        r.set_opts(Opts { quote: q, ..r.opts.clone() }, w, cx)
                                    }),
                                ))
                                .child(labelled(
                                    "Encoding",
                                    t,
                                    choose("encoding", enc_now, encs, o.encoding, view, t, |r: &mut Reader, e, w, cx| r.set_opts(Opts { encoding: e, ..r.opts.clone() }, w, cx)),
                                )),
                        ),
                );
        // Its menus hang past its edge: gpui-kit's own outside click would close it on a menu click.
        let content = self.closes_outside("parse", content, cx);
        panel(self.popover("parse", cx), t, 460.).overlay_closable(false).offset(px(9.)).trigger(pill).child(content)
    }

    /// The search `.field`: icon, text, ⌘F, and the column scope at its right edge.
    fn search_field(&mut self, t: Tokens, min: f32, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        // The list is built only while it shows: a 1,000-column file would build 1,000 checkboxes a frame.
        let names = if self.pop == Some("scope") { self.grid.as_ref().map_or(Vec::new(), |g| g.doc.names.clone()) } else { Vec::new() };
        let label = if self.scope.is_empty() { "All columns".to_string() } else { count(self.scope.len(), "column") };
        let list = div().id("scope-list").max_h(px(400.)).overflow_y_scroll().children(names.into_iter().enumerate().map(|(c, name)| {
            Checkbox::new(("scope", c)).label(name).checked(self.scope.contains(&c)).py(px(3.)).on_change(cx.listener(move |r, on: &bool, _, cx| {
                if *on {
                    r.scope.push(c);
                } else {
                    r.scope.retain(|&x| x != c);
                }
                r.dirty = true;
                r.rescan(cx);
            }))
        }));
        let scope_btn = Button::new("scope-btn")
            .ghost()
            .h(px(22.))
            .pl(px(10.))
            .pr(px(4.))
            .gap(px(4.))
            .rounded_none()
            .border_l_1()
            .border_color(t.line)
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.muted)
            .child(label)
            .child(chevron(11., t));
        let scope = panel(self.popover("scope", cx), t, 240.).anchor(Anchor::TopRight).offset(px(10.)).trigger(scope_btn).child(
            v_flex()
                .gap(px(6.))
                .child(caption("Search in", t))
                .child(btn("scope-all", t).h(px(26.)).px(px(8.)).justify_start().child("All columns").on_click(cx.listener(|r, _: &ClickEvent, _, cx| {
                    r.scope.clear();
                    r.dirty = true;
                    r.rescan(cx);
                })))
                .child(list),
        );
        let focused = focused(&self.search, window, cx);
        field_box(t, focused).flex_1().min_w(px(min)).child(icon(IconName::Search, 14., t.muted)).child(bare_input(&self.search).flex_1()).when(min >= 300.0, |d| d.child(kbd("⌘F", t))).child(scope)
    }

    /// Show or hide each column.
    fn columns_button(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        // In the column order. Built only while it shows.
        let cols: Vec<(usize, String, bool)> = match (&self.grid, self.pop == Some("columns")) {
            (Some(g), true) => g.order.iter().map(|&c| (c, g.doc.names[c].clone(), !g.hidden[c])).collect(),
            _ => Vec::new(),
        };
        let list = div().id("columns-list").max_h(px(400.)).overflow_y_scroll().children(cols.into_iter().map(|(c, name, on)| {
            Checkbox::new(("column", c)).label(name).checked(on).py(px(3.)).on_change(cx.listener(move |r, on: &bool, _, cx| {
                if let Some(g) = &mut r.grid {
                    (g.hidden[c], g.sel) = (!*on, None);
                }
                cx.notify();
            }))
        }));
        let open = self.pop == Some("columns");
        panel(self.popover("columns", cx), t, 240.).anchor(Anchor::TopRight).offset(px(11.)).trigger(icon_btn("columns-btn", IconName::Columns3, "Columns", t).when(open, |b| b.bg(t.hover))).child(
            v_flex()
                .gap(px(6.))
                .child(caption("Columns", t))
                .child(
                    h_flex()
                        .gap(px(4.))
                        .child(btn("columns-all", t).h(px(26.)).px(px(8.)).child("Show all").on_click(cx.listener(|r, _: &ClickEvent, _, cx| {
                            if let Some(g) = &mut r.grid {
                                (g.hidden, g.sel) = (vec![false; g.hidden.len()], None);
                            }
                            cx.notify();
                        })))
                        .child(btn("columns-reset", t).h(px(26.)).px(px(8.)).child("Reset order").on_click(cx.listener(|r, _: &ClickEvent, _, cx| {
                            if let Some(g) = &mut r.grid {
                                g.reset_order();
                                g.sel = None;
                            }
                            cx.notify();
                        }))),
                )
                .child(list),
        )
    }

    /// Copy: the selection as tab-separated text, with or without the column names.
    fn copy_button(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let view = cx.entity();
        icon_btn("copy", IconName::Copy, "Copy the selection (⌘C)", t).disabled(self.grid.is_none()).dropdown_menu(move |menu, _, _| {
            let (a, b) = (view.clone(), view.clone());
            menu.item(PopupMenuItem::new("Copy").on_click(move |_, _, cx| a.update(cx, |r, cx| r.copy(false, cx))))
                .item(PopupMenuItem::new("Copy with column names").on_click(move |_, _, cx| b.update(cx, |r, cx| r.copy(true, cx))))
        })
    }

    /// The grid font, its size, and the theme.
    fn font_button(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let view = cx.entity();
        let current = self.settings.font.as_deref().map_or("Built-in".to_string(), font_name);
        let fonts: Vec<(String, Option<PathBuf>)> = std::iter::once(("Built-in".to_string(), None)).chain(self.fonts.iter().map(|p| (font_name(p), Some(p.clone())))).collect();
        let size = self.settings.size;
        let themes: Vec<(String, Theme)> = Theme::ALL.iter().map(|&th| (th.name().to_string(), th)).collect();
        let open = self.pop == Some("font");
        let step = |id: &'static str, label: &'static str| outline(id, t).w(px(30.)).px_0().child(label);
        // Its menus hang past its edge: a click on them must not count as a click outside.
        panel(self.popover("font", cx), t, 300.)
            .overlay_closable(false)
            .anchor(Anchor::TopRight)
            .offset(px(11.))
            .trigger(icon_btn("font-btn", IconName::Type, "Font and size", t).when(open, |b| b.bg(t.hover)))
            .child(
                self.closes_outside(
                    "font",
                    v_flex()
                        .gap(px(14.))
                        .child(caption("View", t))
                        .child(labelled("Grid font", t, choose("font-menu", (false, current), fonts, self.settings.font.clone(), view.clone(), t, |r: &mut Reader, f, _, cx| r.set_font(f, cx))))
                        .child(labelled(
                            "Size",
                            t,
                            h_flex()
                                .gap(px(8.))
                                .child(step("smaller", "−").on_click(cx.listener(move |r, _: &ClickEvent, _, cx| r.set_size(size - 1.0, cx))))
                                .child(div().min_w(px(48.)).text_center().text_size(px(TEXT)).font_weight(FontWeight::MEDIUM).child(format!("{size} pt")))
                                .child(step("larger", "+").on_click(cx.listener(move |r, _: &ClickEvent, _, cx| r.set_size(size + 1.0, cx)))),
                        ))
                        .child(labelled(
                            "Theme",
                            t,
                            choose("theme", (false, self.settings.theme.name().to_string()), themes, self.settings.theme, view, t, |r: &mut Reader, th, w, cx| r.set_theme(th, w, cx)),
                        ))
                        .child(labelled("Rows", t, Checkbox::new("zebra").label("Zebra rows").checked(self.settings.zebra).on_change(cx.listener(|r, on: &bool, _, cx| r.set_zebra(*on, cx))))),
                    cx,
                ),
            )
    }

    /// The Save question: overwrite the open file, or write a new file in its folder.
    pub fn ask_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(g) = &self.grid else { return };
        let own = g.doc.path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
        let (choice, name, view) = (self.save_new.clone(), self.save_name.clone(), cx.entity());
        window.open_dialog(cx, move |d, window, cx| {
            let t = if cx.theme().is_dark() { Tokens::dark() } else { Tokens::light() };
            let (c2, v2) = (choice.clone(), view.clone());
            d.title("Save")
                .w(px(500.))
                .child(
                    v_flex()
                        .gap(px(14.))
                        .child(div().text_size(px(TEXT)).child(
                            "Save writes every row of the file, in file order, with the columns you see. It keeps the delimiter, the record delimiter, and the text qualifier. It writes UTF-8.",
                        ))
                        .child(
                            RadioGroup::vertical("save-mode")
                                .selected_index(Some(*choice.read(cx)))
                                .child(Radio::new("overwrite").label(format!("Overwrite {own}")))
                                .child(Radio::new("new").label("Write a new file in the same folder:"))
                                .on_change(move |ix, _, cx| c2.update(cx, |v, cx| (*v, ()) = (*ix, cx.notify()))),
                        )
                        .child(field_box(t, focused(&name, window, cx)).child(bare_input(&name).flex_1())),
                )
                .footer(DialogFooter::new().child(DialogClose::new().trigger(|b| b.label("Cancel"))).child(DialogAction::new().child(Button::new("save-go").primary().label("Save"))))
                .on_ok(move |_, _, cx| {
                    v2.update(cx, |r, cx| r.start_save(cx));
                    true
                })
        });
    }

    /// "Open in": the oldest waiting file in this window or in a new instance. Cancel skips it.
    pub fn ask_open_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.ask_open.front().cloned() else { return };
        if let Ok(how) = std::env::var("DSV_OPEN_IN") {
            return self.answer_open(&path, Some(how == "new"), window, cx);
        }
        let name = path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
        let view = cx.entity();
        window.open_dialog(cx, move |d, _, _| {
            let (a, b, c, p) = (view.clone(), view.clone(), view.clone(), path.clone());
            let (pa, pb) = (path.clone(), path.clone());
            d.title("Open in")
                .w(px(440.))
                .child(div().text_size(px(TEXT)).child(format!("Open {name} in:")))
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().trigger(|b| b.label("Cancel")))
                        .child(Button::new("open-new").outline().label("New window").on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            b.update(cx, |r, cx| r.answer_open(&pb, Some(true), window, cx));
                        }))
                        .child(DialogAction::new().child(Button::new("open-current").primary().label("Current window"))),
                )
                .on_ok(move |_, window, cx| {
                    a.update(cx, |r, cx| r.answer_open(&pa, Some(false), window, cx));
                    true
                })
                // Cancel, Esc, or ×: skip the file. After an answer it is gone, and this does nothing.
                .on_close(move |_, window, cx| c.update(cx, |r, cx| r.answer_open(&p, None, window, cx)))
        });
    }

    /// The question before a quit that loses the column order.
    pub fn ask_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.asking_quit, true) {
            return;
        }
        let view = cx.entity();
        window.open_dialog(cx, move |d, _, _| {
            let (a, b) = (view.clone(), view.clone());
            d.title("Quit")
                .w(px(440.))
                .child(div().text_size(px(TEXT)).child("The column order and the hidden columns are not saved. Quit?"))
                .footer(DialogFooter::new().child(DialogClose::new().trigger(|b| b.label("Cancel"))).child(DialogAction::new().child(Button::new("quit-go").primary().label("Quit"))))
                .on_ok(move |_, _, cx| {
                    a.update(cx, |r, _| r.quit_ok = true);
                    crate::mac::MODIFIED.store(false, std::sync::atomic::Ordering::Relaxed);
                    cx.quit();
                    true
                })
                .on_close(move |_, _, cx| b.update(cx, |r, _| r.asking_quit = false))
        });
    }
}
