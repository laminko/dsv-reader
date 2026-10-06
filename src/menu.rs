//! The column menu (a right-click on a column name): the type when detection is wrong, and how
//! the cells show. The same choices as the egui app's menu, in the toolbar's popover style.

use crate::Reader;
use crate::dsv::{DatePat, Dtype, Family, Kind, Order, Reading, Show, TimePat, Unit, family};
use crate::grid::{Tokens, fits};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// What detection found, in words.
fn type_name(t: Dtype, r: Reading) -> String {
    match t {
        Dtype::Int | Dtype::Float if r.comma => "number, decimal comma".into(),
        Dtype::Int | Dtype::Float => "number".into(),
        Dtype::Date => format!("date, {}", ["DMY", "MDY", "YMD"][r.order as usize]),
        Dtype::Bool => "yes-no".into(),
        Dtype::String => "text".into(),
    }
}

fn caption(text: &str, t: Tokens) -> Div {
    div().text_size(px(10.5)).font_weight(FontWeight::SEMIBOLD).text_color(t.muted).child(text.to_uppercase())
}

/// A radio row: a 14 px ring, filled with the accent when chosen, and its label.
fn radio(id: impl Into<ElementId>, on: bool, label: impl Into<SharedString>, t: Tokens) -> Stateful<Div> {
    let dot = div()
        .size(px(14.))
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(if on { t.accent } else { t.line_strong })
        .flex()
        .items_center()
        .justify_center()
        .when(on, |d| d.child(div().size(px(6.)).rounded_full().bg(t.accent)));
    h_flex().id(id).h(px(28.)).px(px(8.)).gap(px(8.)).rounded(px(6.)).text_size(px(12.5)).text_color(t.ink).hover(|s| s.bg(t.hover)).child(dot).child(label.into())
}

/// A small choice chip, 24 px, the accent border when chosen.
fn chip(id: impl Into<ElementId>, on: bool, label: impl Into<SharedString>, t: Tokens) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(24.))
        .px(px(8.))
        .flex()
        .items_center()
        .rounded(px(5.))
        .border_1()
        .border_color(if on { t.accent } else { t.line })
        .bg(if on { t.sel } else { t.field })
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(if on { t.accent } else { t.ink })
        .hover(|s| s.border_color(t.accent))
        .child(label.into())
}

/// A row of chips under a radio, indented to its label.
fn chips(label: &str, t: Tokens) -> Div {
    h_flex().pl(px(30.)).gap(px(6.)).flex_wrap().child(div().text_size(px(12.)).text_color(t.muted).mr(px(2.)).child(label.to_string()))
}

impl Reader {
    /// The open column menu, floating at the click, or nothing.
    pub fn column_menu(&mut self, t: Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let g = self.grid.as_ref()?;
        let (c, at) = g.menu?;
        let (found, read) = g.doc.found_type(c);
        let f = g.fmts.get(c)?.clone();
        let kind = f.kind;
        let ty = kind.typed().map_or(found, |k| k.0);
        let set_kind = |k: Kind| cx.listener(move |r: &mut Reader, _: &ClickEvent, _, cx| r.set_kind(c, k, cx));
        let set_show = |s: Show| cx.listener(move |r: &mut Reader, _: &ClickEvent, _, cx| r.set_show(c, s.clone(), cx));

        let mut types = v_flex()
            .gap(px(2.))
            .child(caption("Type", t))
            .child(radio("k-auto", kind == Kind::Auto, format!("Auto: {}", type_name(found, read)), t).on_click(set_kind(Kind::Auto)))
            .child(radio("k-text", kind == Kind::Text, "Text", t).on_click(set_kind(Kind::Text)))
            .child(radio("k-num", matches!(kind, Kind::Number { .. }), "Number", t).on_click(set_kind(match kind {
                Kind::Number { .. } => kind,
                _ => Kind::Number { comma: read.comma },
            })));
        if let Kind::Number { comma } = kind {
            types = types.child(
                chips("Decimal symbol", t)
                    .child(chip("dec-dot", !comma, ".", t).on_click(set_kind(Kind::Number { comma: false })))
                    .child(chip("dec-comma", comma, ",", t).on_click(set_kind(Kind::Number { comma: true }))),
            );
        }
        types = types.child(radio("k-date", matches!(kind, Kind::Date { .. }), "Date", t).on_click(set_kind(match kind {
            Kind::Date { .. } => kind,
            _ => Kind::Date { order: read.order },
        })));
        if let Kind::Date { order } = kind {
            let mut row = chips("Order", t);
            for (o, name) in [(Order::Dmy, "DMY"), (Order::Mdy, "MDY"), (Order::Ymd, "YMD")] {
                row = row.child(chip(name, order == o, name, t).on_click(set_kind(Kind::Date { order: o })));
            }
            types = types.child(row);
        }
        types = types.child(radio("k-bool", kind == Kind::Bool, "Yes-No", t).on_click(set_kind(Kind::Bool)));

        let s = if fits(&f.show, ty) { f.show.clone() } else { Show::Plain };
        let same = |a: &Show, b: &Show| std::mem::discriminant(a) == std::mem::discriminant(b);
        let mut shows = v_flex().gap(px(2.)).child(caption("Show", t));
        match family(ty) {
            Family::Number => {
                shows = shows.child(radio("s-plain", s == Show::Plain, "As in the file", t).on_click(set_show(Show::Plain)));
                let picks = [
                    ("Number", Show::Number { sep: true, places: None }),
                    ("Percent", Show::Percent { places: None }),
                    ("Currency", Show::Currency { symbol: "$".into(), places: Some(2) }),
                    ("Duration", Show::Duration { unit: Unit::S, words: false }),
                ];
                for (label, default) in picks {
                    let on = same(&s, &default);
                    shows = shows.child(radio(label, on, label, t).on_click(set_show(if on { s.clone() } else { default.clone() })));
                    if !on {
                        continue;
                    }
                    let places = |p: Option<u8>, make: &dyn Fn(Option<u8>) -> Show| {
                        let mut row = chips("Decimals", t).child(chip("p-file", p.is_none(), "As in the file", t).on_click(set_show(make(None))));
                        for d in 0..=4u8 {
                            row = row.child(chip(("p", d as usize), p == Some(d), d.to_string(), t).on_click(set_show(make(Some(d)))));
                        }
                        row
                    };
                    match s.clone() {
                        Show::Number { sep, places: p } => {
                            shows = shows
                                .child(
                                    h_flex().pl(px(30.)).child(
                                        Checkbox::new("thousands")
                                            .label("Thousands separator")
                                            .checked(sep)
                                            .on_change(cx.listener(move |r: &mut Reader, on: &bool, _, cx| r.set_show(c, Show::Number { sep: *on, places: p }, cx))),
                                    ),
                                )
                                .child(places(p, &move |p| Show::Number { sep, places: p }));
                        }
                        Show::Percent { places: p } => shows = shows.child(places(p, &|p| Show::Percent { places: p })),
                        Show::Currency { symbol, places: p } => {
                            let mut row = chips("Symbol", t);
                            for sym in ["$", "€", "£", "¥", "฿", "₹"] {
                                row = row.child(chip(sym, symbol == sym, sym, t).on_click(set_show(Show::Currency { symbol: sym.into(), places: p })));
                            }
                            let sym2 = symbol.clone();
                            shows = shows.child(row).child(places(p, &move |p| Show::Currency { symbol: sym2.clone(), places: p }));
                        }
                        Show::Duration { unit, words } => {
                            let mut row = chips("The cell holds", t);
                            for (u, name) in [(Unit::Ms, "ms"), (Unit::S, "s"), (Unit::Min, "min"), (Unit::H, "h")] {
                                row = row.child(chip(name, unit == u, name, t).on_click(set_show(Show::Duration { unit: u, words })));
                            }
                            shows = shows.child(row).child(
                                chips("Show as", t)
                                    .child(chip("clock", !words, "1:02:03", t).on_click(set_show(Show::Duration { unit, words: false })))
                                    .child(chip("words", words, "1h 2m 3s", t).on_click(set_show(Show::Duration { unit, words: true }))),
                            );
                        }
                        _ => {}
                    }
                }
            }
            Family::Date => {
                shows = shows
                    .child(radio("s-plain", s == Show::Plain, "As in the file", t).on_click(set_show(Show::Plain)))
                    .child(radio("s-date", matches!(s, Show::Date(_)), "Date", t).on_click(set_show(match s {
                        Show::Date(_) => s.clone(),
                        _ => Show::Date(DatePat::Iso),
                    })))
                    .child(radio("s-datetime", matches!(s, Show::DateTime(..)), "Date & Time", t).on_click(set_show(match s {
                        Show::DateTime(..) => s.clone(),
                        _ => Show::DateTime(DatePat::Iso, TimePat::Hm),
                    })));
                if let Show::Date(p) | Show::DateTime(p, _) = s {
                    let time = match s {
                        Show::DateTime(_, tp) => Some(tp),
                        _ => None,
                    };
                    let mut row = chips("Pattern", t);
                    for (v, name) in [(DatePat::Iso, "2024-02-05"), (DatePat::Dmy, "05/02/2024"), (DatePat::Mdy, "02/05/2024"), (DatePat::Long, "5 Feb 2024")] {
                        let next = time.map_or(Show::Date(v), |tp| Show::DateTime(v, tp));
                        row = row.child(chip(name, p == v, name, t).on_click(set_show(next)));
                    }
                    shows = shows.child(row);
                    if let Some(tp) = time {
                        let mut row = chips("Time", t);
                        for (v, name) in [(TimePat::Hm, "14:05"), (TimePat::Hms, "14:05:09"), (TimePat::H12, "2:05 PM")] {
                            row = row.child(chip(name, tp == v, name, t).on_click(set_show(Show::DateTime(p, v))));
                        }
                        shows = shows.child(row);
                    }
                }
            }
            Family::Bool => {
                shows = shows
                    .child(radio("s-plain", s == Show::Plain, "As in the file", t).on_click(set_show(Show::Plain)))
                    .child(radio("s-yes", s == Show::YesNo { ticks: false }, "Yes / No", t).on_click(set_show(Show::YesNo { ticks: false })))
                    .child(radio("s-ticks", s == Show::YesNo { ticks: true }, "✓ / ×", t).on_click(set_show(Show::YesNo { ticks: true })));
            }
            Family::String => {
                shows = shows.child(div().px(px(8.)).py(px(4.)).text_size(px(12.5)).text_color(t.muted).child("Text shows as in the file."));
            }
        }

        let name = g.doc.names.get(c).cloned().unwrap_or_default();
        let shadow = |y: f32, blur: f32, a: f32| BoxShadow { color: hsla(0.61, 0.33, 0.09, a), offset: point(px(0.), px(y)), blur_radius: px(blur), spread_radius: px(0.), inset: false };
        let panel = v_flex()
            .id("column-menu")
            .occlude()
            .w(px(300.))
            .p(px(12.))
            .gap(px(12.))
            .rounded(px(10.))
            .border_1()
            .border_color(t.line)
            .bg(t.field)
            .shadow(vec![shadow(12., 32., 0.16), shadow(2., 6., 0.08)])
            .child(div().px(px(8.)).text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink).truncate().child(name))
            .child(types)
            .child(div().h(px(1.)).bg(t.line_soft))
            .child(shows)
            .on_mouse_down_out(cx.listener(|r: &mut Reader, _: &MouseDownEvent, _, cx| {
                if let Some(g) = &mut r.grid {
                    g.menu = None;
                }
                cx.notify();
            }));
        Some(deferred(anchored().position(at).snap_to_window().child(panel)).with_priority(1).into_any_element())
    }
}
