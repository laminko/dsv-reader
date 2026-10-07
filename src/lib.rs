//! The port of bb-plugin-dsv `logic.ts` 0.10.1, plus a record index over the
//! mapped file. No GUI, so `cargo test` runs it.

use memchr::{memchr2, memchr3, memmem};
use rayon::prelude::*;
use std::cmp::Ordering as Cmp;
use std::collections::{HashMap, HashSet};
use std::ops::{Deref, RangeInclusive};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dtype {
    Int,
    Float,
    Bool,
    Date,
    /// A date with a time of day written in the file.
    DateTime,
    Time,
    String,
}

impl Dtype {
    pub fn name(self) -> &'static str {
        match self {
            Dtype::Int => "int",
            Dtype::Float => "float",
            Dtype::Bool => "bool",
            Dtype::Date => "date",
            Dtype::DateTime => "datetime",
            Dtype::Time => "time",
            Dtype::String => "string",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    Dmy,
    Mdy,
    Ymd,
}

/// How one column writes values: a decimal comma or not, the date part order, and whether a
/// 2-digit year reads (a date type set by hand only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reading {
    pub comma: bool,
    pub order: Order,
    pub short_year: bool,
}

/// The user's read-format choices. `None` is "auto", decided per column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Format {
    pub comma: Option<bool>,
    pub order: Option<Order>,
}

const LF: u8 = b'\n';
const CR: u8 = b'\r';
const BOM: &[u8] = b"\xEF\xBB\xBF";

fn bom_len(b: &[u8]) -> usize {
    if b.starts_with(BOM) { BOM.len() } else { 0 }
}

/// Index of the next delimiter or line break at or after `i`.
fn field_end(b: &[u8], mut i: usize, d: &[u8]) -> usize {
    while let Some(k) = memchr3(d[0], LF, CR, &b[i..]) {
        i += k;
        if b[i] != d[0] || b[i..].starts_with(d) {
            return i;
        }
        i += 1;
    }
    b.len()
}

fn keep(row: &[String]) -> bool {
    row.len() > 1 || !row[0].is_empty()
}

/// Each field of the record that starts at `i` (< `b.len()`), in order, as `f(column, bytes)`.
/// RFC 4180 as in `parse`; a quoted field is unescaped into `scratch`. `f` returns false to stop.
/// Returns where the next record starts, or None when `f` stopped.
pub fn each_field(b: &[u8], mut i: usize, d: &[u8], q: &[u8], scratch: &mut Vec<u8>, f: &mut dyn FnMut(usize, &[u8]) -> bool) -> Option<usize> {
    let n = b.len();
    let mut col = 0;
    loop {
        let go = if !q.is_empty() && b[i..].starts_with(q) {
            scratch.clear();
            let mut j = i + q.len();
            loop {
                let Some(k) = memmem::find(&b[j..], q) else {
                    scratch.extend_from_slice(&b[j..]);
                    i = n;
                    break;
                };
                let k = j + k;
                scratch.extend_from_slice(&b[j..k]);
                if b[k + q.len()..].starts_with(q) {
                    scratch.extend_from_slice(q);
                    j = k + 2 * q.len();
                    continue;
                }
                i = k + q.len();
                break;
            }
            // Lenient: text between a closing quote and the next delimiter stays.
            let e = field_end(b, i, d);
            scratch.extend_from_slice(&b[i..e]);
            i = e;
            f(col, scratch)
        } else {
            let e = field_end(b, i, d);
            let go = f(col, &b[i..e]);
            i = e;
            go
        };
        if !go {
            return None;
        }
        col += 1;
        if i >= n {
            return Some(n);
        }
        if b[i..].starts_with(d) {
            i += d.len();
            if i == n {
                return f(col, b"").then_some(n);
            }
            continue;
        }
        return Some(i + if b[i] == CR && b.get(i + 1) == Some(&LF) { 2 } else { 1 });
    }
}

/// One record that starts at `i` (< `b.len()`): its fields, and where the next record starts.
/// Bytes that are not UTF-8 become U+FFFD.
pub fn parse_record(b: &[u8], i: usize, d: &[u8], q: &[u8]) -> (Vec<String>, usize) {
    let mut row = Vec::new();
    let next = each_field(b, i, d, q, &mut Vec::new(), &mut |_, f| {
        row.push(String::from_utf8_lossy(f).into_owned());
        true
    });
    (row, next.unwrap_or(b.len()))
}

/**
 * RFC 4180: quoted fields, doubled-qualifier escapes, CRLF / LF / CR, line
 * breaks inside quotes, a leading BOM, and ragged rows (kept as-is). Blank
 * lines are dropped. `q` is the text qualifier: `"`, `'`, or "" for none.
 */
pub fn parse(text: &str, d: &str, q: &str) -> Vec<Vec<String>> {
    let b = text.as_bytes();
    let mut rows = Vec::new();
    let mut i = bom_len(b);
    while i < b.len() {
        let (row, next) = parse_record(b, i, d.as_bytes(), q.as_bytes());
        if keep(&row) {
            rows.push(row);
        }
        i = next;
    }
    rows
}

/// The first `k` records `parse` keeps.
fn head_rows(b: &[u8], d: &[u8], q: &[u8], k: usize) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut i = bom_len(b);
    while i < b.len() && rows.len() < k {
        let (row, next) = parse_record(b, i, d, q);
        if keep(&row) {
            rows.push(row);
        }
        i = next;
    }
    rows
}

/// Rows per `index_records` batch.
const BATCH: usize = 1 << 16;

/**
 * The start offset of each record `parse` keeps, in batches. One memchr pass
 * finds quotes and line breaks. A quote opens a field only at a field start,
 * as in `parse`. `on_batch` returns false to stop.
 */
pub fn index_records(b: &[u8], d: &[u8], q: &[u8], on_batch: &mut dyn FnMut(&[u64]) -> bool) {
    let n = b.len();
    let fq = memmem::Finder::new(q);
    let mut batch = Vec::with_capacity(BATCH);
    // A small first batch: the first rows show before much of a cold file is read.
    let mut limit = 1 << 10;
    let mut pos = bom_len(b);
    while pos < n {
        let start = pos;
        let mut i = pos;
        let end = loop {
            let hit = match q.first() {
                Some(&q0) => memchr3(q0, LF, CR, &b[i..]),
                None => memchr2(LF, CR, &b[i..]),
            };
            let Some(k) = hit else { break n };
            let j = i + k;
            if b[j] == LF || b[j] == CR {
                break j;
            }
            i = j + 1;
            if !b[j..].starts_with(q) || !(j == start || b[start..j].ends_with(d)) {
                continue;
            }
            // A quoted field: jump past its closing quote. Doubled quotes stay inside.
            let mut m = j + q.len();
            i = loop {
                let Some(k) = fq.find(&b[m..]) else { break n };
                let c = m + k + q.len();
                if !b[c..].starts_with(q) {
                    break c;
                }
                m = c + q.len();
            };
        };
        pos = if end < n && b[end] == CR && b.get(end + 1) == Some(&LF) { end + 2 } else { end + 1 };
        // Only a record of two quotes or less can parse to one empty field, which `parse` drops.
        if end - start > 2 * q.len() || (end > start && keep(&parse_record(b, start, d, q).0)) {
            batch.push(start as u64);
            if batch.len() == limit {
                if !on_batch(&batch) {
                    return;
                }
                batch.clear();
                limit = BATCH;
            }
        }
    }
    if !batch.is_empty() {
        on_batch(&batch);
    }
}

/// The file's first line break: "CRLF", "LF", "CR", or "none".
pub fn record_delimiter(b: &[u8]) -> &'static str {
    match memchr2(CR, LF, b) {
        None => "none",
        Some(i) if b[i] == LF => "LF",
        Some(i) if b.get(i + 1) == Some(&LF) => "CRLF",
        _ => "CR",
    }
}

pub const DELIMITERS: [&str; 4] = [",", "\t", ";", "|"];
const SAMPLE: usize = 65536;

/**
 * The extension's delimiter when it splits the first 50 rows consistently;
 * otherwise the candidate with the most consistent column count above 1.
 */
pub fn detect_delimiter(b: &[u8], ext: &str, q: &str) -> &'static str {
    let cut = b.len() > SAMPLE;
    let head = String::from_utf8_lossy(&b[..b.len().min(SAMPLE)]);
    let score = |d: &str| {
        let mut rows = parse(&head, d, q);
        if cut {
            rows.pop(); // the sample may cut the last row
        }
        rows.truncate(50);
        let mut freq: HashMap<usize, usize> = HashMap::new();
        for r in &rows {
            *freq.entry(r.len()).or_default() += 1;
        }
        let (mut count, mut width) = (0, 0);
        for (&w, &c) in &freq {
            if c > count || (c == count && w > width) {
                count = c;
                width = w;
            }
        }
        // Share of rows at the modal width; a wider split breaks ties.
        if width < 2 { 0.0 } else { count as f64 / rows.len() as f64 + width as f64 * 1e-6 }
    };
    let from_ext = match ext.to_ascii_lowercase().as_str() {
        "csv" => Some(","),
        "tsv" | "tab" => Some("\t"),
        "psv" => Some("|"),
        "ssv" => Some(";"),
        _ => None,
    };
    if let Some(d) = from_ext
        && score(d) >= 0.9
    {
        return d;
    }
    let mut best = from_ext.unwrap_or(",");
    let mut top = 0.0;
    for d in DELIMITERS {
        let s = score(d);
        if s > top {
            top = s;
            best = d;
        }
    }
    best
}

fn sign(b: &[u8], i: usize) -> usize {
    if matches!(b.get(i), Some(b'-' | b'+')) { i + 1 } else { i }
}

fn digits(b: &[u8], i: usize) -> usize {
    i + b[i..].iter().take_while(|c| c.is_ascii_digit()).count()
}

/// `^[-+]?\d+$`
fn is_int(v: &str) -> bool {
    let b = v.as_bytes();
    let i = sign(b, 0);
    let e = digits(b, i);
    e > i && e == b.len()
}

/// `^[-+]?(\d+<sep>?\d*|<sep>\d+)(e[-+]?\d+)?$`, case ignored.
fn is_float(v: &str, sep: u8) -> bool {
    let b = v.as_bytes();
    let i = sign(b, 0);
    let mut e = digits(b, i);
    if e > i {
        if b.get(e) == Some(&sep) {
            e = digits(b, e + 1);
        }
    } else if b.get(i) == Some(&sep) && digits(b, i + 1) > i + 1 {
        e = digits(b, i + 1);
    } else {
        return false;
    }
    if e < b.len() && b[e] | 0x20 == b'e' {
        let s = sign(b, e + 1);
        let x = digits(b, s);
        if x == s {
            return false;
        }
        e = x;
    }
    e == b.len()
}

fn is_bool(v: &str) -> bool {
    v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("false")
}

/// A month name or its first 3 letters, case ignored, as 1 to 12.
fn month(w: &str) -> Option<i64> {
    const NAMES: [&str; 12] = ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];
    NAMES.iter().position(|m| m.eq_ignore_ascii_case(w) || (w.len() == 3 && m[..3].eq_ignore_ascii_case(w))).map(|i| i as i64 + 1)
}

/// The date at the start of `s`, as [y, m, d] and its end. Four forms: `(\d{1,4})([-/.])(\d{1,2})\2(\d{1,4})`
/// in part order `order`, the year 4 digits (or 2 with `short_year`); `yyyymmdd` before a `T`, year first only; `d Mon yyyy`;
/// `Mon d, yyyy`, the comma optional. A month name reads in any order.
fn date_head(s: &str, order: Order, short_year: bool) -> Option<([i64; 3], usize)> {
    let b = s.as_bytes();
    let num = |a: usize, e: usize| s[a..e].parse::<i64>().ok();
    let a_end = digits(b, 0);
    if a_end == 0 {
        let m_end = b.iter().position(|&c| c == b' ')?;
        let mo = month(&s[..m_end])?;
        let d_end = digits(b, m_end + 1);
        let y0 = if b.get(d_end) == Some(&b',') { d_end + 1 } else { d_end };
        if !(1..=2).contains(&(d_end - m_end - 1)) || b.get(y0) != Some(&b' ') {
            return None;
        }
        let y_end = digits(b, y0 + 1);
        return (y_end - y0 == 5).then(|| Some(([num(y0 + 1, y_end)?, mo, num(m_end + 1, d_end)?], y_end))).flatten();
    }
    if a_end == 8 && matches!(b.get(8), Some(b'T' | b't')) {
        return (order == Order::Ymd).then(|| Some(([num(0, 4)?, num(4, 6)?, num(6, 8)?], 8))).flatten();
    }
    let sep = *b.get(a_end)?;
    if sep == b' ' && a_end <= 2 {
        let m0 = a_end + 1;
        let m_end = m0 + b[m0..].iter().take_while(|c| c.is_ascii_alphabetic()).count();
        let mo = month(&s[m0..m_end])?;
        if b.get(m_end) != Some(&b' ') {
            return None;
        }
        let y_end = digits(b, m_end + 1);
        return (y_end - m_end == 5).then(|| Some(([num(m_end + 1, y_end)?, mo, num(0, a_end)?], y_end))).flatten();
    }
    if !(1..=4).contains(&a_end) || !matches!(sep, b'-' | b'/' | b'.') {
        return None;
    }
    let b0 = a_end + 1;
    let b_end = digits(b, b0);
    if !(1..=2).contains(&(b_end - b0)) || b.get(b_end) != Some(&sep) {
        return None;
    }
    let c0 = b_end + 1;
    let c_end = digits(b, c0);
    if !(1..=4).contains(&(c_end - c0)) {
        return None;
    }
    let (y_len, d_len) = if order == Order::Ymd { (a_end, c_end - c0) } else { (c_end - c0, a_end) };
    if !(y_len == 4 || short_year && y_len == 2) || d_len > 2 {
        return None;
    }
    let (a, m, c) = (num(0, a_end)?, num(b0, b_end)?, num(c0, c_end)?);
    let [y, mo, d] = match order {
        Order::Ymd => [a, m, c],
        Order::Dmy => [c, m, a],
        Order::Mdy => [c, a, m],
    };
    // A 2-digit year as `%y` reads it: 00–68 are 2000–2068, 69–99 are 1969–1999.
    let y = if y_len == 2 { y + if y < 69 { 2000 } else { 1900 } } else { y };
    Some(([y, mo, d], c_end))
}

/// A zone, ignored: an optional space, then `Z`, `±hh`, `±hhmm`, or `±hh:mm` (hh up to 14, mm below 60).
/// The end of the zone, `i` without one, or None for a bad zone.
fn zone(b: &[u8], i: usize) -> Option<usize> {
    let j = if b.get(i) == Some(&b' ') && matches!(b.get(i + 1), Some(b'Z' | b'z' | b'+' | b'-')) { i + 1 } else { i };
    match b.get(j) {
        Some(b'Z' | b'z') => Some(j + 1),
        Some(b'+' | b'-') => {
            let n = |a: usize| i64::from(b[a] - b'0') * 10 + i64::from(b[a + 1] - b'0');
            let e = digits(b, j + 1);
            let (end, mm) = match e - j - 1 {
                2 if b.get(e) == Some(&b':') && digits(b, e + 1) == e + 3 => (e + 3, n(e + 1)),
                2 => (e, 0),
                4 => (e, n(j + 3)),
                _ => return None,
            };
            (n(j + 1) <= 14 && mm < 60).then_some(end)
        }
        _ => Some(i),
    }
}

/// A time of day from byte `i` to the end of `s`, as [h, m, s, ms]: `h:mm(:ss(.f+)?)?`, or
/// `hhmm(ss)?(.f+)?` when `compact`, then ` ?(AM|PM)` (case ignored) and a [`zone`], both optional.
/// `hours`: how many digits h can have. Minutes and seconds are 2 digits below 60. With AM or PM,
/// h is 1 to 12. Digits past the milliseconds are cut.
fn clock(s: &str, i: usize, hours: RangeInclusive<usize>, compact: bool) -> Option<[i64; 4]> {
    let b = s.as_bytes();
    let two = |a: usize| s.get(a..a + 2).filter(|t| t.bytes().all(|c| c.is_ascii_digit())).and_then(|t| t.parse::<i64>().ok()).filter(|&n| n < 60);
    let (h_end, mi, sec, mut j);
    if compact {
        let e = digits(b, i);
        if !matches!(e - i, 4 | 6) {
            return None;
        }
        (h_end, mi, j) = (i + 2, two(i + 2)?, e);
        sec = if e - i == 6 { Some(two(i + 4)?) } else { None };
    } else {
        h_end = digits(b, i);
        if !hours.contains(&(h_end - i)) || b.get(h_end) != Some(&b':') {
            return None;
        }
        (mi, j) = (two(h_end + 1)?, h_end + 3);
        sec = if b.get(j) == Some(&b':') { Some(two(j + 1)?) } else { None };
        j += if sec.is_some() { 3 } else { 0 };
    }
    let mut ms = 0;
    if sec.is_some() && b.get(j) == Some(&b'.') {
        let e = digits(b, j + 1);
        if e == j + 1 {
            return None;
        }
        let k = e.min(j + 4);
        ms = b[j + 1..k].iter().fold(0, |n, c| n * 10 + i64::from(c - b'0')) * 10_i64.pow((j + 4 - k) as u32);
        j = e;
    }
    let mut h = s[i..h_end].parse::<i64>().ok()?;
    let k = j + usize::from(b.get(j) == Some(&b' '));
    if let Some(w) = s.get(k..k + 2).filter(|w| w.eq_ignore_ascii_case("am") || w.eq_ignore_ascii_case("pm")) {
        if !(1..=12).contains(&h) {
            return None;
        }
        h = h % 12 + if w.eq_ignore_ascii_case("pm") { 12 } else { 0 };
        j = k + 2;
    }
    (zone(b, j)? == b.len()).then_some([h, mi, sec.unwrap_or(0), ms])
}

/// Days from 1970-01-01 to a proleptic Gregorian date (H. Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

// Limit: time-zone offsets are ignored; parse them when a file mixes zones.
/**
 * UTC milliseconds for a date in part order `order`, as `Date.UTC` gives them.
 * The forms are in [`date_head`]. A time may follow, after `T`, a space, or ", ": see [`clock`].
 */
pub fn to_date(v: &str, order: Order) -> Option<i64> {
    date_time(v, order, false).map(|(ms, _)| ms)
}

/// [`to_date`] with the order of a column's reading, and its 2-digit years when the type was set by hand.
fn read_date(v: &str, r: &Reading) -> Option<i64> {
    date_time(v, r.order, r.short_year).map(|(ms, _)| ms)
}

/// [`to_date`], and whether a time of day follows the date.
fn date_time(v: &str, order: Order, short_year: bool) -> Option<(i64, bool)> {
    let s = v.trim();
    let b = s.as_bytes();
    let ([y, mo, d], end) = date_head(s, order, short_year)?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let [h, mi, sec, _] = match b.get(end) {
        None => [0; 4],
        Some(b'T' | b't' | b' ') => clock(s, end + 1, 1..=2, digits(b, 0) == 8)?,
        Some(b',') if b.get(end + 1) == Some(&b' ') => clock(s, end + 2, 1..=2, false)?,
        _ => return None,
    };
    // Date.UTC reads years 0–99 as 1900–1999, and a day past the month's end rolls over.
    let y = if y <= 99 { 1900 + y } else { y };
    let days = days_from_civil(y, mo, 1) + d - 1;
    Some(((((days * 24 + h) * 60 + mi) * 60 + sec) * 1000, end < b.len()))
}

/// Milliseconds in a time of day or a duration, `h+:mm(:ss(.f+)?)?`: "14:05", "00:02:05.881533",
/// "100:00:00". An AM or PM ("2:05 PM") and a zone ("09:05:48+00", ignored) can follow: see [`clock`].
pub fn to_time(v: &str) -> Option<i64> {
    let [h, mi, sec, ms] = clock(v.trim(), 0, 1..=9, false)?;
    Some(((h * 60 + mi) * 60 + sec) * 1000 + ms)
}

/// Whether a trimmed value is of dtype `t` when read with `r`.
fn fits(t: Dtype, r: &Reading, v: &str) -> bool {
    match t {
        Dtype::Bool => is_bool(v),
        Dtype::Int => is_int(v),
        Dtype::Float => is_float(v, if r.comma { b',' } else { b'.' }),
        Dtype::Date | Dtype::DateTime => read_date(v, r).is_some(),
        Dtype::Time => to_time(v).is_some(),
        Dtype::String => true,
    }
}

/**
 * The first of bool, int, float, date, time that ≥95% of up to 1,000 non-empty
 * values match, else string. Empty cells are nulls. Auto decimal tries "."
 * then ",". Auto date order takes the order that reads the most values; a
 * tie (every day and month ≤ 12) goes to DMY.
 */
pub fn infer_column<'a>(values: impl IntoIterator<Item = &'a str>, f: &Format) -> (Dtype, Reading) {
    let sample: Vec<&str> = values.into_iter().map(str::trim).filter(|s| !s.is_empty()).take(1000).collect();
    let base = Reading { comma: f.comma.unwrap_or(false), order: f.order.unwrap_or(Order::Ymd), short_year: false };
    if sample.is_empty() {
        return (Dtype::String, base);
    }
    let hits = |t: Dtype, r: &Reading| sample.iter().filter(|v| fits(t, r, v)).count();
    let enough = |k: usize| k as f64 >= sample.len() as f64 * 0.95;
    if enough(hits(Dtype::Bool, &base)) {
        return (Dtype::Bool, base);
    }
    if enough(hits(Dtype::Int, &base)) {
        return (Dtype::Int, base);
    }
    let commas = match f.comma {
        None => vec![false, true],
        Some(c) => vec![c],
    };
    for comma in commas {
        let read = Reading { comma, ..base };
        if enough(hits(Dtype::Float, &read)) {
            return (Dtype::Float, read);
        }
    }
    let orders = match f.order {
        None => vec![Order::Ymd, Order::Dmy, Order::Mdy],
        Some(o) => vec![o],
    };
    let (mut best, mut top) = (base, 0);
    for order in orders {
        let read = Reading { order, ..base };
        let k = hits(Dtype::Date, &read);
        if k > top {
            top = k;
            best = read;
        }
    }
    if top > 0 && enough(top) {
        // A written time of day after the date makes a date & time.
        let timed = sample.iter().any(|v| date_time(v, best.order, false).is_some_and(|(_, t)| t));
        return (if timed { Dtype::DateTime } else { Dtype::Date }, best);
    }
    if enough(hits(Dtype::Time, &base)) { (Dtype::Time, base) } else { (Dtype::String, base) }
}

fn column(rows: &[Vec<String>], c: usize) -> impl Iterator<Item = &str> {
    rows.iter().map(move |r| r.get(c).map_or("", String::as_str))
}

/**
 * Typed columns decide: text in row 1 over a typed body means a header. For
 * all-text data, unique row-1 labels that do not recur below mean a header.
 */
pub fn detect_header(rows: &[Vec<String>], f: &Format) -> (bool, String) {
    if rows.len() < 2 {
        return (false, "only one row".into());
    }
    let first = &rows[0];
    let body = &rows[1..rows.len().min(1001)];
    let mut typed = false;
    for (c, v) in first.iter().enumerate() {
        let (t, read) = infer_column(column(body, c), f);
        if t == Dtype::String {
            continue;
        }
        typed = true;
        let v = v.trim();
        if !v.is_empty() && !fits(t, &read, v) {
            return (true, format!("row 1 is text, rows 2+ are {} in column {}", t.name(), c + 1));
        }
    }
    if typed {
        return (false, "row 1 has the same types as rows 2+".into());
    }
    let mut seen = HashSet::new();
    for (c, v) in first.iter().enumerate() {
        let v = v.trim();
        if v.is_empty() || !seen.insert(v) || body.iter().any(|r| r.get(c).is_some_and(|x| x.trim() == v)) {
            return (false, "row 1 looks like data".into());
        }
    }
    (true, "row 1 labels are unique and not repeated below".into())
}

/// Column names, and a dtype and reading per column inferred from the first 1,000 body rows of `all`.
pub fn columns(all: &[Vec<String>], header: bool, f: &Format) -> (Vec<String>, Vec<Dtype>, Vec<Reading>) {
    let body = &all[(header as usize).min(all.len())..];
    let sample = &body[..body.len().min(1000)];
    let width = all.iter().map(Vec::len).max().unwrap_or(0);
    let mut names = Vec::with_capacity(width);
    let (mut dtypes, mut reads) = (Vec::with_capacity(width), Vec::with_capacity(width));
    for c in 0..width {
        let name = if header { all[0].get(c).map_or("", |s| s.trim()) } else { "" };
        names.push(if name.is_empty() { format!("column {}", c + 1) } else { name.to_string() });
        let (t, r) = infer_column(column(sample, c), f);
        dtypes.push(t);
        reads.push(r);
    }
    (names, dtypes, reads)
}

/// The encodings the Format menu offers, as [encoding_rs label, name].
pub const ENCODINGS: [(&str, &str); 12] = [
    ("utf-8", "UTF-8"),
    ("utf-16le", "UTF-16 LE"),
    ("utf-16be", "UTF-16 BE"),
    ("windows-874", "Windows-874 (Thai, TIS-620)"),
    ("windows-1252", "Windows-1252 (Western, ISO-8859-1)"),
    ("windows-1250", "Windows-1250 (Central European)"),
    ("windows-1251", "Windows-1251 (Cyrillic)"),
    ("shift_jis", "Shift_JIS (Japanese)"),
    ("euc-jp", "EUC-JP (Japanese)"),
    ("gb18030", "GB18030 (Simplified Chinese)"),
    ("big5", "Big5 (Traditional Chinese)"),
    ("euc-kr", "EUC-KR (Korean)"),
];

/// Bytes as text in the encoding `label`, and how many characters the decoder could not read (each became U+FFFD).
pub fn decode(b: &[u8], label: &str) -> (String, usize) {
    let enc = encoding_rs::Encoding::for_label(label.as_bytes()).unwrap_or(encoding_rs::UTF_8);
    let (text, had_errors) = enc.decode_with_bom_removal(b);
    let bad = if had_errors { text.matches('\u{FFFD}').count() } else { 0 };
    (text.into_owned(), bad)
}

/// How many characters a UTF-8 decoder replaces with U+FFFD in `b`.
pub fn count_bad_utf8(mut b: &[u8]) -> usize {
    let mut bad = 0;
    while let Err(e) = std::str::from_utf8(b) {
        bad += 1;
        match e.error_len() {
            Some(len) => b = &b[e.valid_up_to() + len..],
            None => break,
        }
    }
    bad
}

/// The file as UTF-8: mapped when it is UTF-8, else decoded once.
pub enum Bytes {
    Map(memmap2::Mmap),
    Owned(Vec<u8>),
}

impl Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Bytes::Map(m) => m,
            Bytes::Owned(v) => v,
        }
    }
}

/// Record starts, filled by a background thread.
#[derive(Default)]
pub struct Index {
    /// Shared with scans once `done`; nothing writes it after that.
    pub starts: Mutex<Arc<Vec<u64>>>,
    pub done: AtomicBool,
    pub cancel: AtomicBool,
    /// Milliseconds to index, set when `done`.
    pub ms: AtomicU64,
    /// Characters the decoder could not read, set when `done`.
    pub bad: AtomicUsize,
    /// The most fields in any record, set after `done`; 0 until then.
    pub width: AtomicUsize,
}

/// How to read a file. `None` means detect.
#[derive(Clone, Debug, PartialEq)]
pub struct Opts {
    pub delim: Option<String>,
    pub quote: String,
    pub header: Option<bool>,
    /// A label from `ENCODINGS`. `None`: UTF-16 when the file starts with its BOM, else UTF-8.
    pub encoding: Option<&'static str>,
    pub format: Format,
}

impl Default for Opts {
    fn default() -> Self {
        Opts { delim: None, quote: "\"".into(), header: None, encoding: None, format: Format::default() }
    }
}

/// Sort ranks per column, for one header choice and one set of column types.
type Ranks = Arc<Mutex<HashMap<usize, Arc<Vec<u32>>>>>;

/// An open file: its bytes, the detected format, and the record index.
pub struct Doc {
    pub path: PathBuf,
    pub data: Arc<Bytes>,
    pub index: Arc<Index>,
    pub encoding: &'static str,
    pub delim: String,
    pub quote: String,
    pub eol: &'static str,
    pub header: bool,
    pub reason: String,
    pub names: Vec<String>,
    pub dtypes: Vec<Dtype>,
    pub reads: Vec<Reading>,
    /// The first 1,001 records, for header and dtype detection.
    sample: Vec<Vec<String>>,
    ranks: Ranks,
}

impl Doc {
    /// Maps the file, decodes it when it is not UTF-8, and calls `from_bytes`.
    // Limit: a file in another encoding decodes on this thread before the first paint; decode on the index thread if that is slow.
    pub fn open(path: &Path, opts: &Opts, notify: impl Fn() + Send + 'static) -> std::io::Result<Doc> {
        let file = std::fs::File::open(path)?;
        // Limit: a mapped file that shrinks on disk while open can crash the app (SIGBUS); copy it if that matters.
        let map = if file.metadata()?.len() == 0 { None } else { Some(unsafe { memmap2::Mmap::map(&file)? }) };
        let raw: &[u8] = map.as_deref().unwrap_or(&[]);
        let encoding = opts.encoding.unwrap_or(match raw {
            [0xFF, 0xFE, ..] => "utf-16le",
            [0xFE, 0xFF, ..] => "utf-16be",
            _ => "utf-8",
        });
        let (data, bad) = if encoding == "utf-8" {
            (map.map_or(Bytes::Owned(Vec::new()), Bytes::Map), None)
        } else {
            let (text, bad) = decode(raw, encoding);
            (Bytes::Owned(text.into_bytes()), Some(bad))
        };
        Ok(Doc::from_bytes(data, bad, encoding, path, opts, notify))
    }

    /// Detects the format from the head of `data` and starts the index thread. `bad` counts the
    /// characters a decoder could not read; None counts them as UTF-8 on the index thread.
    /// `notify` runs after each batch of rows, at `done`, and when the width is known.
    pub fn from_bytes(data: Bytes, bad: Option<usize>, encoding: &'static str, path: &Path, opts: &Opts, notify: impl Fn() + Send + 'static) -> Doc {
        let data = Arc::new(data);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let delim = match &opts.delim {
            Some(d) if !d.is_empty() => d.clone(),
            _ => detect_delimiter(&data, ext, &opts.quote).to_string(),
        };
        let sample = head_rows(&data, delim.as_bytes(), opts.quote.as_bytes(), 1001);
        let index = Arc::new(Index::default());
        // A cold file: the kernel reads it ahead of the index, not one page fault at a time. On
        // its own thread, because `WillNeed` waits until the whole file is read, and the first
        // rows must not. Not `Sequential`: it let macOS drop pages behind the index, so a later
        // search of one column was 8 % slower. Limit: it asks for the whole file, so a file near
        // the memory size can push other files out of the cache.
        if matches!(&*data, Bytes::Map(_)) {
            let ahead = data.clone();
            std::thread::spawn(move || {
                if let Bytes::Map(m) = &*ahead {
                    m.advise(memmap2::Advice::WillNeed).ok();
                }
            });
        }
        let (b, idx, d, q) = (data.clone(), index.clone(), delim.clone(), opts.quote.clone());
        std::thread::spawn(move || {
            let (d, q) = (d.as_bytes(), q.as_bytes());
            let t = Instant::now();
            index_records(&b, d, q, &mut |batch| {
                // No scan holds the Arc before `done`, so this does not copy.
                Arc::make_mut(&mut *idx.starts.lock().unwrap()).extend_from_slice(batch);
                notify();
                !idx.cancel.load(Ordering::Relaxed)
            });
            idx.ms.store(t.elapsed().as_millis() as u64, Ordering::Relaxed);
            idx.bad.store(bad.unwrap_or_else(|| count_bad_utf8(&b)), Ordering::Relaxed);
            idx.done.store(true, Ordering::Release);
            notify();
            let starts = idx.starts.lock().unwrap().clone();
            let width = starts
                .par_chunks(CHUNK)
                .map(|chunk| {
                    if idx.cancel.load(Ordering::Relaxed) {
                        return 0;
                    }
                    let mut scratch = Vec::new();
                    let mut count = |s: u64| {
                        let mut w = 0;
                        each_field(&b, s as usize, d, q, &mut scratch, &mut |c, _| {
                            w = c + 1;
                            true
                        });
                        w
                    };
                    chunk.iter().map(|&s| count(s)).max().unwrap_or(0)
                })
                .max()
                .unwrap_or(0);
            idx.width.store(width, Ordering::Relaxed);
            notify();
        });
        let mut doc = Doc {
            path: path.to_path_buf(),
            eol: record_delimiter(&data),
            data,
            index,
            encoding,
            delim,
            quote: opts.quote.clone(),
            header: false,
            reason: String::new(),
            names: Vec::new(),
            dtypes: Vec::new(),
            reads: Vec::new(),
            sample,
            ranks: Ranks::default(),
        };
        doc.retype(opts.header, &opts.format);
        doc
    }

    /// Decides the header and the column types again, with no new index. Sort ranks are dropped.
    pub fn retype(&mut self, header: Option<bool>, f: &Format) {
        (self.header, self.reason) = match header {
            Some(h) => (h, if h { "on" } else { "off" }.into()),
            None => detect_header(&self.sample, f),
        };
        (self.names, self.dtypes, self.reads) = columns(&self.sample, self.header, f);
        self.ranks = Ranks::default();
    }

    /// Adds string columns up to `width`, for rows wider than the sample.
    pub fn widen(&mut self, width: usize) {
        while self.names.len() < width {
            self.names.push(format!("column {}", self.names.len() + 1));
            self.dtypes.push(Dtype::String);
            self.reads.push(PLAIN);
        }
    }

    /// Data rows indexed so far.
    pub fn rows(&self) -> usize {
        self.index.starts.lock().unwrap().len().saturating_sub(self.header as usize)
    }

    /// Data row `i`, parsed, or empty when it is not indexed yet.
    pub fn row(&self, i: usize) -> Vec<String> {
        let start = self.index.starts.lock().unwrap().get(i + self.header as usize).copied();
        start.map_or_else(Vec::new, |s| parse_record(&self.data, s as usize, self.delim.as_bytes(), self.quote.as_bytes()).0)
    }

    /// Data row `i`, parsed up to its first `cols` fields, or empty when it is not indexed yet.
    pub fn row_upto(&self, i: usize, cols: usize) -> Vec<String> {
        let mut row = Vec::new();
        if let (Some(s), true) = (self.index.starts.lock().unwrap().get(i + self.header as usize).copied(), cols > 0) {
            let (d, q) = (self.delim.as_bytes(), self.quote.as_bytes());
            each_field(&self.data, s as usize, d, q, &mut Vec::new(), &mut |c, f| {
                row.push(String::from_utf8_lossy(f).into_owned());
                c + 1 < cols
            });
        }
        row
    }

    /// What search, filters, and sort read, once the index is done.
    pub fn scan(&self) -> Option<Scan> {
        self.index.done.load(Ordering::Acquire).then(|| Scan {
            data: self.data.clone(),
            starts: self.index.starts.lock().unwrap().clone(),
            header: self.header as usize,
            d: self.delim.as_bytes().to_vec(),
            q: self.quote.as_bytes().to_vec(),
            dtypes: self.dtypes.clone(),
            reads: self.reads.clone(),
            ranks: self.ranks.clone(),
        })
    }
}

impl Drop for Doc {
    fn drop(&mut self) {
        self.index.cancel.store(true, Ordering::Relaxed);
    }
}

/// The unit of a number that shows as a duration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Ms,
    S,
    Min,
    H,
}

/// How a date shows: 2024-02-05, 05/02/2024, 02/05/2024, or 5 Feb 2024.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatePat {
    Iso,
    Dmy,
    Mdy,
    Long,
}

/// How a time shows: 14:05, 14:05:09, or 2:05 PM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimePat {
    Hm,
    Hms,
    H12,
}

/// How a column shows its cells on screen. `Plain` shows the file's text. Copy and Export
/// always use the file's text. `places` None keeps the decimals that the file writes.
#[derive(Clone, Debug, PartialEq)]
pub enum Show {
    Plain,
    Number {
        sep: bool,
        places: Option<u8>,
    },
    Percent {
        places: Option<u8>,
    },
    Currency {
        symbol: String,
        places: Option<u8>,
    },
    /// `words`: "1h 2m 3s"; else "1:02:03". A time column holds ms: its `unit` is not used.
    Duration {
        unit: Unit,
        words: bool,
    },
    Date(DatePat),
    DateTime(DatePat, TimePat),
    /// A time column as a time of day.
    Time(TimePat),
    /// `ticks`: ✓ and × (no loaded font draws ✗); else Yes and No.
    YesNo {
        ticks: bool,
    },
}

/// A column's type, set by hand when detection (Auto) is wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Auto,
    Text,
    Number { comma: bool },
    Date { order: Order },
    DateTime { order: Order },
    Time,
    Bool,
}

impl Kind {
    /// The dtype and reading this kind sets, or None for Auto.
    pub fn typed(self) -> Option<(Dtype, Reading)> {
        match self {
            Kind::Auto => None,
            Kind::Text => Some((Dtype::String, PLAIN)),
            Kind::Number { comma } => Some((Dtype::Float, Reading { comma, order: Order::Ymd, short_year: false })),
            Kind::Date { order } => Some((Dtype::Date, Reading { comma: false, order, short_year: true })),
            Kind::DateTime { order } => Some((Dtype::DateTime, Reading { comma: false, order, short_year: true })),
            Kind::Time => Some((Dtype::Time, PLAIN)),
            Kind::Bool => Some((Dtype::Bool, PLAIN)),
        }
    }
}

/// Digits after the decimal symbol, as the file writes the number. None with an exponent.
fn decimals(v: &str, comma: bool) -> Option<usize> {
    let s = v.trim();
    if s.contains(['e', 'E']) {
        return None;
    }
    Some(s.split_once(if comma { ',' } else { '.' }).map_or(0, |(_, f)| f.len()))
}

fn nonzero(s: &str) -> bool {
    s.bytes().any(|b| (b'1'..=b'9').contains(&b))
}

/// `n` with `places` decimals (None: as short as it reads), thousands grouped when `sep`, in the
/// style of the file: "1,234.5", or "1.234,5" when the file writes a decimal comma. No "-0".
fn number_text(n: f64, places: Option<usize>, sep: bool, comma: bool) -> String {
    let s = match places {
        Some(p) => format!("{:.*}", p, n.abs()),
        None => format!("{}", n.abs()),
    };
    let (int, frac) = s.split_once('.').unwrap_or((&s, ""));
    let (group, point) = if comma { ('.', ',') } else { (',', '.') };
    let mut out = String::with_capacity(s.len() + s.len() / 3 + 1);
    if n < 0.0 && nonzero(&s) {
        out.push('-');
    }
    for (i, ch) in int.chars().enumerate() {
        if sep && i > 0 && (int.len() - i) % 3 == 0 {
            out.push(group);
        }
        out.push(ch);
    }
    if !frac.is_empty() {
        out.push(point);
        out.push_str(frac);
    }
    out
}

/// The date of a day number from 1970-01-01 (H. Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn date_text(ms: i64, p: DatePat) -> String {
    let (y, m, d) = civil_from_days(ms.div_euclid(86_400_000));
    match p {
        DatePat::Iso => format!("{y:04}-{m:02}-{d:02}"),
        DatePat::Dmy => format!("{d:02}/{m:02}/{y:04}"),
        DatePat::Mdy => format!("{m:02}/{d:02}/{y:04}"),
        DatePat::Long => format!("{d} {} {y}", MONTHS[(m - 1) as usize]),
    }
}

fn time_text(ms: i64, p: TimePat) -> String {
    let secs = ms.rem_euclid(86_400_000) / 1000;
    let (h, mi, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    match p {
        TimePat::Hm => format!("{h:02}:{mi:02}"),
        TimePat::Hms => format!("{h:02}:{mi:02}:{s:02}"),
        TimePat::H12 => format!("{}:{mi:02} {}", if h % 12 == 0 { 12 } else { h % 12 }, if h < 12 { "AM" } else { "PM" }),
    }
}

fn duration_text(n: f64, unit: Unit, words: bool) -> String {
    let secs = n * match unit {
        Unit::Ms => 0.001,
        Unit::S => 1.0,
        Unit::Min => 60.0,
        Unit::H => 3600.0,
    };
    let t = secs.abs().round() as u64;
    let sign = if secs < 0.0 && t > 0 { "-" } else { "" };
    if !words {
        return format!("{sign}{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60);
    }
    let parts = [(t / 86_400, "d"), (t / 3600 % 24, "h"), (t / 60 % 60, "m"), (t % 60, "s")];
    let text: Vec<String> = parts.iter().filter(|p| p.0 > 0).map(|(v, u)| format!("{v}{u}")).collect();
    if text.is_empty() { "0s".into() } else { format!("{sign}{}", text.join(" ")) }
}

/// A cell as the column format `s` shows it, read as dtype `t` with `r`. None means: show the
/// file's text, because `s` is Plain, does not fit the type, or the cell does not read.
pub fn show(v: &str, t: Dtype, r: Reading, s: &Show) -> Option<String> {
    let num = || matches!(t, Dtype::Int | Dtype::Float).then(|| to_number(v, r.comma)).filter(|n| n.is_finite());
    let date = || (family(t) == Family::Date).then(|| read_date(v, &r)).flatten();
    let time = || (t == Dtype::Time).then(|| to_time(v)).flatten();
    match s {
        Show::Plain => None,
        Show::Number { sep, places } => {
            let n = num()?;
            Some(number_text(n, places.map(usize::from).or_else(|| decimals(v, r.comma)), *sep, r.comma))
        }
        Show::Percent { places } => {
            let n = num()?;
            let p = places.map(usize::from).or_else(|| decimals(v, r.comma).map(|d| d.saturating_sub(2)));
            Some(number_text(n * 100.0, p, true, r.comma) + "%")
        }
        Show::Currency { symbol, places } => {
            let n = num()?;
            let body = number_text(n.abs(), places.map(usize::from).or_else(|| decimals(v, r.comma)), true, r.comma);
            let sign = if n < 0.0 && nonzero(&body) { "-" } else { "" };
            Some(format!("{sign}{symbol}{body}"))
        }
        Show::Duration { words, .. } if t == Dtype::Time => Some(duration_text(time()? as f64, Unit::Ms, *words)),
        Show::Duration { unit, words } => Some(duration_text(num()?, *unit, *words)),
        Show::Time(tp) => Some(time_text(time().or_else(date)?, *tp)),
        Show::Date(p) => Some(date_text(date()?, *p)),
        Show::DateTime(p, tp) => {
            let ms = date()?;
            Some(format!("{} {}", date_text(ms, *p), time_text(ms, *tp)))
        }
        Show::YesNo { ticks } => {
            let s = v.trim();
            (t == Dtype::Bool && is_bool(s)).then(|| {
                let yes = s.eq_ignore_ascii_case("true");
                match (ticks, yes) {
                    (true, true) => "✓",
                    (true, false) => "×",
                    (false, true) => "Yes",
                    (false, false) => "No",
                }
                .to_string()
            })
        }
    }
}

impl Doc {
    /// The type that detection finds for column `c`, from the first 1,000 body rows.
    pub fn found_type(&self, c: usize) -> (Dtype, Reading) {
        let body = &self.sample[(self.header as usize).min(self.sample.len())..];
        infer_column(column(&body[..body.len().min(1000)], c), &Format::default())
    }

    /// Of the non-empty cells of column `c` in the first 1,000 body rows: how many read as its
    /// type, how many there are, and whether the body has more rows than that.
    pub fn sample_reads(&self, c: usize) -> (usize, usize, bool) {
        let body = &self.sample[(self.header as usize).min(self.sample.len())..];
        let (t, r) = (self.dtypes.get(c).copied().unwrap_or(Dtype::String), self.reads.get(c).copied().unwrap_or(PLAIN));
        let cells: Vec<&str> = column(&body[..body.len().min(1000)], c).map(str::trim).filter(|s| !s.is_empty()).collect();
        (cells.iter().filter(|v| fits(t, &r, v)).count(), cells.len(), body.len() > 1000)
    }

    /// Column `c` takes the type of `kind`; Auto finds it again. Cached sort ranks are dropped.
    pub fn set_kind(&mut self, c: usize, kind: Kind) {
        let (t, r) = kind.typed().unwrap_or_else(|| self.found_type(c));
        if c < self.dtypes.len() {
            self.dtypes[c] = t;
            self.reads[c] = r;
        }
        self.ranks = Ranks::default();
    }
}

/// A selection by position on screen: the active cell (ar, ac) and the far corner (fr, fc).
/// Rows index the rows on screen, in order. Columns index the shown columns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sel {
    pub ar: usize,
    pub ac: usize,
    pub fr: usize,
    pub fc: usize,
}

impl Sel {
    /// One cell.
    pub fn at(r: usize, c: usize) -> Sel {
        Sel { ar: r, ac: c, fr: r, fc: c }
    }

    /// (top, left, bottom, right), inclusive.
    pub fn bounds(&self) -> (usize, usize, usize, usize) {
        (self.ar.min(self.fr), self.ac.min(self.fc), self.ar.max(self.fr), self.ac.max(self.fc))
    }

    /// An arrow key: move the active cell and collapse the range, or with `extend` move the far
    /// corner. Both stop at the edges of a rows × cols grid.
    pub fn step(self, dr: isize, dc: isize, extend: bool, rows: usize, cols: usize) -> Sel {
        if rows == 0 || cols == 0 {
            return self;
        }
        let clamp = |v: usize, d: isize, n: usize| (v as isize + d).clamp(0, n as isize - 1) as usize;
        if extend { Sel { fr: clamp(self.fr, dr, rows), fc: clamp(self.fc, dc, cols), ..self } } else { Sel::at(clamp(self.ar, dr, rows), clamp(self.ac, dc, cols)) }
    }
}

/// The position of the last value ≤ `x` in an ascending list, or None.
pub fn floor_index(sorted: &[u32], x: u32) -> Option<usize> {
    sorted.partition_point(|&v| v <= x).checked_sub(1)
}

/**
 * `order` with the column at position `from` moved in front of the column at position `to`.
 * `to` is a drop position, so `order.len()` means "after the last column", and a drop on either
 * side of the dragged column gives the same order back. A position outside the list changes nothing.
 */
pub fn arrange(order: &[usize], from: usize, to: usize) -> Vec<usize> {
    let mut next = order.to_vec();
    if from >= order.len() || to > order.len() {
        return next;
    }
    let col = next.remove(from);
    next.insert(if from < to { to - 1 } else { to }, col);
    next
}

/**
 * Delimited text: `d` between cells, `eol` between rows. A cell with `d`, a line break, or the
 * text qualifier `q` is put in `q`, with each `q` inside it doubled. An empty `q` quotes nothing.
 */
pub fn to_delimited(rows: &[Vec<String>], d: &str, eol: &str, q: &str) -> String {
    let cell = |v: &String| {
        if !q.is_empty() && (v.contains(d) || v.contains(q) || v.contains(['\n', '\r'])) { format!("{q}{}{q}", v.replace(q, &format!("{q}{q}"))) } else { v.clone() }
    };
    rows.iter().map(|r| r.iter().map(cell).collect::<Vec<_>>().join(d)).collect::<Vec<_>>().join(eol)
}

/// Spreadsheet clipboard text (TSV).
pub fn to_tsv(rows: &[Vec<String>]) -> String {
    to_delimited(rows, "\t", "\n", "\"")
}

impl Doc {
    /// The selected values, row by row. `view` maps rows on screen to data rows (None: the
    /// same). `cols` are the shown columns. With `names`, the first line is the names of the
    /// selected columns.
    pub fn cells(&self, view: Option<&[u32]>, cols: &[usize], s: Sel, names: bool) -> Vec<Vec<String>> {
        let (top, left, bottom, right) = s.bounds();
        let picked = &cols[left.min(cols.len())..(right + 1).min(cols.len())];
        let mut out = Vec::with_capacity(bottom - top + 1 + names as usize);
        if names {
            out.push(picked.iter().map(|&c| self.names.get(c).cloned().unwrap_or_default()).collect());
        }
        for k in top..=bottom {
            let row = self.row(view.map_or(k, |v| v[k] as usize));
            out.push(picked.iter().map(|&c| row.get(c).cloned().unwrap_or_default()).collect());
        }
        out
    }

    /// The `k` longest values of column `col` in the data rows `rows`, longest first. Equal
    /// lengths keep the order of `rows`. Empty cells do not count, so a column with no text gives [""].
    pub fn longest(&self, rows: &[usize], col: usize, k: usize) -> Vec<String> {
        let len = |s: &str| s.chars().count();
        let mut top: Vec<String> = Vec::new();
        for &r in rows {
            let v = self.row(r).into_iter().nth(col).unwrap_or_default();
            if k == 0 || v.is_empty() || (top.len() == k && len(&v) <= len(&top[k - 1])) {
                continue;
            }
            let mut j = top.len();
            while j > 0 && len(&top[j - 1]) < len(&v) {
                j -= 1;
            }
            top.insert(j, v);
            if top.len() > k {
                top.pop();
            }
        }
        if top.is_empty() { vec![String::new()] } else { top }
    }
}

const PLAIN: Reading = Reading { comma: false, order: Order::Ymd, short_year: false };
/// The rank of empty and unreadable cells.
const LAST: u32 = u32::MAX;
/// Records per scan task.
const CHUNK: usize = 1 << 14;

/// A cell as JavaScript's `Number(v)` reads it: trimmed; decimal, 0x, 0o, 0b, or "Infinity"; else NaN.
fn js_number(v: &str) -> f64 {
    let s = v.trim();
    if s.is_empty() {
        return 0.0;
    }
    let (sign, body) = match s.as_bytes()[0] {
        b'-' => (-1.0, &s[1..]),
        b'+' => (1.0, &s[1..]),
        _ => (1.0, s),
    };
    if body == "Infinity" {
        return sign * f64::INFINITY;
    }
    let radix = match body.as_bytes() {
        [b'0', p, _, ..] if body.len() == s.len() => match p | 0x20 {
            b'x' => 16,
            b'o' => 8,
            b'b' => 2,
            _ => 0,
        },
        _ => 0,
    };
    if radix > 0 {
        let digits = &body[2..];
        let ok = digits.bytes().all(|c| c.is_ascii_alphanumeric());
        return if ok { u64::from_str_radix(digits, radix).map_or(f64::NAN, |n| n as f64) } else { f64::NAN };
    }
    if !s.bytes().all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-')) {
        return f64::NAN;
    }
    s.parse().unwrap_or(f64::NAN)
}

/// A cell as a number. With `comma`, "1,5" is 1.5 and any "." makes it NaN.
fn to_number(v: &str, comma: bool) -> f64 {
    if v.trim().is_empty() || (comma && v.contains('.')) {
        return f64::NAN;
    }
    if comma { js_number(&v.replacen(',', ".", 1)) } else { js_number(v) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Contains,
    Equals,
    /// The trimmed cell is the text, case and all: what a Profile counts as one value.
    Is,
    Starts,
    Empty,
    NotEmpty,
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Between,
    True,
    False,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Filter {
    pub col: usize,
    pub op: Op,
    pub a: String,
    pub b: String,
    /// Joined to the filters before it by "or", not "and". The first filter's is not used.
    pub or: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    String,
    Number,
    Date,
    Time,
    Bool,
}

pub fn family(t: Dtype) -> Family {
    match t {
        Dtype::Int | Dtype::Float => Family::Number,
        Dtype::Date | Dtype::DateTime => Family::Date,
        Dtype::Time => Family::Time,
        Dtype::Bool => Family::Bool,
        Dtype::String => Family::String,
    }
}

/// Operators per dtype family, as (op, label).
pub fn ops(f: Family) -> &'static [(Op, &'static str)] {
    match f {
        Family::String => &[(Op::Contains, "contains"), (Op::Equals, "equals"), (Op::Is, "is exactly"), (Op::Starts, "starts with"), (Op::Empty, "is empty"), (Op::NotEmpty, "not empty")],
        Family::Number | Family::Time => &[(Op::Eq, "="), (Op::Ne, "≠"), (Op::Gt, ">"), (Op::Ge, "≥"), (Op::Lt, "<"), (Op::Le, "≤"), (Op::Between, "between")],
        Family::Date => &[(Op::Lt, "before"), (Op::Gt, "after"), (Op::Between, "between")],
        Family::Bool => &[(Op::True, "is true"), (Op::False, "is false")],
    }
}

/// How many values an operator takes.
pub fn arity(op: Op) -> usize {
    match op {
        Op::Between => 2,
        Op::Empty | Op::NotEmpty | Op::True | Op::False => 0,
        _ => 1,
    }
}

/// A cell test.
pub type Test = Box<dyn Fn(&str) -> bool + Send + Sync>;

/**
 * Cell test for one filter, or None while the filter is incomplete, so the caller ignores it.
 * Text tests ignore case. Empty cells fail number and date tests. Cells are read with `r`.
 * The operands are ISO dates and "." decimals.
 */
pub fn predicate(f: &Filter, t: Dtype, r: Reading) -> Option<Test> {
    let a = f.a.trim().to_lowercase();
    match f.op {
        Op::Contains => return (!a.is_empty()).then(|| Box::new(move |v: &str| v.to_lowercase().contains(&a)) as Test),
        Op::Equals => return (!a.is_empty()).then(|| Box::new(move |v: &str| v.trim().to_lowercase() == a) as Test),
        Op::Is => {
            let exact = f.a.trim().to_string();
            return (!exact.is_empty()).then(|| Box::new(move |v: &str| v.trim() == exact) as Test);
        }
        Op::Starts => {
            return (!a.is_empty()).then(|| Box::new(move |v: &str| v.trim_start().to_lowercase().starts_with(&a)) as Test);
        }
        Op::Empty => return Some(Box::new(|v: &str| v.trim().is_empty())),
        Op::NotEmpty => return Some(Box::new(|v: &str| !v.trim().is_empty())),
        Op::True => return Some(Box::new(|v: &str| v.trim().to_lowercase() == "true")),
        Op::False => return Some(Box::new(|v: &str| v.trim().to_lowercase() == "false")),
        _ => {}
    }
    let (x, y) = (value(t, PLAIN, &f.a), value(t, PLAIN, &f.b));
    if x.is_nan() || (f.op == Op::Between && y.is_nan()) {
        return None;
    }
    let (lo, hi, op) = (x.min(y), x.max(y), f.op);
    Some(Box::new(move |v: &str| {
        let n = value(t, r, v);
        !n.is_nan()
            && match op {
                Op::Eq => n == x,
                Op::Ne => n != x,
                Op::Gt => n > x,
                Op::Ge => n >= x,
                Op::Lt => n < x,
                Op::Le => n <= x,
                _ => n >= lo && n <= hi,
            }
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Asc,
    Desc,
}

/// One sort level: a column and its direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SortKey {
    pub col: usize,
    pub dir: Dir,
}

/// A number or time cell whose value is 0, as the sort reads it: `0`, `0.00`, `-0`, `0:00:00`.
pub fn is_zero(t: Dtype, r: Reading, v: &str) -> bool {
    matches!(t, Dtype::Int | Dtype::Float | Dtype::Time) && sort_number(t, r, v) == Some(0.0)
}

/// A cell as a sort key for a dtype that is not string, or None when it is empty or does not read as `t`.
fn sort_number(t: Dtype, r: Reading, v: &str) -> Option<f64> {
    let s = v.trim();
    if s.is_empty() {
        return None;
    }
    let n = match t {
        Dtype::Bool => return is_bool(s).then(|| if s.eq_ignore_ascii_case("true") { 1.0 } else { 0.0 }),
        _ => value(t, r, s),
    };
    (!n.is_nan()).then_some(n)
}

/// A cell of a number, date, or time column as a number (dates and times in ms), or NaN.
fn value(t: Dtype, r: Reading, s: &str) -> f64 {
    match t {
        Dtype::Date | Dtype::DateTime => read_date(s, &r).map_or(f64::NAN, |n| n as f64),
        Dtype::Time => to_time(s).map_or(f64::NAN, |n| n as f64),
        _ => to_number(s, r.comma),
    }
}

fn trim_zeros(s: &[u8]) -> &[u8] {
    &s[s.iter().take_while(|&&c| c == b'0').count()..]
}

// Limit: no accent folding and no ICU punctuation order; add a collation crate if a file needs them.
/// Text as a key whose byte order is close to `Intl.Collator(undefined, { numeric: true,
/// sensitivity: "base" })` on lower-cased text: each digit run, without leading zeros, becomes a
/// length mark in '0'..='9' and then its digits, so a longer number sorts later. Runs of more
/// than 255 digits compare by their first digits.
fn natural_key(s: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < s.len() {
        if s[i].is_ascii_digit() {
            let e = digits(s, i);
            let run = trim_zeros(&s[i..e]);
            if run.len() < 9 {
                out.push(b'0' + run.len() as u8);
            } else {
                out.extend_from_slice(&[b'9', run.len().min(255) as u8]);
            }
            out.extend_from_slice(run);
            i = e;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
}

/// Ranks of rows 0..n from (key, row) pairs, sorted here: equal keys share a rank, and rows
/// with no pair rank LAST. Pairs keep the compares in memory that is close together.
fn ranks_of<K: Send>(n: usize, pairs: &mut [(K, u32)], cmp: impl Fn(&(K, u32), &(K, u32)) -> Cmp + Sync) -> Vec<u32> {
    pairs.par_sort_unstable_by(&cmp);
    let mut out = vec![LAST; n];
    let mut k = 0;
    for j in 0..pairs.len() {
        if j > 0 && cmp(&pairs[j - 1], &pairs[j]) != Cmp::Equal {
            k += 1;
        }
        out[pairs[j].1 as usize] = k;
    }
    out
}

/// Whether `hay` contains the lower-cased needle of `finder`, case ignored as `toLowerCase` does.
fn contains_ci(hay: &[u8], finder: &memmem::Finder<'_>, low: &mut Vec<u8>) -> bool {
    if hay.is_ascii() {
        low.clear();
        low.extend(hay.iter().map(u8::to_ascii_lowercase));
        finder.find(low).is_some()
    } else {
        finder.find(String::from_utf8_lossy(hay).to_lowercase().as_bytes()).is_some()
    }
}

/// One cell for `Scan::write`: in `q`, with each `q` inside doubled, when it holds `d`, `q`, or a line break.
fn put_cell(buf: &mut Vec<u8>, v: &[u8], d: &[u8], q: &[u8]) {
    let quote = !q.is_empty() && (memmem::find(v, d).is_some() || memmem::find(v, q).is_some() || memchr2(LF, CR, v).is_some());
    if !quote {
        return buf.extend_from_slice(v);
    }
    buf.extend_from_slice(q);
    let mut at = 0;
    for k in memmem::find_iter(v, q) {
        buf.extend_from_slice(&v[at..k]);
        buf.extend_from_slice(q);
        buf.extend_from_slice(q);
        at = k + q.len();
    }
    buf.extend_from_slice(&v[at..]);
    buf.extend_from_slice(q);
}

/// How a profile counts distinct values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Distinct {
    /// Every distinct value of the column is kept in memory while it runs.
    Exact,
    /// HyperLogLog (about 0.8% error, 16 KB). Top values come from a Misra-Gries summary of 1,024 counters.
    Approx,
}

/// Bars in a profile histogram.
pub const BINS: usize = 20;

/// The bar of a histogram over `span` that value `x` falls in: equal widths, the highest value
/// in the last bar.
fn bin(x: f64, (min, max): (f64, f64)) -> usize {
    let b = if max > min { ((x - min) / (max - min) * BINS as f64) as usize } else { 0 };
    b.min(BINS - 1)
}

/**
 * The lowest and highest value that bar `i` of a histogram over `span` holds, both included,
 * so "between" them keeps the rows the bar counts. Values step by `step` from the lowest one:
 * 1 for whole numbers, 1000 for dates (ms; they read to the second); 0 for any f64.
 * A bar that can hold no value gives a low above the high.
 */
pub fn bin_range(span: (f64, f64), i: usize, step: f64) -> (f64, f64) {
    // The least value in bar `i` or a later one. Bars only grow with the value: halve the gap.
    let first = |i: usize| {
        if step > 0.0 {
            let (mut a, mut b) = (0i64, ((span.1 - span.0) / step).ceil() as i64);
            while b - a > 1 {
                let m = (a + b) / 2;
                if bin(span.0 + m as f64 * step, span) >= i { b = m } else { a = m }
            }
            span.0 + b as f64 * step
        } else {
            let (mut lo, mut hi) = span;
            loop {
                let m = lo + (hi - lo) / 2.0;
                if m <= lo || m >= hi {
                    break hi;
                }
                if bin(m, span) >= i { hi = m } else { lo = m }
            }
        }
    };
    let lo = if i == 0 { span.0 } else { first(i) };
    let hi = match i + 1 >= BINS {
        true => span.1,
        false if step > 0.0 => first(i + 1) - step,
        false => first(i + 1).next_down(),
    };
    (lo, hi)
}
pub const TOP: usize = 10;
const MG_K: usize = 1024;
const HLL_P: u32 = 14;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NumStats {
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    pub median: f64,
    /// Sample standard deviation (n - 1).
    pub std: f64,
    pub sum: f64,
}

/// The profile of one column over a set of rows. Cells are read trimmed, as types read them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Profile {
    pub col: usize,
    pub rows: usize,
    pub empty: usize,
    /// Cells that are not empty and do not read as the column type.
    pub unreadable: usize,
    pub distinct: usize,
    /// `distinct` and the counts in `top` are estimates (Distinct::Approx).
    pub approx: bool,
    /// Number columns: over the cells that read.
    pub num: Option<NumStats>,
    /// Shortest, mean, and longest length in characters, over cells that are not empty.
    pub len: Option<(usize, f64, usize)>,
    /// The most common values, at most 10: by count, then by first appearance (Exact) or by
    /// text (Approx).
    pub top: Vec<(String, usize)>,
    /// `distinct`, `unique`, and `top` are in: `Scan::values` ran.
    pub counted: bool,
    /// Values that appear once. Exact only: None when Distinct is Approx.
    pub unique: Option<usize>,
    /// Text columns: the first and the last value in code-point order.
    pub range: Option<(String, String)>,
    /// Lowest and highest value of a number or date column (dates in ms), and a histogram
    /// of BINS bars between them.
    pub span: Option<(f64, f64)>,
    pub hist: Vec<usize>,
}

/// The value counts of a column: what `Scan::values` gives.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Values {
    pub distinct: usize,
    pub unique: Option<usize>,
    pub top: Vec<(String, usize)>,
    pub approx: bool,
}

impl Profile {
    /// Takes in the value counts of the same column and rows.
    pub fn count(&mut self, v: Values) {
        (self.distinct, self.unique, self.top, self.approx, self.counted) = (v.distinct, v.unique, v.top, v.approx, true);
    }
}

type Counts = HashMap<Box<[u8]>, usize>;

fn hash64(v: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new(); // fixed keys: the same every run
    v.hash(&mut h);
    h.finish()
}

/// HyperLogLog with 2^14 registers.
struct Hll(Vec<u8>);

impl Hll {
    fn new() -> Hll {
        Hll(vec![0; 1 << HLL_P])
    }

    fn add(&mut self, h: u64) {
        let i = (h >> (64 - HLL_P)) as usize;
        let rank = ((h << HLL_P) | (1 << (HLL_P - 1))).leading_zeros() as u8 + 1;
        self.0[i] = self.0[i].max(rank);
    }

    fn merge(&mut self, o: &Hll) {
        for (a, b) in self.0.iter_mut().zip(&o.0) {
            *a = (*a).max(*b);
        }
    }

    /// The estimate, with linear counting for small sets.
    fn count(&self) -> usize {
        let m = self.0.len() as f64;
        let sum: f64 = self.0.iter().map(|&r| 2f64.powi(-(r as i32))).sum();
        let e = 0.7213 / (1.0 + 1.079 / m) * m * m / sum;
        let zeros = self.0.iter().filter(|&&r| r == 0).count();
        if e <= 2.5 * m && zeros > 0 { (m * (m / zeros as f64).ln()).round() as usize } else { e.round() as usize }
    }
}

/// Misra-Gries: a value seen more than 1/1,025 of the time stays; its count can be low by that much.
fn mg_add(m: &mut Counts, v: &[u8]) {
    if let Some(c) = m.get_mut(v) {
        *c += 1;
    } else if m.len() < MG_K {
        m.insert(v.into(), 1);
    } else {
        m.retain(|_, c| {
            *c -= 1;
            *c > 0
        });
    }
}

fn mg_merge(a: &mut Counts, b: Counts) {
    for (k, c) in b {
        *a.entry(k).or_insert(0) += c;
    }
    if a.len() > MG_K {
        let mut counts: Vec<usize> = a.values().copied().collect();
        counts.select_nth_unstable_by(MG_K, |x, y| y.cmp(x));
        let cut = counts[MG_K];
        a.retain(|_, c| {
            *c = c.saturating_sub(cut);
            *c > 0
        });
    }
}

/// The 10 largest counts, by count and then by text.
fn top_values<'a>(counts: impl Iterator<Item = (&'a [u8], usize)>) -> Vec<(String, usize)> {
    let better = |a: &(&[u8], usize), b: &(&[u8], usize)| a.1 > b.1 || (a.1 == b.1 && a.0 < b.0);
    let mut top: Vec<(&[u8], usize)> = Vec::with_capacity(TOP + 1);
    for e in counts {
        if top.len() == TOP && !better(&e, &top[TOP - 1]) {
            continue;
        }
        let at = top.iter().position(|x| better(&e, x)).unwrap_or(top.len());
        top.insert(at, e);
        top.truncate(TOP);
    }
    top.into_iter().map(|(k, c)| (String::from_utf8_lossy(k).into_owned(), c)).collect()
}

/// What one part of the rows adds up to for `Scan::basic`. The numbers of a column go straight
/// into one buffer at their rows' places.
struct Part {
    rows: usize,
    empty: usize,
    unreadable: usize,
    len: (usize, usize, usize),
    /// Text columns: the least and the greatest value so far.
    range: Option<(Vec<u8>, Vec<u8>)>,
    /// Approx value counts, when asked for: a HyperLogLog and a Misra-Gries summary. Small, so
    /// they come in the same pass.
    approx: Option<(Hll, Counts)>,
}

impl Part {
    fn new(approx: bool) -> Part {
        Part { rows: 0, empty: 0, unreadable: 0, len: (usize::MAX, 0, 0), range: None, approx: approx.then(|| (Hll::new(), Counts::default())) }
    }

    /// Adds one cell. Returns its value when it reads as a number or a date (in ms).
    fn add(&mut self, v: &[u8], t: Dtype, r: Reading) -> Option<f64> {
        self.rows += 1;
        let s = String::from_utf8_lossy(v);
        let s = s.trim();
        if s.is_empty() {
            self.empty += 1;
            return None;
        }
        let chars = s.chars().count();
        self.len = (self.len.0.min(chars), self.len.1 + chars, self.len.2.max(chars));
        let read = match t {
            Dtype::Int | Dtype::Float | Dtype::Date | Dtype::DateTime | Dtype::Time => Some(value(t, r, s)).filter(|n| !n.is_nan()),
            Dtype::Bool => is_bool(s).then_some(f64::NAN),
            Dtype::String => Some(f64::NAN),
        };
        if read.is_none() {
            self.unreadable += 1;
        }
        if let Some((hll, mg)) = &mut self.approx {
            hll.add(hash64(s.as_bytes()));
            mg_add(mg, s.as_bytes());
        }
        if t == Dtype::String {
            let b = s.as_bytes();
            match &mut self.range {
                None => self.range = Some((b.to_vec(), b.to_vec())),
                Some((lo, hi)) => {
                    if b < &lo[..] {
                        *lo = b.to_vec();
                    } else if b > &hi[..] {
                        *hi = b.to_vec();
                    }
                }
            }
        }
        read.filter(|x| !x.is_nan())
    }
}

/// A date in ms as "2024-02-05", with " 14:05:09" when it has a time of day.
pub fn date_label(ms: i64) -> String {
    let date = date_text(ms, DatePat::Iso);
    if ms.rem_euclid(86_400_000) == 0 { date } else { format!("{date} {}", time_text(ms, TimePat::Hms)) }
}

/// A time in ms as "1:02:03", with ".045" when it has milliseconds. `to_time` reads it back.
pub fn time_label(ms: i64) -> String {
    let (s, f) = (ms.div_euclid(1000), ms.rem_euclid(1000));
    let hms = format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60);
    if f == 0 { hms } else { format!("{hms}.{f:03}") }
}

/// A JSON string: in quotes, with `"`, `\`, and control characters escaped.
pub fn json_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The Profile of columns `cols` as one JSON object, for the command line: the first `limit`
/// rows (None: all), one column at a time. A key that a type does not have is left out. Dates
/// and times are text, as the app shows them; other numbers are JSON numbers (NaN as null).
pub fn profile_json(doc: &Doc, cols: &[usize], limit: Option<usize>, distinct: Distinct) -> String {
    let Some(scan) = doc.scan() else { return "{}".into() };
    let stop = AtomicBool::new(false);
    let num = |x: f64| if x.is_finite() { format!("{x}") } else { "null".into() };
    let text = |s: String| json_text(&s);
    let file = doc.path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
    let total = scan.rows();
    let mut out = format!(
        "{{\"file\": {}, \"rows\": {}, \"total_rows\": {total}, \"distinct\": \"{}\", \"columns\": [",
        json_text(&file),
        total.min(limit.unwrap_or(usize::MAX)),
        if distinct == Distinct::Exact { "exact" } else { "approx" }
    );
    for (k, &c) in cols.iter().enumerate() {
        let p = scan.profile(None, limit, c, distinct, &stop);
        let ty = doc.dtypes.get(c).copied().unwrap_or(Dtype::String);
        let mut f: Vec<(&str, String)> = vec![("name", json_text(&doc.names[c])), ("type", json_text(ty.name())), ("count", p.rows.to_string()), ("empty", p.empty.to_string())];
        if ty != Dtype::String {
            f.push(("not_readable", p.unreadable.to_string()));
        }
        f.extend([("distinct", p.distinct.to_string()), ("approx", p.approx.to_string())]);
        if let Some(u) = p.unique {
            f.push(("unique", u.to_string()));
        }
        let top: Vec<String> = p.top.iter().map(|(v, n)| format!("{{\"value\": {}, \"count\": {n}}}", json_text(v))).collect();
        f.push(("top", format!("[{}]", top.join(", "))));
        match (ty, p.num) {
            (Dtype::String, _) => {
                if let Some((lo, hi)) = &p.range {
                    f.extend([("first", json_text(lo)), ("last", json_text(hi))]);
                }
                if let Some((a, m, b)) = p.len {
                    f.extend([("shortest", a.to_string()), ("longest", b.to_string()), ("mean_length", num(m))]);
                }
            }
            (Dtype::Int | Dtype::Float, Some(s)) => f.extend([("min", num(s.min)), ("max", num(s.max)), ("mean", num(s.mean)), ("median", num(s.median)), ("std", num(s.std)), ("sum", num(s.sum))]),
            (Dtype::Time, Some(s)) => {
                let at = |x: f64| text(time_label(x.round() as i64));
                f.extend([("min", at(s.min)), ("max", at(s.max)), ("mean", at(s.mean)), ("median", at(s.median)), ("std", at(s.std)), ("sum", at(s.sum))]);
            }
            (Dtype::Date | Dtype::DateTime, _) => {
                if let Some((lo, hi)) = p.span {
                    f.extend([("earliest", text(date_label(lo as i64))), ("latest", text(date_label(hi as i64))), ("span_seconds", num((hi - lo) / 1000.0))]);
                }
            }
            (Dtype::Bool, _) => {
                let n = |w: &str| p.top.iter().filter(|(v, _)| v.trim().eq_ignore_ascii_case(w)).map(|v| v.1).sum::<usize>();
                f.extend([("true", n("true").to_string()), ("false", n("false").to_string())]);
            }
            _ => {}
        }
        if !p.hist.is_empty() {
            f.push(("histogram", format!("[{}]", p.hist.iter().map(usize::to_string).collect::<Vec<_>>().join(", "))));
        }
        let body: Vec<String> = f.into_iter().map(|(key, v)| format!("\"{key}\": {v}")).collect();
        out += &format!("{}\n  {{{}}}", if k == 0 { "" } else { "," }, body.join(", "));
    }
    out + "\n]}"
}

impl Scan {
    /// Cell `c` of row `i` of the rows `rows` (None: every data row), as it is in the file.
    fn cell_of(&self, rows: Option<&[u32]>, i: usize, c: usize, scratch: &mut Vec<u8>, cell: &mut Vec<u8>) {
        cell.clear();
        let s = self.body()[rows.map_or(i, |v| v[i] as usize)] as usize;
        each_field(&self.data, s, &self.d, &self.q, scratch, &mut |k, x| {
            if k == c {
                cell.extend_from_slice(x);
            }
            k < c
        });
    }

    /// The profile of column `c`: `basic`, with the value counts.
    pub fn profile(&self, rows: Option<&[u32]>, limit: Option<usize>, c: usize, distinct: Distinct, stop: &AtomicBool) -> Profile {
        let mut p = self.basic(rows, limit, c, distinct, stop);
        if !p.counted {
            p.count(self.values(rows, limit, c, distinct, stop));
        }
        p
    }

    /**
     * The counts, lengths, and number statistics of column `c` over the data rows `rows` (None:
     * every data row), the first `limit` of them when given. The value counts too when
     * `distinct` is Approx (they are small); Exact counts come from `values`. Parts of the rows
     * run in parallel, then merge. A number column holds one f64 a row while it runs, for the
     * median. When `stop` is set, it returns early with part of the rows.
     */
    pub fn basic(&self, rows: Option<&[u32]>, limit: Option<usize>, c: usize, distinct: Distinct, stop: &AtomicBool) -> Profile {
        let (t, r) = (self.dtype(c), self.read(c));
        let n = rows.map_or(self.rows(), <[u32]>::len).min(limit.unwrap_or(usize::MAX));
        // Rows a..a + CHUNK; a number or date goes to `slot`, at the row's place.
        let approx = distinct == Distinct::Approx;
        let work = |a: usize, slot: &mut [f64]| {
            let mut part = Part::new(approx);
            if stop.load(Ordering::Relaxed) {
                return part;
            }
            let (mut scratch, mut cell) = (Vec::new(), Vec::new());
            for i in a..(a + CHUNK).min(n) {
                self.cell_of(rows, i, c, &mut scratch, &mut cell);
                if let Some(x) = part.add(&cell, t, r) {
                    slot[i - a] = x;
                }
            }
            part
        };
        let numeric = matches!(t, Dtype::Int | Dtype::Float | Dtype::Date | Dtype::DateTime | Dtype::Time);
        let mut nums = if numeric { vec![f64::NAN; n] } else { Vec::new() };
        let parts: Vec<Part> = if numeric {
            nums.par_chunks_mut(CHUNK).enumerate().map(|(k, slot)| work(k * CHUNK, slot)).collect()
        } else {
            let firsts: Vec<usize> = (0..n).step_by(CHUNK).collect();
            firsts.par_iter().map(|&a| work(a, &mut [])).collect()
        };
        nums.retain(|x| !x.is_nan());
        let mut out = Profile { col: c, ..Default::default() };
        let mut len = (usize::MAX, 0, 0);
        for p in &parts {
            (out.rows, out.empty, out.unreadable) = (out.rows + p.rows, out.empty + p.empty, out.unreadable + p.unreadable);
            len = (len.0.min(p.len.0), len.1 + p.len.1, len.2.max(p.len.2));
        }
        let full = out.rows - out.empty;
        if full > 0 {
            out.len = Some((len.0, len.1 as f64 / full as f64, len.2));
        }
        let ends = parts.iter().filter_map(|p| p.range.as_ref());
        if let (Some(lo), Some(hi)) = (ends.clone().map(|r| &r.0).min(), ends.map(|r| &r.1).max()) {
            out.range = Some((String::from_utf8_lossy(lo).into_owned(), String::from_utf8_lossy(hi).into_owned()));
        }
        if approx {
            let (mut hll, mut mg) = (Hll::new(), Counts::default());
            for (h, m) in parts.into_iter().filter_map(|p| p.approx) {
                hll.merge(&h);
                mg_merge(&mut mg, m);
            }
            out.count(Values { distinct: hll.count(), unique: None, top: top_values(mg.iter().map(|(k, &c)| (&k[..], c))), approx: true });
        }
        let v = &mut nums;
        if !v.is_empty() {
            let (min, max) = v.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &x| (a.min(x), b.max(x)));
            out.span = Some((min, max));
            out.hist = vec![0; BINS];
            for &x in v.iter() {
                out.hist[bin(x, (min, max))] += 1;
            }
            if matches!(t, Dtype::Int | Dtype::Float | Dtype::Time) {
                let (len, sum) = (v.len() as f64, v.iter().sum::<f64>());
                let mean = sum / len;
                let var = if v.len() > 1 { v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (len - 1.0) } else { 0.0 };
                let m = v.len() / 2;
                let hi = *v.select_nth_unstable_by(m, |a, b| a.total_cmp(b)).1;
                let median = if v.len() % 2 == 1 { hi } else { (v[..m].iter().copied().fold(f64::NEG_INFINITY, f64::max) + hi) / 2.0 };
                out.num = Some(NumStats { min, max, mean, median, std: var.sqrt(), sum });
            }
        }
        out
    }

    /**
     * The value counts of column `c` over the same rows as `basic`: distinct values, values seen
     * once, and the 10 most common. Cells are read trimmed; empty cells do not count.
     * Exact: a 64-bit hash and the row of every cell (16 bytes a row), sorted in place; equal
     * hashes are one value. Two values share a hash about once in 10^12 pairs: for 6.5 million
     * values, about 1 chance in a million that two of them count as one.
     * Approx: a HyperLogLog and a Misra-Gries summary for each part of the rows (`basic` makes
     * them in its own pass).
     */
    pub fn values(&self, rows: Option<&[u32]>, limit: Option<usize>, c: usize, distinct: Distinct, stop: &AtomicBool) -> Values {
        let n = rows.map_or(self.rows(), <[u32]>::len).min(limit.unwrap_or(usize::MAX));
        if distinct == Distinct::Approx {
            let firsts: Vec<usize> = (0..n).step_by(CHUNK).collect();
            let parts: Vec<(Hll, Counts)> = firsts
                .par_iter()
                .map(|&a| {
                    let (mut hll, mut mg) = (Hll::new(), Counts::default());
                    if stop.load(Ordering::Relaxed) {
                        return (hll, mg);
                    }
                    let (mut scratch, mut cell) = (Vec::new(), Vec::new());
                    for i in a..(a + CHUNK).min(n) {
                        self.cell_of(rows, i, c, &mut scratch, &mut cell);
                        let s = String::from_utf8_lossy(&cell);
                        let s = s.trim();
                        if !s.is_empty() {
                            hll.add(hash64(s.as_bytes()));
                            mg_add(&mut mg, s.as_bytes());
                        }
                    }
                    (hll, mg)
                })
                .collect();
            let (mut hll, mut mg) = (Hll::new(), Counts::default());
            for (h, m) in parts {
                hll.merge(&h);
                mg_merge(&mut mg, m);
            }
            return Values { distinct: hll.count(), unique: None, top: top_values(mg.iter().map(|(k, &c)| (&k[..], c))), approx: true };
        }
        // (hash, row); an empty cell keeps row u32::MAX and is dropped.
        let mut pairs = vec![(0u64, u32::MAX); n];
        pairs.par_chunks_mut(CHUNK).enumerate().for_each(|(k, slot)| {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let (mut scratch, mut cell) = (Vec::new(), Vec::new());
            for (j, p) in slot.iter_mut().enumerate() {
                let i = k * CHUNK + j;
                self.cell_of(rows, i, c, &mut scratch, &mut cell);
                let s = String::from_utf8_lossy(&cell);
                let s = s.trim();
                if !s.is_empty() {
                    *p = (hash64(s.as_bytes()), i as u32);
                }
            }
        });
        pairs.retain(|p| p.1 != u32::MAX);
        pairs.par_sort_unstable_by_key(|p| p.0);
        let (mut out, mut best) = (Values::default(), Vec::<(usize, u32)>::with_capacity(TOP + 1));
        out.unique = Some(0);
        for run in pairs.chunk_by(|a, b| a.0 == b.0) {
            let (count, first) = (run.len(), run.iter().map(|p| p.1).min().unwrap_or(0));
            out.distinct += 1;
            if count == 1 {
                *out.unique.as_mut().unwrap() += 1;
            }
            // The 10 best by count, then by first appearance.
            if best.len() < TOP || (count, std::cmp::Reverse(first)) > (best[TOP - 1].0, std::cmp::Reverse(best[TOP - 1].1)) {
                let at = best.iter().position(|b| (count, std::cmp::Reverse(first)) > (b.0, std::cmp::Reverse(b.1))).unwrap_or(best.len());
                best.insert(at, (count, first));
                best.truncate(TOP);
            }
        }
        let (mut scratch, mut cell) = (Vec::new(), Vec::new());
        out.top = best
            .into_iter()
            .map(|(count, i)| {
                self.cell_of(rows, i as usize, c, &mut scratch, &mut cell);
                (String::from_utf8_lossy(&cell).trim().to_string(), count)
            })
            .collect();
        out
    }
}

/// The parts in order, in one Vec of exact capacity. A collect of a flatten grows by doubling.
fn concat<T>(parts: Vec<Vec<T>>) -> Vec<T> {
    let mut out = Vec::with_capacity(parts.iter().map(Vec::len).sum());
    for p in parts {
        out.extend(p);
    }
    out
}

/// A done index and the column types: what search, filters, and sort read. Cheap to clone into a thread.
#[derive(Clone)]
pub struct Scan {
    data: Arc<Bytes>,
    starts: Arc<Vec<u64>>,
    header: usize,
    d: Vec<u8>,
    q: Vec<u8>,
    dtypes: Vec<Dtype>,
    reads: Vec<Reading>,
    ranks: Ranks,
}

impl Scan {
    /// Data rows.
    pub fn rows(&self) -> usize {
        self.starts.len().saturating_sub(self.header)
    }

    fn body(&self) -> &[u64] {
        &self.starts[self.header.min(self.starts.len())..]
    }

    pub fn dtype(&self, c: usize) -> Dtype {
        self.dtypes.get(c).copied().unwrap_or(Dtype::String)
    }

    pub fn read(&self, c: usize) -> Reading {
        self.reads.get(c).copied().unwrap_or(PLAIN)
    }

    /**
     * Data rows where any `scope` column (empty = all columns, joined by a space) contains
     * `search`, case ignored, and every complete filter passes. In file order. When `stop`
     * is set, it returns early with part of the rows.
     */
    pub fn query(&self, search: &str, scope: &[usize], filters: &[Filter], stop: &AtomicBool) -> Vec<u32> {
        let needle = search.to_lowercase();
        // "and" binds first, as in SQL: each "or" starts a group, and a row matches when every
        // filter of one group matches. A filter with no value yet is left out, and so is a
        // group it leaves empty.
        let mut groups: Vec<Vec<(usize, Test)>> = Vec::new();
        for (i, f) in filters.iter().enumerate() {
            if i == 0 || f.or {
                groups.push(Vec::new());
            }
            if let (Some(t), Some(g)) = (predicate(f, self.dtype(f.col), self.read(f.col)), groups.last_mut()) {
                g.push((f.col, t));
            }
        }
        groups.retain(|g| !g.is_empty());
        let tests: Vec<&(usize, Test)> = groups.iter().flatten().collect();
        if needle.is_empty() && tests.is_empty() {
            return (0..self.rows() as u32).collect();
        }
        let all = !needle.is_empty() && scope.is_empty();
        let cols: &[usize] = if needle.is_empty() { &[] } else { scope };
        let width = cols.iter().chain(tests.iter().map(|t| &t.0)).max().map_or(0, |m| m + 1);
        let mut need = vec![false; width];
        for &c in cols.iter().chain(tests.iter().map(|t| &t.0)) {
            need[c] = true;
        }
        let (d, q) = (&self.d[..], &self.q[..]);
        let parts: Vec<Vec<u32>> = self
            .body()
            .par_chunks(CHUNK)
            .enumerate()
            .map(|(k, chunk)| {
                let mut out = Vec::new();
                if stop.load(Ordering::Relaxed) {
                    return out;
                }
                let finder = memmem::Finder::new(needle.as_bytes());
                let (mut scratch, mut join, mut low) = (Vec::new(), Vec::new(), Vec::new());
                let mut cells = vec![Vec::new(); width];
                for (j, &s) in chunk.iter().enumerate() {
                    join.clear();
                    cells.iter_mut().for_each(Vec::clear);
                    each_field(&self.data, s as usize, d, q, &mut scratch, &mut |c, f| {
                        if all {
                            if c > 0 {
                                join.push(b' ');
                            }
                            join.extend_from_slice(f);
                        }
                        if c < width && need[c] {
                            cells[c].extend_from_slice(f);
                        }
                        all || c + 1 < width
                    });
                    let found = needle.is_empty() || if all { contains_ci(&join, &finder, &mut low) } else { cols.iter().any(|&c| contains_ci(&cells[c], &finder, &mut low)) };
                    let pass = |g: &Vec<(usize, Test)>| g.iter().all(|(c, t)| t(&String::from_utf8_lossy(&cells[*c])));
                    if found && (groups.is_empty() || groups.iter().any(pass)) {
                        out.push((k * CHUNK + j) as u32);
                    }
                }
                out
            })
            .collect();
        concat(parts)
    }

    /**
     * The rows `view` (None: every data row, in file order) and the columns `cols` as delimited
     * text: `d` between cells, `eol` after each row, and `q` around a cell that holds `d`, `q`, or
     * a line break, with each `q` inside doubled. The header record comes first when the file has
     * one. Cells keep their bytes. Rows are formatted in parallel, about 1 M rows at a time.
     */
    pub fn write(&self, out: &mut dyn std::io::Write, view: Option<&[u32]>, cols: &[usize], d: &str, eol: &str, q: &str) -> std::io::Result<()> {
        let need = cols.iter().max().map_or(0, |m| m + 1);
        let (sd, sq) = (&self.d[..], &self.q[..]);
        let (d, eol, q) = (d.as_bytes(), eol.as_bytes(), q.as_bytes());
        let put = |s: u64, buf: &mut Vec<u8>, scratch: &mut Vec<u8>, cells: &mut Vec<Vec<u8>>| {
            cells.iter_mut().for_each(Vec::clear);
            each_field(&self.data, s as usize, sd, sq, scratch, &mut |c, f| {
                if c < need {
                    cells[c].extend_from_slice(f);
                }
                c + 1 < need
            });
            for (k, &c) in cols.iter().enumerate() {
                if k > 0 {
                    buf.extend_from_slice(d);
                }
                put_cell(buf, &cells[c], d, q);
            }
            buf.extend_from_slice(eol);
        };
        if self.header == 1 && !self.starts.is_empty() {
            let mut buf = Vec::new();
            put(self.starts[0], &mut buf, &mut Vec::new(), &mut vec![Vec::new(); need]);
            out.write_all(&buf)?;
        }
        let body = self.body();
        let n = view.map_or(body.len(), <[u32]>::len);
        let at = |i: usize| body[view.map_or(i, |v| v[i] as usize)];
        for lo in (0..n).step_by(CHUNK * 64) {
            let hi = (lo + CHUNK * 64).min(n);
            let firsts: Vec<usize> = (lo..hi).step_by(CHUNK).collect();
            let parts: Vec<Vec<u8>> = firsts
                .par_iter()
                .map(|&a| {
                    let (mut buf, mut scratch, mut cells) = (Vec::new(), Vec::new(), vec![Vec::new(); need]);
                    for i in a..(a + CHUNK).min(hi) {
                        put(at(i), &mut buf, &mut scratch, &mut cells);
                    }
                    buf
                })
                .collect();
            for p in parts {
                out.write_all(&p)?;
            }
        }
        Ok(())
    }

    /// `f` of column `c` in every data row, in row order. A missing cell is "".
    fn map_column<T: Send>(&self, c: usize, f: impl Fn(&str) -> T + Sync) -> Vec<T> {
        let (d, q) = (&self.d[..], &self.q[..]);
        let parts: Vec<Vec<T>> = self
            .body()
            .par_chunks(CHUNK)
            .map(|chunk| {
                let mut scratch = Vec::new();
                let mut cell = |s: u64| {
                    let mut v = None;
                    each_field(&self.data, s as usize, d, q, &mut scratch, &mut |k, x| {
                        if k == c {
                            v = Some(f(&String::from_utf8_lossy(x)));
                        }
                        k < c
                    });
                    v.unwrap_or_else(|| f(""))
                };
                chunk.iter().map(|&s| cell(s)).collect()
            })
            .collect();
        concat(parts)
    }

    /// Data row `i`'s natural text key in column `c` (trimmed, lower-cased), added to `out`.
    /// False for an empty cell.
    fn cell_key(&self, i: usize, c: usize, scratch: &mut Vec<u8>, low: &mut Vec<u8>, out: &mut Vec<u8>) -> bool {
        let mut found = false;
        each_field(&self.data, self.body()[i] as usize, &self.d, &self.q, scratch, &mut |k, x| {
            if k == c {
                let v = String::from_utf8_lossy(x);
                let v = v.trim();
                if !v.is_empty() {
                    found = true;
                    if v.is_ascii() {
                        low.clear();
                        low.extend(v.bytes().map(|b| b.to_ascii_lowercase()));
                        natural_key(low, out);
                    } else {
                        natural_key(v.to_lowercase().as_bytes(), out);
                    }
                }
            }
            k < c
        });
        found
    }

    /// The sort rank of every data row in column `c`, cached. Text compares with digit runs as
    /// numbers, case ignored. Equal values share a rank. Empty and unreadable cells rank LAST.
    fn rank(&self, c: usize) -> Arc<Vec<u32>> {
        if let Some(r) = self.ranks.lock().unwrap().get(&c) {
            return r.clone();
        }
        let (t, r, n) = (self.dtype(c), self.read(c), self.rows());
        let out = if t == Dtype::String {
            // (15-byte key prefix, length mark, row): the mark is the key's length, or 16 when it
            // is longer than 15 bytes. With the zero padding, (prefix, mark) orders as the keys
            // do, except two keys longer than 15 bytes with one prefix. Only those runs make
            // their keys again from the file, so no key is held for every row.
            let mut pairs: Vec<([u8; 16], u32)> = vec![([0; 16], u32::MAX); n];
            pairs.par_chunks_mut(CHUNK).enumerate().for_each(|(ch, part)| {
                let (mut scratch, mut low, mut key) = (Vec::new(), Vec::new(), Vec::new());
                for (j, slot) in part.iter_mut().enumerate() {
                    let i = ch * CHUNK + j;
                    key.clear();
                    if self.cell_key(i, c, &mut scratch, &mut low, &mut key) {
                        let m = key.len().min(15);
                        slot.0[..m].copy_from_slice(&key[..m]);
                        slot.0[15] = key.len().min(16) as u8;
                        slot.1 = i as u32;
                    }
                }
            });
            pairs.retain(|p| p.1 != u32::MAX); // the empty cells: they rank LAST
            pairs.par_sort_unstable_by(|a, b| a.0.cmp(&b.0));
            let mut runs = Vec::new();
            let mut a = 0;
            while a < pairs.len() {
                let b = a + pairs[a..].iter().take_while(|p| p.0 == pairs[a].0).count();
                if b - a > 1 && pairs[a].0[15] == 16 {
                    runs.push((a, b));
                }
                a = b;
            }
            // Each run in key order, and where a new key starts in it.
            let resolved: Vec<(Vec<u32>, Vec<bool>)> = runs
                .par_iter()
                .map(|&(a, b)| {
                    let (mut scratch, mut low, mut arena, mut spans) = (Vec::new(), Vec::new(), Vec::new(), Vec::with_capacity(b - a));
                    for p in &pairs[a..b] {
                        let at = arena.len();
                        self.cell_key(p.1 as usize, c, &mut scratch, &mut low, &mut arena);
                        spans.push((at, arena.len()));
                    }
                    let key = |m: usize| &arena[spans[m].0..spans[m].1];
                    let mut order: Vec<usize> = (0..b - a).collect();
                    order.sort_by(|&x, &y| key(x).cmp(key(y)));
                    let starts = (0..order.len()).map(|k| k == 0 || key(order[k - 1]) != key(order[k])).collect();
                    (order.iter().map(|&m| pairs[a + m].1).collect(), starts)
                })
                .collect();
            for (&(a, _), (rows, _)) in runs.iter().zip(&resolved) {
                for (m, &row) in rows.iter().enumerate() {
                    pairs[a + m].1 = row;
                }
            }
            let mut out = vec![LAST; n];
            let (mut k, mut run) = (0, 0);
            for j in 0..pairs.len() {
                let same = match runs.get(run) {
                    Some(&(a, b)) if (a..b).contains(&j) => {
                        let same = !resolved[run].1[j - a];
                        if j + 1 == b {
                            run += 1;
                        }
                        same
                    }
                    _ => j > 0 && pairs[j - 1].0 == pairs[j].0,
                };
                if j > 0 && !same {
                    k += 1;
                }
                out[pairs[j].1 as usize] = k;
            }
            out
        } else {
            let keys = self.map_column(c, |v| sort_number(t, r, v).unwrap_or(f64::NAN));
            let mut pairs: Vec<(f64, u32)> = Vec::with_capacity(keys.iter().filter(|x| !x.is_nan()).count());
            pairs.extend(keys.iter().enumerate().filter(|p| !p.1.is_nan()).map(|(i, &x)| (x, i as u32)));
            drop(keys);
            ranks_of(n, &mut pairs, |a, b| a.0.partial_cmp(&b.0).unwrap_or(Cmp::Equal))
        };
        let out = Arc::new(out);
        self.ranks.lock().unwrap().insert(c, out.clone());
        out
    }

    /**
     * `hits` sorted by the levels in `keys`, as a new list. Level 2 orders only rows equal in
     * level 1, and so on. Rows equal in all levels keep their order. In each level, empty and
     * unreadable cells stay last in both directions.
     */
    pub fn sort(&self, hits: &[u32], keys: &[SortKey]) -> Vec<u32> {
        let levels: Vec<(Arc<Vec<u32>>, bool)> = keys.iter().map(|k| (self.rank(k.col), k.dir == Dir::Asc)).collect();
        let mut out = hits.to_vec();
        if !levels.is_empty() {
            out.par_sort_by(|&a, &b| {
                for (r, asc) in &levels {
                    let (x, y) = (r[a as usize], r[b as usize]);
                    let o = if *asc || x == LAST || y == LAST { x.cmp(&y) } else { y.cmp(&x) };
                    if o != Cmp::Equal {
                        return o;
                    }
                }
                Cmp::Equal
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const F: Format = Format { comma: None, order: None };

    fn rows(v: &[&[&str]]) -> Vec<Vec<String>> {
        v.iter().map(|r| r.iter().map(|s| s.to_string()).collect()).collect()
    }
    fn infer(v: &[&str], f: &Format) -> Dtype {
        infer_column(v.iter().copied(), f).0
    }
    fn n(k: usize, v: &str) -> Vec<&str> {
        vec![v; k]
    }

    // Parser
    #[test]
    fn parse_plain_rows() {
        assert_eq!(parse("a,b\n1,2", ",", "\""), rows(&[&["a", "b"], &["1", "2"]]));
    }
    #[test]
    fn parse_quoted_field_keeps_the_delimiter() {
        assert_eq!(parse("\"a,b\",c", ",", "\""), rows(&[&["a,b", "c"]]));
    }
    #[test]
    fn parse_doubled_quotes_unescape() {
        assert_eq!(parse("\"say \"\"hi\"\"\",x", ",", "\""), rows(&[&["say \"hi\"", "x"]]));
    }
    #[test]
    fn parse_crlf_line_endings() {
        assert_eq!(parse("a,b\r\n1,2\r\n", ",", "\""), rows(&[&["a", "b"], &["1", "2"]]));
    }
    #[test]
    fn parse_line_break_inside_quotes() {
        assert_eq!(parse("\"line1\nline2\",x\ny,z", ",", "\""), rows(&[&["line1\nline2", "x"], &["y", "z"]]));
        assert_eq!(parse("h\r\n\"a\r\nb\",1\n\nd,2\r\"e\re\",3", ",", "\""), rows(&[&["h"], &["a\r\nb", "1"], &["d", "2"], &["e\re", "3"]]));
    }
    #[test]
    fn parse_bom_stripped() {
        assert_eq!(parse("\u{FEFF}a,b\n1,2", ",", "\"")[0][0], "a");
    }
    #[test]
    fn parse_ragged_rows_blank_lines_trailing_delimiter() {
        assert_eq!(parse("a,b,c\n1\n\n2,3\n", ",", "\""), rows(&[&["a", "b", "c"], &["1"], &["2", "3"]]));
        assert_eq!(parse("a,", ",", "\""), rows(&[&["a", ""]]));
        assert_eq!(parse("a,\nb,c", ",", "\""), rows(&[&["a", ""], &["b", "c"]]));
    }

    // Delimiter detection
    #[test]
    fn delimiter_extension_wins_when_it_splits_consistently() {
        let text = b"a,x\tb\n1,y\t2";
        assert_eq!(detect_delimiter(text, "tsv", "\""), "\t");
        assert_eq!(detect_delimiter(text, "", "\""), ",");
    }
    #[test]
    fn delimiter_semicolon_csv_falls_back_to_consistency() {
        assert_eq!(detect_delimiter(b"a;b;c\n1,5;2;3\n4;5,5;6", "csv", "\""), ";");
    }
    #[test]
    fn delimiter_pipe_with_no_extension() {
        assert_eq!(detect_delimiter(b"a|b|c\n1|2|3", "", "\""), "|");
    }
    #[test]
    fn delimiter_commas_inside_quotes_do_not_count() {
        assert_eq!(detect_delimiter(b"\"a,b,c\";d\n\"e,f,g\";h\n\"i,j,k\";l", "csv", "\""), ";");
    }

    // Header detection
    #[test]
    fn header_text_row_over_a_numeric_body() {
        let (h, reason) = detect_header(&parse("name,price\nfoo,1.5\nbar,2\n", ",", "\""), &F);
        assert!(h);
        assert!(reason.contains("row 1 is text, rows 2+ are float"), "{reason}");
    }
    #[test]
    fn header_numeric_row_1_like_the_body_is_data() {
        assert!(!detect_header(&parse("1,2\n3,4\n5,6", ",", "\""), &F).0);
    }
    #[test]
    fn header_all_text_unique_labels_on_repeated_value_off() {
        assert!(detect_header(&parse("first,last\nAnn,Lee\nBob,Ray", ",", "\""), &F).0);
        assert!(!detect_header(&parse("Ann,Lee\nBob,Lee\nCid,Lee", ",", "\""), &F).0);
    }

    // Dtype inference
    #[test]
    fn dtype_int() {
        assert_eq!(infer(&["1", "-2", "+30", " 4 "], &F), Dtype::Int);
    }
    #[test]
    fn dtype_float_including_mixed_ints() {
        assert_eq!(infer(&["1", "2.5", "-3e2", ".5"], &F), Dtype::Float);
    }
    #[test]
    fn dtype_bool_and_date() {
        assert_eq!(infer(&["true", "False", "TRUE"], &F), Dtype::Bool);
        assert_eq!(infer(&["2024-01-05", "2024/2/9", "2024-03-01T10:00:00Z"], &F), Dtype::DateTime);
    }
    #[test]
    fn date_with_a_space_before_the_zone() {
        let v = "2026-01-12 04:40:00.587 +0700";
        assert!(to_date(v, Order::Ymd).is_some());
        assert_eq!(to_date(v, Order::Ymd), to_date("2026-01-12 04:40:00", Order::Ymd));
        assert_eq!(infer(&[v, "2026-01-12 20:10:57.105 -0130", "2026-01-13 09:45:01 Z"], &F), Dtype::DateTime);
        assert_eq!(to_date("2026-01-12 04:40  +0700", Order::Ymd), None);
        assert_eq!(to_date("2026-01-12 04:40 x", Order::Ymd), None);
    }
    #[test]
    fn date_with_an_hour_only_zone() {
        let v = "2023-05-05 09:05:48+00";
        assert_eq!(to_date(v, Order::Ymd), to_date("2023-05-05 09:05:48", Order::Ymd));
        assert!(to_date(v, Order::Ymd).is_some());
        assert_eq!(infer(&[v, "2023-05-06 05:52:37-03", "2023-05-07 12:04:33 +05"], &F), Dtype::DateTime);
        for bad in ["2023-05-05 09:05:48+0", "2023-05-05 09:05:48+000", "2023-05-05 09:05:48+15", "2023-05-05 09:05:48+05:60"] {
            assert_eq!(to_date(bad, Order::Ymd), None, "{bad}");
        }
    }
    #[test]
    fn date_with_a_month_name() {
        let day = to_date("2024-02-05", Order::Ymd);
        for v in ["5 Feb 2024", "05 feb 2024", "Feb 5, 2024", "Feb 5 2024", "February 5, 2024", "5 FEBRUARY 2024"] {
            assert_eq!(to_date(v, Order::Ymd), day, "{v}");
            assert_eq!(to_date(v, Order::Dmy), day, "{v}");
        }
        assert_eq!(to_date("Feb 5, 2024 2:05 PM", Order::Mdy), to_date("2024-02-05 14:05", Order::Ymd));
        assert_eq!(to_date("5 Feb 2024 14:05:09", Order::Ymd), to_date("2024-02-05 14:05:09", Order::Ymd));
        for bad in ["5 Fbr 2024", "Feb 32, 2024", "Feb 5, 24", "Feb 5,2024", "123 Feb 2024", "Feb", "5 Feb"] {
            assert_eq!(to_date(bad, Order::Ymd), None, "{bad}");
        }
        assert_eq!(infer(&["5 Feb 2024", "Feb 6, 2024"], &F), Dtype::Date);
        assert_eq!(infer(&["5 Feb 2024 2:05 PM", "6 Feb 2024 9:00 AM"], &F), Dtype::DateTime);
    }
    #[test]
    fn compact_iso_date() {
        assert_eq!(to_date("20230505T090548Z", Order::Ymd), to_date("2023-05-05 09:05:48", Order::Ymd));
        assert_eq!(to_date("20230505T0905", Order::Ymd), to_date("2023-05-05 09:05", Order::Ymd));
        assert_eq!(to_date("20230505t090548.25+0700", Order::Ymd), to_date("2023-05-05 09:05:48", Order::Ymd));
        for bad in ["20230505T09054", "20230505T", "20230505 090548", "20231305T0905", "20230505T0960"] {
            assert_eq!(to_date(bad, Order::Ymd), None, "{bad}");
        }
        assert_eq!(to_date("20230505T090548Z", Order::Dmy), None);
        // No ':' in it, and still a date & time.
        assert_eq!(infer(&["20230505T090548Z", "20230506T101500Z"], &F), Dtype::DateTime);
        assert_eq!(infer(&["20230505", "20230506"], &F), Dtype::Int);
    }
    #[test]
    fn twelve_hour_time() {
        assert_eq!(to_time("2:05 PM"), to_time("14:05"));
        assert_eq!(to_time("2:05:09pm"), to_time("14:05:09"));
        assert_eq!(to_time("12:00 AM"), to_time("0:00"));
        assert_eq!(to_time("12:30 pm"), to_time("12:30"));
        assert_eq!(to_time("11:59:59.5 AM"), to_time("11:59:59.5"));
        for bad in ["13:05 PM", "0:05 AM", "100:00 PM", "2:05 XM", "2:05  PM"] {
            assert_eq!(to_time(bad), None, "{bad}");
        }
        assert_eq!(infer(&["2:05 PM", "11:59 AM"], &F), Dtype::Time);
        assert_eq!(to_date("2024-02-05 2:05 PM", Order::Ymd), to_date("2024-02-05 14:05", Order::Ymd));
        assert_eq!(to_date("2/5/2024, 2:05:00 PM", Order::Mdy), to_date("2024-02-05 14:05", Order::Ymd));
        // The app's own 12-hour format reads back.
        let shown = fmt("2024-02-05 14:05", Dtype::DateTime, PLAIN, Show::DateTime(DatePat::Long, TimePat::H12)).unwrap();
        assert_eq!(to_date(&shown, Order::Ymd), to_date("2024-02-05 14:05", Order::Ymd));
    }
    #[test]
    fn time_with_a_zone() {
        for v in ["09:05:48+00", "09:05:48Z", "09:05:48 +0700", "09:05:48+05:30", "9:05:48 AM -03"] {
            assert_eq!(to_time(v), to_time("09:05:48"), "{v}");
        }
        assert_eq!(to_time("09:00-17:00"), None);
        assert_eq!(infer(&["09:05:48+00", "10:00:00+00"], &F), Dtype::Time);
    }
    #[test]
    fn two_digit_year_only_by_hand() {
        // Auto never reads one: a version number would be a date.
        assert_eq!(infer(&["01/02/03", "04/05/06"], &F), Dtype::String);
        assert_eq!(infer(&["1.2.10", "2.0.11"], &F), Dtype::String);
        assert_eq!(to_date("01/02/03", Order::Dmy), None);
        let by_hand = |order| Kind::Date { order }.typed().unwrap().1;
        for (order, same) in [(Order::Dmy, "2003-02-01"), (Order::Mdy, "2003-01-02"), (Order::Ymd, "2001-02-03")] {
            let r = by_hand(order);
            assert!(r.short_year && fits(Dtype::Date, &r, "01/02/03"));
            assert_eq!(read_date("01/02/03", &r), to_date(same, Order::Ymd), "{order:?}");
        }
        let r = by_hand(Order::Dmy);
        assert_eq!(read_date("01/01/68", &r), to_date("2068-01-01", Order::Ymd));
        assert_eq!(read_date("01/01/69", &r), to_date("1969-01-01", Order::Ymd));
        assert_eq!(read_date("31/12/99 23:59", &r), to_date("1999-12-31 23:59", Order::Ymd));
        assert_eq!(read_date("01/02/2003", &r), to_date("2003-02-01", Order::Ymd));
        for bad in ["01/02/003", "01/02/3", "001/02/03"] {
            assert_eq!(read_date(bad, &r), None, "{bad}");
        }
    }
    #[test]
    fn dtype_time_and_datetime() {
        assert_eq!(infer(&["00:02:05.881533", "01:27:36.5", "14:05"], &F), Dtype::Time);
        assert_eq!(infer(&["2024-01-05 10:00", "2024-01-06"], &F), Dtype::DateTime);
        assert_eq!(infer(&["2024-01-05", "2024-01-06"], &F), Dtype::Date);
        assert_eq!(to_time("00:02:05.881533"), Some(125_881));
        assert_eq!(to_time("14:05"), Some(50_700_000));
        assert_eq!(to_time("100:00:00"), Some(360_000_000));
        assert_eq!(to_time(" 1:02:03.4 "), Some(3_723_400));
        for bad in ["1:2", "1:60", "1:02:60", "1:02.5", "1:02:03.", ":02", "1:02:03:04", "12"] {
            assert_eq!(to_time(bad), None, "{bad}");
        }
    }
    #[test]
    fn time_sorts_filters_and_shows() {
        let v = "00:02:05.881533";
        assert_eq!(fmt(v, Dtype::Time, PLAIN, Show::Duration { unit: Unit::S, words: true }).unwrap(), "2m 6s");
        assert_eq!(fmt(v, Dtype::Time, PLAIN, Show::Duration { unit: Unit::Ms, words: false }).unwrap(), "0:02:06");
        assert_eq!(fmt("14:05:09", Dtype::Time, PLAIN, Show::Time(TimePat::H12)).unwrap(), "2:05 PM");
        assert_eq!(fmt("x", Dtype::Time, PLAIN, Show::Time(TimePat::Hm)), None);
        assert_eq!(fmt("2024-02-05 14:05:09", Dtype::DateTime, PLAIN, Show::Time(TimePat::Hms)).unwrap(), "14:05:09");
        assert_eq!(fmt("2024-02-05 14:05:09", Dtype::DateTime, PLAIN, Show::Date(DatePat::Long)).unwrap(), "5 Feb 2024");
        assert_eq!(sorted(&one_col(Dtype::Time, &["1:00:00", "x", "0:02:05.9", v]), 0), [vec![3, 2, 0, 1], vec![0, 2, 3, 1]]);
        assert_eq!(pick(&["0:01:12", "0:15:00", "1:27:36"], Dtype::Time, Op::Gt, "0:10:00", "", PLAIN), ["0:15:00", "1:27:36"]);
        assert_eq!(pick(&["2024-01-05 10:00", "2024-02-05 09:00"], Dtype::DateTime, Op::Lt, "2024-02-01", "", PLAIN), ["2024-01-05 10:00"]);
        assert_eq!((time_label(125_881), time_label(3_600_000)), ("0:02:05.881".into(), "1:00:00".into()));
        assert_eq!(to_time(&time_label(125_881)), Some(125_881));
    }
    #[test]
    fn dtype_95_percent_threshold_empties_ignored_1000_value_cap() {
        assert_eq!(infer(&[n(96, "1"), n(4, "x")].concat(), &F), Dtype::Int);
        assert_eq!(infer(&[n(90, "1"), n(10, "x")].concat(), &F), Dtype::String);
        assert_eq!(infer(&["1", "", " ", "2"], &F), Dtype::Int);
        assert_eq!(infer(&["", ""], &F), Dtype::String);
        assert_eq!(infer(&[n(1000, "1"), n(2000, "x")].concat(), &F), Dtype::Int);
    }

    // Read format
    #[test]
    fn format_single_quote_qualifier() {
        assert_eq!(parse("'a,b',c\n'it''s',d", ",", "'"), rows(&[&["a,b", "c"], &["it's", "d"]]));
        assert_eq!(parse("\"x,y\",z", ",", "'"), rows(&[&["\"x", "y\"", "z"]]));
        assert_eq!(detect_delimiter(b"'a;b',c\n'd;e',f\n'g;h',i", "", "'"), ",");
    }
    #[test]
    fn format_no_qualifier() {
        assert_eq!(parse("\"a,b\",c\n'd',e", ",", ""), rows(&[&["\"a", "b\"", "c"], &["'d'", "e"]]));
    }
    #[test]
    fn format_date_order_dmy_auto_ambiguous_is_dmy() {
        let (t, r) = infer_column(["13/01/2024", "05/02/2024"], &F);
        assert_eq!((t, r.order), (Dtype::Date, Order::Dmy));
        assert_eq!(to_date("05/02/2024", Order::Dmy), Some(1_707_091_200_000));
        assert_eq!(infer_column(["01/02/2024", "03/04/2024"], &F).1.order, Order::Dmy);
        assert_eq!(pick(&["13/01/2024", "05/02/2024"], Dtype::Date, Op::Lt, "2024-02-01", "", r), ["13/01/2024"]);
    }
    #[test]
    fn format_date_order_mdy_auto() {
        let (t, r) = infer_column(["12/25/2024", "01/05/2024 08:30"], &F);
        assert_eq!((t, r.order), (Dtype::DateTime, Order::Mdy));
        assert_eq!(to_date("12/25/2024", Order::Mdy), Some(1_735_084_800_000));
        assert_eq!(to_date("12/25/2024", Order::Dmy), None);
        assert_eq!(infer(&["12/25/2024"], &Format { order: Some(Order::Dmy), ..F }), Dtype::String);
    }
    #[test]
    fn format_dot_date_delimiter() {
        assert_eq!(infer(&["05.01.2024", "31.12.2023 08:30"], &F), Dtype::DateTime);
        assert_eq!(to_date("31.12.2023", Order::Dmy), Some(1_703_980_800_000));
        assert_eq!(to_date("2024.01.05", Order::Ymd), Some(1_704_412_800_000));
        assert_eq!(to_date("2024.01-05", Order::Ymd), None);
        assert_eq!(infer(&["1.5", "2.25"], &F), Dtype::Float);
    }
    #[test]
    fn format_record_delimiter_detection() {
        assert_eq!(record_delimiter(b"a,b\r\n1,2\r\n"), "CRLF");
        assert_eq!(record_delimiter(b"a,b\n1,2"), "LF");
        assert_eq!(record_delimiter(b"a,b\r1,2"), "CR");
        assert_eq!(record_delimiter(b"a,b"), "none");
    }

    // Encoding
    /// Thai to Windows-874 bytes: U+0E01–U+0E5B are 0xA1–0xFB, and ASCII stays.
    fn tis(s: &str) -> Vec<u8> {
        s.chars().map(|ch| if ('\u{E01}'..='\u{E5B}').contains(&ch) { (ch as u32 - 0xE00 + 0xA0) as u8 } else { ch as u8 }).collect()
    }
    #[test]
    fn encoding_windows_874_bytes_give_thai_text() {
        let s = "id,เมือง\n1,กรุงเทพ";
        assert_eq!(decode(&tis(s), "windows-874"), (s.to_string(), 0));
        assert!(decode(&tis(s), "utf-8").1 > 0);
        assert_eq!(decode(&[0xff, 0xfe, 0x61, 0x00], "utf-16le").0, "a");
    }
    #[test]
    fn encoding_bad_utf8_bytes_are_counted() {
        assert_eq!(decode(&[0x61, 0xff, 0x62, 0xc3], "utf-8"), ("a\u{FFFD}b\u{FFFD}".to_string(), 2));
        assert_eq!(decode("ok,é".as_bytes(), "utf-8"), ("ok,é".to_string(), 0));
        assert_eq!(count_bad_utf8(&[0x61, 0xff, 0x62, 0xc3]), 2);
        assert_eq!(count_bad_utf8("ok,é".as_bytes()), 0);
    }

    // New: the index finds the same records as `parse`.
    #[test]
    fn index_gives_the_rows_parse_gives() {
        let cases: &[(&str, &str, &str)] = &[
            ("a,b\n1,2", ",", "\""),
            ("\"a,b\",c", ",", "\""),
            ("\"say \"\"hi\"\"\",x", ",", "\""),
            ("a,b\r\n1,2\r\n", ",", "\""),
            ("\"line1\nline2\",x\ny,z", ",", "\""),
            ("h\r\n\"a\r\nb\",1\n\nd,2\r\"e\re\",3", ",", "\""),
            ("\u{FEFF}a,b\n1,2", ",", "\""),
            ("a,b,c\n1\n\n2,3\n", ",", "\""),
            ("a,", ",", "\""),
            ("a,\nb,c", ",", "\""),
            ("'a,b',c\n'it''s',d", ",", "'"),
            ("\"x,y\",z", ",", "'"),
            ("\"a,b\",c\n'd',e", ",", ""),
            // A quote inside a field is text; a quote after a closing quote is text.
            ("5'11\",x\ny,\"z\nz\"\n\"a\"b\"c\n,\"d\ne\"", ",", "\""),
            // Records that parse to one empty field are dropped.
            ("a\n\"\"\n\"\n", ",", "\""),
            ("x\n\"\"", ",", "\""),
            // A delimiter and a qualifier of more than one byte.
            ("a¦b\n\"c¦d\"¦e\nþf¦gþ¦h", "¦", "þ"),
            // An unclosed quote runs to the end.
            ("a,\"b\nc,d", ",", "\""),
        ];
        for &(text, d, q) in cases {
            let mut starts = Vec::new();
            index_records(text.as_bytes(), d.as_bytes(), q.as_bytes(), &mut |b| {
                starts.extend_from_slice(b);
                true
            });
            let got: Vec<Vec<String>> = starts.iter().map(|&s| parse_record(text.as_bytes(), s as usize, d.as_bytes(), q.as_bytes()).0).collect();
            assert_eq!(got, parse(text, d, q), "{text:?}");
        }
    }

    // Phase 2 helpers: a Doc from text, indexed.
    fn doc_of(text: &str, d: &str, header: bool) -> Doc {
        let opts = Opts { delim: Some(d.into()), header: Some(header), ..Opts::default() };
        let doc = Doc::from_bytes(Bytes::Owned(text.as_bytes().to_vec()), None, "utf-8", Path::new("t"), &opts, || {});
        while !doc.index.done.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        doc
    }
    fn table(text: &str, d: &str) -> Doc {
        doc_of(text, d, true)
    }
    /// No header, and the given dtypes. A last column "_" keeps a row whose cells are empty.
    fn typed(dtypes: &[Dtype], rows: &[&[&str]]) -> Doc {
        let text: Vec<String> = rows.iter().map(|r| format!("{},_", r.join(","))).collect();
        let mut doc = doc_of(&text.join("\n"), ",", false);
        doc.dtypes = dtypes.to_vec();
        doc.reads = vec![PLAIN; dtypes.len()];
        doc
    }
    fn one_col(t: Dtype, v: &[&str]) -> Doc {
        let rows: Vec<&[&str]> = v.iter().map(std::slice::from_ref).collect();
        typed(&[t], &rows)
    }
    fn q(doc: &Doc, search: &str, scope: &[usize], filters: &[Filter]) -> Vec<u32> {
        doc.scan().unwrap().query(search, scope, filters, &AtomicBool::new(false))
    }
    fn all(doc: &Doc) -> Vec<u32> {
        (0..doc.rows() as u32).collect()
    }
    fn sorted(doc: &Doc, col: usize) -> [Vec<u32>; 2] {
        let s = doc.scan().unwrap();
        [s.sort(&all(doc), &[k(col, Dir::Asc)]), s.sort(&all(doc), &[k(col, Dir::Desc)])]
    }
    fn values(doc: &Doc, c: usize, hits: &[u32]) -> Vec<String> {
        hits.iter().map(|&i| doc.row(i as usize)[c].clone()).collect()
    }
    fn flt(col: usize, op: Op, a: &str, b: &str) -> Filter {
        Filter { col, op, a: a.into(), b: b.into(), or: false }
    }
    fn or(f: Filter) -> Filter {
        Filter { or: true, ..f }
    }
    fn pick<'a>(v: &[&'a str], t: Dtype, op: Op, a: &str, b: &str, r: Reading) -> Vec<&'a str> {
        let p = predicate(&flt(0, op, a, b), t, r).expect("complete");
        v.iter().copied().filter(|x| p(x)).collect()
    }
    fn k(col: usize, dir: Dir) -> SortKey {
        SortKey { col, dir }
    }

    // Operators
    #[test]
    fn ops_string_contains_equals_starts_empty_notempty() {
        let v = ["Apple", "banana", "", "Pineapple ", "apple"];
        let s = Dtype::String;
        assert_eq!(pick(&v, s, Op::Contains, "APP", "", PLAIN), ["Apple", "Pineapple ", "apple"]);
        assert_eq!(pick(&v, s, Op::Equals, "apple", "", PLAIN), ["Apple", "apple"]);
        assert_eq!(pick(&v, s, Op::Starts, "pine", "", PLAIN), ["Pineapple "]);
        assert_eq!(pick(&v, s, Op::Empty, "", "", PLAIN), [""]);
        assert_eq!(pick(&v, s, Op::NotEmpty, "", "", PLAIN), ["Apple", "banana", "Pineapple ", "apple"]);
    }
    #[test]
    fn ops_string_is_exactly_keeps_case() {
        let v = ["Apple", "banana", "", "Pineapple ", "apple", " Apple "];
        assert_eq!(pick(&v, Dtype::String, Op::Is, "apple", "", PLAIN), ["apple"]);
        assert_eq!(pick(&v, Dtype::String, Op::Is, " Apple", "", PLAIN), ["Apple", " Apple "]);
        assert!(predicate(&flt(0, Op::Is, " ", ""), Dtype::String, PLAIN).is_none());
    }
    #[test]
    fn a_top_value_count_is_the_rows_its_exact_filter_keeps() {
        // Case and spaces vary: "equals" keeps 5 rows of Oslo; the Profile counts 3.
        let t = one_col(Dtype::String, &["Oslo", "oslo", " Oslo ", "Lima", "OSLO", "Oslo", "Lima"]);
        let p = prof(&t, None, None, 0, Distinct::Exact);
        assert_eq!(p.top[0], ("Oslo".to_string(), 3));
        assert_eq!(q(&t, "", &[], &[flt(0, Op::Is, "Oslo", "")]).len(), 3);
        assert_eq!(q(&t, "", &[], &[flt(0, Op::Equals, "Oslo", "")]).len(), 5);
    }
    #[test]
    fn ops_number_numeric_not_text_order() {
        let v = ["1", "2", "3", "10", "", "2.5"];
        let f = Dtype::Float;
        assert_eq!(pick(&v, f, Op::Eq, "2", "", PLAIN), ["2"]);
        assert_eq!(pick(&v, f, Op::Ne, "2", "", PLAIN), ["1", "3", "10", "2.5"]);
        assert_eq!(pick(&v, f, Op::Gt, "2", "", PLAIN), ["3", "10", "2.5"]);
        assert_eq!(pick(&v, f, Op::Ge, "2", "", PLAIN), ["2", "3", "10", "2.5"]);
        assert_eq!(pick(&v, f, Op::Lt, "2", "", PLAIN), ["1"]);
        assert_eq!(pick(&v, f, Op::Le, "2", "", PLAIN), ["1", "2"]);
        assert_eq!(pick(&v, f, Op::Between, "2", "3", PLAIN), ["2", "3", "2.5"]);
        assert_eq!(pick(&v, f, Op::Between, "3", "2", PLAIN), ["2", "3", "2.5"]);
    }
    #[test]
    fn ops_date_before_after_between() {
        let v = ["2024-01-05", "2024-02-10", "2024/03/01", "2024-02-10 08:30", ""];
        let t = Dtype::Date;
        assert_eq!(pick(&v, t, Op::Lt, "2024-02-10", "", PLAIN), ["2024-01-05"]);
        assert_eq!(pick(&v, t, Op::Gt, "2024-02-10", "", PLAIN), ["2024/03/01", "2024-02-10 08:30"]);
        assert_eq!(pick(&v, t, Op::Between, "2024-01-01", "2024-02-10", PLAIN), ["2024-01-05", "2024-02-10"]);
    }
    #[test]
    fn ops_bool_is_true_is_false() {
        let v = ["true", "FALSE", "True", "", "false"];
        assert_eq!(pick(&v, Dtype::Bool, Op::True, "", "", PLAIN), ["true", "True"]);
        assert_eq!(pick(&v, Dtype::Bool, Op::False, "", "", PLAIN), ["FALSE", "false"]);
    }
    #[test]
    fn ops_incomplete_filter_is_ignored() {
        assert!(predicate(&flt(0, Op::Gt, "", ""), Dtype::Int, PLAIN).is_none());
        assert!(predicate(&flt(0, Op::Between, "1", ""), Dtype::Int, PLAIN).is_none());
        assert!(predicate(&flt(0, Op::Contains, " ", ""), Dtype::String, PLAIN).is_none());
        let t = table("x\n1\n2", ",");
        assert_eq!(q(&t, "", &[], &[flt(0, Op::Gt, "", "")]), [0, 1]);
    }

    // Search and filters
    #[test]
    fn query_search_all_vs_one_column_filters_and_combined() {
        let t = table("name,city,age\nAnn,Oslo,30\nBob,Lima,41\nCid,Oslo,25\n", ",");
        assert_eq!(t.names, ["name", "city", "age"]);
        assert_eq!(t.dtypes, [Dtype::String, Dtype::String, Dtype::Int]);
        assert_eq!(q(&t, "O", &[1], &[]), [0, 2]);
        assert_eq!(q(&t, "o", &[], &[]), [0, 1, 2]);
        let oslo = flt(1, Op::Equals, "OSLO", "");
        assert_eq!(q(&t, "", &[], &[oslo, flt(2, Op::Gt, "26", "")]), [0]);
        assert_eq!(q(&t, "o", &[], &[flt(2, Op::Lt, "35", "")]), [0, 2]);
    }
    #[test]
    fn a_bar_range_holds_the_values_its_bar_counts() {
        // Whole numbers, dates in whole seconds, and decimals; spans a bar is narrower than a step in.
        let cases: [(Vec<f64>, f64); 4] = [
            ((0..=97).map(|x| x as f64).collect(), 1.0),
            ((0..5).map(|x| x as f64).collect(), 1.0),
            ((0..500).map(|x| 1_727_000_000_000.0 + x as f64 * 7_000.0).collect(), 1000.0),
            ((0..300).map(|x| -3.0 + x as f64 * 0.0173).collect(), 0.0),
        ];
        for (v, step) in cases {
            let span = (v[0], v[v.len() - 1]);
            for i in 0..BINS {
                let (lo, hi) = bin_range(span, i, step);
                let counted = v.iter().filter(|&&x| bin(x, span) == i).count();
                let between = v.iter().filter(|&&x| x >= lo && x <= hi).count();
                assert_eq!(counted, between, "bar {i} of {span:?}: {lo}..={hi}");
            }
        }
    }
    #[test]
    fn query_filters_and_binds_before_or() {
        let t = table("name,city,age\nAnn,Oslo,30\nBob,Lima,41\nCid,Oslo,25\nDee,Rome,52\n", ",");
        let (oslo, rome, lima) = (flt(1, Op::Equals, "oslo", ""), flt(1, Op::Equals, "rome", ""), flt(1, Op::Equals, "lima", ""));
        let over26 = flt(2, Op::Gt, "26", "");
        assert_eq!(q(&t, "", &[], &[oslo.clone(), over26.clone()]), [0]);
        // (Oslo and > 26) or Rome
        assert_eq!(q(&t, "", &[], &[oslo.clone(), over26.clone(), or(rome.clone())]), [0, 3]);
        // Rome or (Oslo and > 26)
        assert_eq!(q(&t, "", &[], &[rome.clone(), or(oslo.clone()), over26.clone()]), [0, 3]);
        assert_eq!(q(&t, "", &[], &[lima.clone(), or(rome)]), [1, 3]);
        // A filter with no value yet: left out, and so is the group it leaves empty.
        assert_eq!(q(&t, "", &[], &[oslo.clone(), or(flt(2, Op::Gt, "", ""))]), [0, 2]);
        assert_eq!(q(&t, "", &[], &[flt(2, Op::Gt, "", ""), or(lima.clone())]), [1]);
        // The first filter's "or" is not used; the search still applies to every group.
        assert_eq!(q(&t, "", &[], &[or(lima)]), [1]);
        assert_eq!(q(&t, "ann", &[], &[oslo, or(flt(1, Op::Equals, "lima", ""))]), [0]);
    }
    #[test]
    fn format_decimal_comma_auto_forced_and_in_filters() {
        let t = table("id;price;code\n1;1,5;7\n2;2;8\n3;-3,25;9", ";");
        assert_eq!(t.dtypes, [Dtype::Int, Dtype::Float, Dtype::Int]);
        assert!(t.reads[1].comma);
        assert_eq!(q(&t, "", &[], &[flt(1, Op::Gt, "1.4", "")]), [0, 1]);
        assert_eq!(infer(&["1,5", "2,25"], &Format { comma: Some(false), ..F }), Dtype::String);
        assert!(!infer_column(["1.5", "2"], &F).1.comma);
        assert!(detect_header(&parse("name;price\nfoo;1,5\nbar;2", ";", "\""), &F).0);
    }
    fn people() -> Doc {
        table("name,city,note\nAnn,Oslo,x\nBob,Lima,oslo trip\nCid,Rome,y", ",")
    }
    #[test]
    fn search_a_match_in_any_picked_column() {
        assert_eq!(q(&people(), "OSLO", &[1, 2], &[]), [0, 1]);
    }
    #[test]
    fn search_unpicked_columns_ignored() {
        let t = people();
        assert_eq!(q(&t, "oslo", &[2], &[]), [1]);
        assert_eq!(q(&t, "ann", &[1, 2], &[]), Vec::<u32>::new());
        assert_eq!(q(&t, "ann", &[], &[]), [0]);
    }

    // Sort
    #[test]
    fn sort_numbers_with_a_decimal_comma_both_directions() {
        let t = table("id;price\n1;1,5\n2;10\n3;-3,25\n4;2", ";");
        assert_eq!((t.dtypes[1], t.reads[1].comma), (Dtype::Float, true));
        let [asc, desc] = sorted(&t, 1);
        assert_eq!(values(&t, 1, &asc), ["-3,25", "1,5", "2", "10"]);
        assert_eq!(values(&t, 1, &desc), ["10", "2", "1,5", "-3,25"]);
        assert_eq!(t.scan().unwrap().sort(&[0, 1, 3], &[k(1, Dir::Desc)]), [1, 3, 0]);
    }
    #[test]
    fn sort_dmy_dates() {
        let t = table("d\n13/01/2024\n05/02/2023\n01/12/2024", ",");
        assert_eq!((t.dtypes[0], t.reads[0].order), (Dtype::Date, Order::Dmy));
        let [asc, desc] = sorted(&t, 0);
        assert_eq!(values(&t, 0, &asc), ["05/02/2023", "13/01/2024", "01/12/2024"]);
        assert_eq!(values(&t, 0, &desc), ["01/12/2024", "13/01/2024", "05/02/2023"]);
    }
    #[test]
    fn sort_strings_with_numbers_in_numeric_order_case_ignored() {
        let t = one_col(Dtype::String, &["item 10", "Item 2", "banana", "Apple", "item 1"]);
        let [asc, desc] = sorted(&t, 0);
        assert_eq!(values(&t, 0, &asc), ["Apple", "banana", "item 1", "Item 2", "item 10"]);
        assert_eq!(values(&t, 0, &desc), ["item 10", "Item 2", "item 1", "banana", "Apple"]);
    }
    #[test]
    fn sort_text_compares_whole_keys_past_the_15_byte_prefix() {
        let a15 = "a".repeat(15);
        let v = [format!("{a15}X2"), format!("{a15}a"), "ab\u{0}".to_string(), format!("{a15}X1"), String::new(), a15.clone(), "ab".to_string(), format!("{}X1", "A".repeat(15)), format!("{a15}b")];
        let t = one_col(Dtype::String, &v.iter().map(String::as_str).collect::<Vec<_>>());
        // Equal keys (rows 3 and 7, case aside) keep the file order in both directions; empty is last.
        assert_eq!(sorted(&t, 0), [vec![5, 1, 8, 3, 7, 0, 6, 2, 4], vec![2, 6, 0, 3, 7, 8, 1, 5, 4]]);
    }
    #[test]
    fn sort_empty_and_unreadable_cells_last_in_both_directions() {
        assert_eq!(sorted(&one_col(Dtype::Int, &["3", "", "x", "1", "2"]), 0), [vec![3, 4, 0, 1, 2], vec![0, 4, 3, 1, 2]]);
        assert_eq!(sorted(&one_col(Dtype::String, &["b", " ", "a"]), 0), [vec![2, 0, 1], vec![0, 2, 1]]);
        assert_eq!(sorted(&one_col(Dtype::Date, &["2024-01-02", "soon", "2023-05-06"]), 0), [vec![2, 0, 1], vec![0, 2, 1]]);
    }
    #[test]
    fn sort_equal_values_keep_file_order_bool_false_first_ranks_cached() {
        let t = one_col(Dtype::Int, &["2", "1", "2", "1"]);
        assert_eq!(sorted(&t, 0), [vec![1, 3, 0, 2], vec![0, 2, 1, 3]]);
        assert!(t.ranks.lock().unwrap().contains_key(&0));
        assert_eq!(sorted(&one_col(Dtype::String, &["oslo", "Lima", "Oslo"]), 0), [vec![1, 0, 2], vec![0, 2, 1]]);
        assert_eq!(sorted(&one_col(Dtype::Bool, &["true", "FALSE", "True", "false"]), 0), [vec![1, 3, 0, 2], vec![0, 2, 1, 3]]);
    }
    #[test]
    fn sort_two_levels_ascending_then_descending() {
        let t = table("cat,price\nb,1.5\na,2\nb,3\na,10\na,2", ",");
        assert_eq!(t.dtypes, [Dtype::String, Dtype::Float]);
        let s = t.scan().unwrap();
        assert_eq!(s.sort(&all(&t), &[k(0, Dir::Asc), k(1, Dir::Desc)]), [3, 1, 4, 2, 0]);
        assert_eq!(s.sort(&all(&t), &[k(0, Dir::Desc), k(1, Dir::Asc)]), [0, 2, 1, 4, 3]);
    }
    #[test]
    fn sort_empty_and_unreadable_cells_last_inside_a_level() {
        let rows: &[&[&str]] = &[&["a", "3"], &["a", ""], &["b", "1"], &["a", "x"], &["a", "1"], &["b", ""], &["", "2"], &["", "1"]];
        let t = typed(&[Dtype::String, Dtype::Int], rows);
        let s = t.scan().unwrap();
        assert_eq!(s.sort(&all(&t), &[k(0, Dir::Asc), k(1, Dir::Asc)]), [4, 0, 1, 3, 2, 5, 7, 6]);
        assert_eq!(s.sort(&all(&t), &[k(0, Dir::Desc), k(1, Dir::Desc)]), [2, 5, 0, 4, 1, 3, 6, 7]);
    }
    #[test]
    fn sort_three_levels_and_an_empty_list_gives_the_file_order() {
        let t = table("country,city,total\nTH,Bangkok,5\nNO,Oslo,3\nTH,Bangkok,2\nNO,Bergen,7\nTH,Chiang Mai,1\nNO,Oslo,9\nTH,Bangkok,5", ",");
        assert_eq!(t.dtypes, [Dtype::String, Dtype::String, Dtype::Int]);
        let s = t.scan().unwrap();
        let keys = [k(0, Dir::Asc), k(1, Dir::Desc), k(2, Dir::Asc)];
        assert_eq!(s.sort(&all(&t), &keys), [1, 5, 3, 4, 2, 0, 6]);
        assert_eq!(s.sort(&all(&t), &[keys[2], keys[0]]), [4, 2, 1, 0, 6, 3, 5]);
        assert_eq!(s.sort(&all(&t), &[]), [0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(s.sort(&[3, 1, 2], &[]), [3, 1, 2]);
    }

    // Phase 3: go to row, select, copy, fit, arrange
    fn cities() -> Doc {
        table("name,city,age\nAnn,Oslo,30\nBob,Lima,41\nCid,Oslo,25", ",")
    }
    // The JS test also asks for row -1, which a u32 cannot hold; [2, 5] and 1 cover "none".
    #[test]
    fn lookup_floor_index_finds_a_row_among_the_hits() {
        let hits = [0, 2, 5, 9];
        let got: Vec<Option<usize>> = [0, 1, 2, 5, 8, 9, 12].iter().map(|&r| floor_index(&hits, r)).collect();
        assert_eq!(got, [Some(0), Some(0), Some(1), Some(2), Some(2), Some(3), Some(3)]);
        assert_eq!(floor_index(&[2, 5], 1), None);
        assert_eq!(floor_index(&[], 3), None);
    }
    #[test]
    fn copy_tsv_quoting() {
        let r = rows(&[&["a", "b\tc"], &["say \"hi\"", "x\ny"], &["", "z"]]);
        assert_eq!(to_tsv(&r), "a\t\"b\tc\"\n\"say \"\"hi\"\"\"\t\"x\ny\"\n\tz");
    }
    #[test]
    fn copy_a_cell_rectangle_with_and_without_field_names() {
        let t = cities();
        let s = Sel { ar: 1, ac: 2, fr: 0, fc: 1 };
        assert_eq!(t.cells(Some(&[0, 1, 2]), &[0, 1, 2], s, false), rows(&[&["Oslo", "30"], &["Lima", "41"]]));
        assert_eq!(t.cells(Some(&[0, 1, 2]), &[0, 1, 2], s, true), rows(&[&["city", "age"], &["Oslo", "30"], &["Lima", "41"]]));
    }
    #[test]
    fn copy_rows_by_shown_columns() {
        let s = Sel { ar: 0, ac: 0, fr: 1, fc: 1 };
        assert_eq!(cities().cells(Some(&[0, 1, 2]), &[0, 2], s, false), rows(&[&["Ann", "30"], &["Bob", "41"]]));
    }
    #[test]
    fn copy_columns_by_filtered_rows() {
        let t = cities();
        let oslo = q(&t, "", &[], &[flt(1, Op::Equals, "oslo", "")]);
        assert_eq!(oslo, [0, 2]);
        let s = Sel { ar: 0, ac: 1, fr: 1, fc: 1 };
        assert_eq!(t.cells(Some(&oslo), &[0, 1, 2], s, false), rows(&[&["Oslo"], &["Oslo"]]));
    }
    #[test]
    fn select_arrow_moves_stop_at_the_edges() {
        let s = Sel::default();
        assert_eq!(s.step(-1, -1, false, 3, 2), s);
        assert_eq!(s.step(1, 1, true, 3, 2), Sel { ar: 0, ac: 0, fr: 1, fc: 1 });
        let far = Sel { ar: 0, ac: 0, fr: 2, fc: 1 };
        assert_eq!(far.step(1, 1, true, 3, 2), far);
        assert_eq!(far.step(1, 0, false, 3, 2), Sel { ar: 1, ac: 0, fr: 1, fc: 0 });
    }
    fn letters() -> Doc {
        one_col(Dtype::String, &["bb", "a", "dddd", "cc", "eeee", "fff"])
    }
    #[test]
    fn longest_the_k_longest_in_order_with_ties() {
        let (t, all): (Doc, Vec<usize>) = (letters(), (0..6).collect());
        assert_eq!(t.longest(&all, 0, 3), ["dddd", "eeee", "fff"]);
        assert_eq!(t.longest(&all, 0, 4), ["dddd", "eeee", "fff", "bb"]);
        assert_eq!(t.longest(&all, 0, 10), ["dddd", "eeee", "fff", "bb", "cc", "a"]);
    }
    #[test]
    fn longest_only_the_given_rows_count() {
        let t = letters();
        assert_eq!(t.longest(&[1, 3, 5], 0, 2), ["fff", "cc"]);
        assert_eq!(t.longest(&[3, 0], 0, 2), ["cc", "bb"]);
    }
    #[test]
    fn longest_an_empty_column_gives_one_empty_string() {
        let t = doc_of("a,\nb\nc,", ",", false);
        assert_eq!(t.longest(&[0, 1, 2], 1, 20), [""]);
        assert_eq!(t.longest(&[], 0, 20), [""]);
    }
    #[test]
    fn arrange_a_column_moves_in_front_of_the_drop_position() {
        let order = [0, 1, 2, 3];
        assert_eq!(arrange(&order, 0, 2), [1, 0, 2, 3]);
        assert_eq!(arrange(&order, 0, 4), [1, 2, 3, 0]);
        assert_eq!(arrange(&order, 3, 0), [3, 0, 1, 2]);
        assert_eq!(arrange(&order, 3, 1), [0, 3, 1, 2]);
        assert_eq!(order, [0, 1, 2, 3]);
    }
    #[test]
    fn arrange_a_drop_on_either_side_of_the_dragged_column_changes_nothing() {
        let order = [5, 6, 7];
        for (from, to) in [(1, 1), (1, 2), (0, 0), (2, 3)] {
            assert_eq!(arrange(&order, from, to), order);
        }
    }
    // The JS test also tries -1, which a usize cannot hold.
    #[test]
    fn arrange_a_position_outside_the_list_changes_nothing() {
        for (from, to) in [(3, 1), (1, 4)] {
            assert_eq!(arrange(&[0, 1, 2], from, to), [0, 1, 2]);
        }
        assert_eq!(arrange(&[], 0, 0), Vec::<usize>::new());
    }
    #[test]
    fn row_upto_stops_after_the_fields_it_needs() {
        let t = cities();
        assert_eq!(t.row_upto(0, 2), ["Ann", "Oslo"]);
        assert_eq!(t.row_upto(0, 9), ["Ann", "Oslo", "30"]);
        assert_eq!(t.row_upto(0, 0), Vec::<String>::new());
        assert_eq!(t.row_upto(9, 2), Vec::<String>::new());
    }
    #[test]
    fn arrange_the_column_names_follow_the_order() {
        let t = table("name,city,total\nAda,Oslo,3", ",");
        let order = arrange(&[0, 1, 2], 1, 0);
        assert_eq!(order.iter().map(|&c| t.names[c].as_str()).collect::<Vec<_>>(), ["city", "name", "total"]);
        let row = t.row(0);
        assert_eq!(order.iter().map(|&c| row[c].as_str()).collect::<Vec<_>>(), ["Oslo", "Ada", "3"]);
    }

    // Phase 4: Export and Save
    #[test]
    fn save_csv_quoting_and_the_parser_reads_it_back() {
        let r = rows(&[&["id", "note"], &["1", "say \"hi\", then go"], &["2", "a\r\nb"], &["3", "tab\there"], &["4", ""]]);
        let csv = to_delimited(&r, ",", "\n", "\"");
        assert_eq!(csv, "id,note\n1,\"say \"\"hi\"\", then go\"\n2,\"a\r\nb\"\n3,tab\there\n4,");
        assert_eq!(parse(&csv, ",", "\""), r);
    }
    #[test]
    fn to_delimited_the_record_delimiter_and_the_text_qualifier_of_the_file() {
        let r = rows(&[&["a", "b"], &["x,1", "say \"hi\""], &["two\nlines", "plain"]]);
        assert_eq!(to_delimited(&r, ",", "\n", "\""), "a,b\n\"x,1\",\"say \"\"hi\"\"\"\n\"two\nlines\",plain");
        assert_eq!(to_delimited(&r, ",", "\r\n", "\""), "a,b\r\n\"x,1\",\"say \"\"hi\"\"\"\r\n\"two\nlines\",plain");
        assert_eq!(to_delimited(&rows(&[&["a'b", "c"]]), ",", "\n", "'"), "'a''b',c");
        assert_eq!(to_delimited(&rows(&[&["a,b", "c"]]), ",", "\n", ""), "a,b,c");
    }
    #[test]
    fn to_delimited_what_it_writes_comes_back_through_parse() {
        let r = rows(&[&["name", "note"], &["Ada", "two\nlines and a \"quote\""], &["Bo", "x,y"]]);
        assert_eq!(parse(&to_delimited(&r, ",", "\r\n", "\""), ",", "\""), r);
        assert_eq!(parse(&to_delimited(&r, ";", "\n", "'"), ";", "'"), r);
    }
    #[test]
    fn write_gives_the_rows_and_columns_it_is_asked_for() {
        let t = table("name,city,age\nAnn,\"Oslo, NO\",30\nBob,Lima,41\nCid,Oslo,25", ",");
        let s = t.scan().unwrap();
        let mut out = Vec::new();
        s.write(&mut out, Some(&[2, 0]), &[2, 0], ";", "\r\n", "\"").unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "age;name\r\n25;Cid\r\n30;Ann\r\n");
        let mut out = Vec::new();
        s.write(&mut out, None, &[1], ",", "\n", "\"").unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "city\n\"Oslo, NO\"\nLima\nOslo\n");
    }

    // 0.7.0: cell formats and column types
    fn fmt(v: &str, t: Dtype, r: Reading, s: Show) -> Option<String> {
        show(v, t, r, &s)
    }
    const DMY: Reading = Reading { comma: false, order: Order::Dmy, short_year: false };
    const COMMA: Reading = Reading { comma: true, order: Order::Ymd, short_year: false };
    #[test]
    fn show_number_thousands_and_places() {
        let n = |places| Show::Number { sep: true, places };
        assert_eq!(fmt("1234567.5", Dtype::Float, PLAIN, n(None)).unwrap(), "1,234,567.5");
        assert_eq!(fmt("1234567.5", Dtype::Float, PLAIN, n(Some(2))).unwrap(), "1,234,567.50");
        assert_eq!(fmt("1234567.5", Dtype::Float, PLAIN, Show::Number { sep: false, places: Some(0) }).unwrap(), "1234568");
        assert_eq!(fmt("-1234.50", Dtype::Float, PLAIN, n(None)).unwrap(), "-1,234.50");
        assert_eq!(fmt("1234,5", Dtype::Float, COMMA, n(None)).unwrap(), "1.234,5");
        assert_eq!(fmt("42", Dtype::Int, PLAIN, n(None)).unwrap(), "42");
        assert_eq!(fmt("-0.001", Dtype::Float, PLAIN, n(Some(2))).unwrap(), "0.00");
        assert_eq!(fmt("x", Dtype::Float, PLAIN, n(None)), None);
    }
    #[test]
    fn show_percent() {
        let p = |places| Show::Percent { places };
        assert_eq!(fmt("0.125", Dtype::Float, PLAIN, p(None)).unwrap(), "12.5%");
        assert_eq!(fmt("0.07", Dtype::Float, PLAIN, p(None)).unwrap(), "7%");
        assert_eq!(fmt("1", Dtype::Int, PLAIN, p(None)).unwrap(), "100%");
        assert_eq!(fmt("0.5", Dtype::Float, PLAIN, p(Some(1))).unwrap(), "50.0%");
    }
    #[test]
    fn show_currency() {
        let c = |symbol: &str, places| Show::Currency { symbol: symbol.into(), places };
        assert_eq!(fmt("1234.5", Dtype::Float, PLAIN, c("$", Some(2))).unwrap(), "$1,234.50");
        assert_eq!(fmt("-3", Dtype::Int, PLAIN, c("€", Some(2))).unwrap(), "-€3.00");
        assert_eq!(fmt("1500000", Dtype::Int, PLAIN, c("฿", None)).unwrap(), "฿1,500,000");
        assert_eq!(fmt("-0.001", Dtype::Float, PLAIN, c("$", Some(2))).unwrap(), "$0.00");
    }
    #[test]
    fn show_duration() {
        let d = |unit, words| Show::Duration { unit, words };
        assert_eq!(fmt("3723", Dtype::Int, PLAIN, d(Unit::S, false)).unwrap(), "1:02:03");
        assert_eq!(fmt("3723", Dtype::Int, PLAIN, d(Unit::S, true)).unwrap(), "1h 2m 3s");
        assert_eq!(fmt("1500", Dtype::Int, PLAIN, d(Unit::Ms, false)).unwrap(), "0:00:02");
        assert_eq!(fmt("90", Dtype::Int, PLAIN, d(Unit::Min, true)).unwrap(), "1h 30m");
        assert_eq!(fmt("1.5", Dtype::Float, PLAIN, d(Unit::H, false)).unwrap(), "1:30:00");
        assert_eq!(fmt("0", Dtype::Int, PLAIN, d(Unit::S, true)).unwrap(), "0s");
        assert_eq!(fmt("90061", Dtype::Int, PLAIN, d(Unit::S, true)).unwrap(), "1d 1h 1m 1s");
        assert_eq!(fmt("90061", Dtype::Int, PLAIN, d(Unit::S, false)).unwrap(), "25:01:01");
        assert_eq!(fmt("-61", Dtype::Int, PLAIN, d(Unit::S, false)).unwrap(), "-0:01:01");
    }
    #[test]
    fn show_date_and_time() {
        let v = "2024-02-05 14:05:09";
        assert_eq!(fmt(v, Dtype::Date, PLAIN, Show::Date(DatePat::Iso)).unwrap(), "2024-02-05");
        assert_eq!(fmt(v, Dtype::Date, PLAIN, Show::Date(DatePat::Dmy)).unwrap(), "05/02/2024");
        assert_eq!(fmt(v, Dtype::Date, PLAIN, Show::Date(DatePat::Mdy)).unwrap(), "02/05/2024");
        assert_eq!(fmt(v, Dtype::Date, PLAIN, Show::Date(DatePat::Long)).unwrap(), "5 Feb 2024");
        assert_eq!(fmt(v, Dtype::Date, PLAIN, Show::DateTime(DatePat::Iso, TimePat::Hm)).unwrap(), "2024-02-05 14:05");
        assert_eq!(fmt(v, Dtype::Date, PLAIN, Show::DateTime(DatePat::Iso, TimePat::Hms)).unwrap(), "2024-02-05 14:05:09");
        assert_eq!(fmt(v, Dtype::Date, PLAIN, Show::DateTime(DatePat::Long, TimePat::H12)).unwrap(), "5 Feb 2024 2:05 PM");
        assert_eq!(fmt("2024-02-05", Dtype::Date, PLAIN, Show::DateTime(DatePat::Iso, TimePat::H12)).unwrap(), "2024-02-05 12:00 AM");
        assert_eq!(fmt("13/01/2024", Dtype::Date, DMY, Show::Date(DatePat::Iso)).unwrap(), "2024-01-13");
        assert_eq!(fmt("2024-02-29", Dtype::Date, PLAIN, Show::Date(DatePat::Long)).unwrap(), "29 Feb 2024");
        assert_eq!(fmt("1969-12-31T23:59:59Z", Dtype::Date, PLAIN, Show::DateTime(DatePat::Iso, TimePat::Hms)).unwrap(), "1969-12-31 23:59:59");
        assert_eq!(fmt("soon", Dtype::Date, PLAIN, Show::Date(DatePat::Iso)), None);
    }
    #[test]
    fn show_yes_no() {
        assert_eq!(fmt("TRUE", Dtype::Bool, PLAIN, Show::YesNo { ticks: false }).unwrap(), "Yes");
        assert_eq!(fmt(" false", Dtype::Bool, PLAIN, Show::YesNo { ticks: true }).unwrap(), "×");
        assert_eq!(fmt("maybe", Dtype::Bool, PLAIN, Show::YesNo { ticks: true }), None);
    }
    #[test]
    fn show_plain_or_a_format_that_does_not_fit_gives_the_file_text() {
        assert_eq!(fmt("12", Dtype::Int, PLAIN, Show::Plain), None);
        assert_eq!(fmt("12", Dtype::String, PLAIN, Show::Number { sep: true, places: None }), None);
        assert_eq!(fmt("2024-02-05", Dtype::Float, PLAIN, Show::Date(DatePat::Iso)), None);
        assert_eq!(fmt("true", Dtype::String, PLAIN, Show::YesNo { ticks: false }), None);
    }
    #[test]
    fn kind_sets_the_type_and_auto_finds_it_again() {
        // Every day and month is 12 or less: Auto reads DMY.
        let mut t = table("d\n01/02/2024\n03/04/2024\n02/01/2024", ",");
        let asc = |t: &Doc| t.scan().unwrap().sort(&all(t), &[k(0, Dir::Asc)]);
        assert_eq!((t.dtypes[0], t.reads[0].order), (Dtype::Date, Order::Dmy));
        assert_eq!(asc(&t), [2, 0, 1]);
        t.set_kind(0, Kind::Date { order: Order::Mdy });
        assert_eq!(asc(&t), [0, 2, 1]);
        t.set_kind(0, Kind::Text);
        assert_eq!(t.dtypes[0], Dtype::String);
        t.set_kind(0, Kind::Auto);
        assert_eq!((t.dtypes[0], t.reads[0].order), (Dtype::Date, Order::Dmy));
        assert_eq!(t.found_type(0).0, Dtype::Date);
        assert_eq!(t.sample_reads(0), (3, 3, false));
        t.set_kind(0, Kind::Date { order: Order::Ymd });
        assert_eq!(t.sample_reads(0), (0, 3, false));
        t.set_kind(0, Kind::Time);
        assert_eq!(t.sample_reads(0), (0, 3, false));
    }
    #[test]
    fn two_digit_years_read_after_a_type_set_by_hand() {
        let mut t = table("d\n01/02/03\n15/06/99\n31/12/68", ",");
        let asc = |t: &Doc| t.scan().unwrap().sort(&all(t), &[k(0, Dir::Asc)]);
        assert_eq!(t.dtypes[0], Dtype::String);
        t.set_kind(0, Kind::Date { order: Order::Dmy });
        assert_eq!(t.sample_reads(0), (3, 3, false));
        // 15 Jun 1999, 1 Feb 2003, 31 Dec 2068
        assert_eq!(asc(&t), [1, 0, 2]);
        assert_eq!(fmt("15/06/99", Dtype::Date, t.reads[0], Show::Date(DatePat::Iso)).unwrap(), "1999-06-15");
        t.set_kind(0, Kind::Date { order: Order::Ymd });
        assert_eq!(t.sample_reads(0), (1, 3, false));
        t.set_kind(0, Kind::Auto);
        assert_eq!(t.dtypes[0], Dtype::String);
    }

    // 0.8.0: data profiling
    fn prof(t: &Doc, rows: Option<&[u32]>, limit: Option<usize>, c: usize, d: Distinct) -> Profile {
        t.scan().unwrap().profile(rows, limit, c, d, &AtomicBool::new(false))
    }
    #[test]
    fn zero_reads_as_the_sort_reads_it() {
        let comma = Reading { comma: true, ..PLAIN };
        let zero = |t, r, v| is_zero(t, r, v);
        assert!(zero(Dtype::Int, PLAIN, "0") && zero(Dtype::Int, PLAIN, "-0") && zero(Dtype::Float, PLAIN, "0.00") && zero(Dtype::Float, comma, "0,00"));
        assert!(zero(Dtype::Time, PLAIN, "0:00:00") && zero(Dtype::Time, PLAIN, "0:00"));
        assert!(!zero(Dtype::Float, PLAIN, "0.01") && !zero(Dtype::Time, PLAIN, "0:00:01") && !zero(Dtype::Int, PLAIN, "") && !zero(Dtype::Int, PLAIN, "x"));
        assert!(!zero(Dtype::String, PLAIN, "0") && !zero(Dtype::Bool, PLAIN, "false") && !zero(Dtype::Date, PLAIN, "1970-01-01"));
    }
    #[test]
    fn json_text_escapes_quotes_backslashes_and_controls() {
        assert_eq!(json_text("a\"b\\c\nd\te\u{1}é"), "\"a\\\"b\\\\c\\nd\\te\\u0001é\"");
        assert_eq!(json_text(""), "\"\"");
    }
    #[test]
    fn profile_json_keeps_each_type_s_own_keys() {
        let rows: [&[&str]; 4] = [&["Oslo", "1", "2024-01-31", "true"], &["Lima", "2", "2024-02-01", "false"], &["Oslo", "", "x", "true"], &["Oslo \"x\"", "4", "2024-03-01", ""]];
        let doc = typed(&[Dtype::String, Dtype::Int, Dtype::Date, Dtype::Bool], &rows);
        let j = profile_json(&doc, &[0, 1, 2, 3], None, Distinct::Exact);
        let cols: Vec<&str> = j.split("\n  {").skip(1).collect();
        assert!(j.starts_with("{\"file\": \"t\", \"rows\": 4, \"total_rows\": 4, \"distinct\": \"exact\", \"columns\": [") && j.ends_with("}\n]}"), "{j}");
        assert_eq!(cols.len(), 4);
        assert!(
            cols[0].contains("\"type\": \"string\"")
                && cols[0].contains("{\"value\": \"Oslo\", \"count\": 2}")
                && cols[0].contains("\"Oslo \\\"x\\\"\"")
                && !cols[0].contains("not_readable")
                && !cols[0].contains("\"min\"")
        );
        assert!(cols[1].contains("\"empty\": 1") && cols[1].contains("\"min\": 1, \"max\": 4") && cols[1].contains("\"sum\": 7") && cols[1].contains("\"histogram\": ["));
        assert!(cols[2].contains("\"not_readable\": 1") && cols[2].contains("\"earliest\": \"2024-01-31\"") && cols[2].contains("\"latest\": \"2024-03-01\""));
        assert!(cols[3].contains("\"true\": 2, \"false\": 1") && cols[3].contains("\"empty\": 1"));
        assert_eq!(profile_json(&doc, &[1], Some(2), Distinct::Approx).split(", \"columns\"").next(), Some("{\"file\": \"t\", \"rows\": 2, \"total_rows\": 4, \"distinct\": \"approx\""));
    }
    #[test]
    fn profile_counts_and_number_statistics() {
        let t = one_col(Dtype::Int, &["1", "2", "2", "", "x", "3"]);
        let p = prof(&t, None, None, 0, Distinct::Exact);
        assert_eq!((p.rows, p.empty, p.unreadable, p.distinct, p.approx), (6, 1, 1, 4, false));
        let s = p.num.unwrap();
        assert_eq!((s.min, s.max, s.sum, s.mean, s.median), (1.0, 3.0, 8.0, 2.0, 2.0));
        assert!((s.std - (2.0f64 / 3.0).sqrt()).abs() < 1e-12);
        let odd = prof(&one_col(Dtype::Float, &["5", "1.5", "3"]), None, None, 0, Distinct::Exact).num.unwrap();
        // Mean 19/6; squared deviations sum to 37/6; over n - 1 = 2: 37/12.
        assert_eq!(odd.median, 3.0);
        assert!((odd.std - (37.0f64 / 12.0).sqrt()).abs() < 1e-12, "{}", odd.std);
    }
    #[test]
    fn profile_text_lengths_and_top_values() {
        let t = one_col(Dtype::String, &["Oslo", "Lima", "Oslo", "Rome", "Oslo", "Lima", "Accra"]);
        let p = prof(&t, None, None, 0, Distinct::Exact);
        // Exact ties go by first appearance: Rome comes before Accra.
        assert_eq!(p.top, [("Oslo".to_string(), 3), ("Lima".to_string(), 2), ("Rome".to_string(), 1), ("Accra".to_string(), 1)]);
        assert_eq!((p.len.unwrap().0, p.len.unwrap().2), (4, 5));
        assert!((p.len.unwrap().1 - 29.0 / 7.0).abs() < 1e-12);
        assert_eq!((p.distinct, p.num, p.span), (4, None, None));
        // Accra and Rome appear once; Oslo and Lima repeat. First and last in code-point order.
        assert_eq!((p.unique, p.range.clone()), (Some(2), Some(("Accra".to_string(), "Rome".to_string()))));
        let a = prof(&t, None, None, 0, Distinct::Approx);
        assert_eq!((a.unique, a.range), (None, p.range));
        assert_eq!(prof(&one_col(Dtype::Int, &["3", "1"]), None, None, 0, Distinct::Exact).range, None);
    }
    #[test]
    fn exact_value_counts_match_a_plain_count_over_many_chunks() {
        // Repeats, values seen once, and empty cells, over several CHUNKs of rows.
        let v: Vec<String> = (0..3 * CHUNK + 777)
            .map(|i| {
                if i % 13 == 0 {
                    String::new()
                } else if i % 5 == 0 {
                    format!("u{i}")
                } else {
                    format!("v{}", i % 97)
                }
            })
            .collect();
        let refs: Vec<&str> = v.iter().map(String::as_str).collect();
        let t = one_col(Dtype::String, &refs);
        let got = t.scan().unwrap().values(None, None, 0, Distinct::Exact, &AtomicBool::new(false));
        let mut counts: HashMap<&str, (usize, usize)> = HashMap::new();
        for (i, x) in refs.iter().enumerate().filter(|(_, x)| !x.is_empty()) {
            counts.entry(x).or_insert((0, i)).0 += 1;
        }
        let mut want: Vec<(&str, (usize, usize))> = counts.iter().map(|(k, v)| (*k, *v)).collect();
        want.sort_by_key(|(_, (n, first))| (std::cmp::Reverse(*n), *first));
        let top: Vec<(String, usize)> = want.iter().take(10).map(|(k, (n, _))| (k.to_string(), *n)).collect();
        assert_eq!((got.distinct, got.unique, got.top, got.approx), (counts.len(), Some(counts.values().filter(|c| c.0 == 1).count()), top, false));
        // The rows and the limit apply, as in `basic`.
        let some = t.scan().unwrap().values(Some(&[1, 2, 3, 98]), Some(3), 0, Distinct::Exact, &AtomicBool::new(false));
        assert_eq!((some.distinct, some.unique), (3, Some(3)));
    }
    #[test]
    fn profile_dates_span_and_histogram() {
        let t = one_col(Dtype::Date, &["2024-01-01", "2024-01-11", "2024-01-21 06:00"]);
        let p = prof(&t, None, None, 0, Distinct::Exact);
        let (lo, hi) = p.span.unwrap();
        assert_eq!((date_label(lo as i64), date_label(hi as i64)), ("2024-01-01".to_string(), "2024-01-21 06:00:00".to_string()));
        assert_eq!((p.hist.len(), p.hist[0], p.hist[9], p.hist[19], p.hist.iter().sum::<usize>()), (BINS, 1, 1, 1, 3));
        assert_eq!(p.num, None);
    }
    #[test]
    fn profile_follows_the_rows_and_the_limit() {
        let t = one_col(Dtype::Int, &["10", "20", "30", "40", "50"]);
        assert_eq!(prof(&t, Some(&[1, 3]), None, 0, Distinct::Exact).num.unwrap().sum, 60.0);
        assert_eq!(prof(&t, None, Some(2), 0, Distinct::Exact).num.unwrap().sum, 30.0);
        assert_eq!(prof(&t, Some(&[4, 0, 2]), Some(2), 0, Distinct::Exact).rows, 2);
    }
    #[test]
    fn hyperloglog_estimates_within_two_percent() {
        let mut h = Hll::new();
        for i in 0..100_000 {
            h.add(hash64(format!("v{i}").as_bytes()));
        }
        assert!((h.count() as f64 / 100_000.0 - 1.0).abs() < 0.02, "{}", h.count());
        let mut small = Hll::new();
        for i in 0..10 {
            small.add(hash64(format!("v{i}").as_bytes()));
        }
        assert_eq!(small.count(), 10);
    }
    #[test]
    fn approximate_profile_keeps_the_common_values() {
        let v: Vec<String> = (0..3000)
            .map(|i| {
                if i % 3 == 0 {
                    "a".into()
                } else if i % 6 == 1 {
                    "b".into()
                } else {
                    format!("u{i}")
                }
            })
            .collect();
        let t = one_col(Dtype::String, &v.iter().map(String::as_str).collect::<Vec<_>>());
        let exact = prof(&t, None, None, 0, Distinct::Exact);
        assert_eq!((exact.distinct, &exact.top[..2]), (1502, &[("a".to_string(), 1000), ("b".to_string(), 500)][..]));
        let est = prof(&t, None, None, 0, Distinct::Approx);
        assert!(est.approx && (est.distinct as f64 / 1502.0 - 1.0).abs() < 0.02, "{}", est.distinct);
        assert_eq!((est.top[0].0.as_str(), est.top[1].0.as_str()), ("a", "b"));
        assert!(est.top[0].1 >= 990 && est.top[0].1 <= 1000, "{}", est.top[0].1);
    }
}
