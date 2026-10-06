//! Opens and indexes a file with no window, RUNS times, and prints the times.
//! --check: parses every record and compares the count with the index.
//! --query: times search, a filter, and sorts on the done index.
//! --sort COL: one sort on column COL alone, for its peak memory (`/usr/bin/time -l`).
//!
//! cargo run --release --example bench -- PATH [RUNS] [--check] [--query] [--sort COL]

use dsv_reader::{Dir, Distinct, Doc, Filter, Op, Opts, Sel, SortKey, parse_record, to_tsv};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

fn wait(doc: &Doc) {
    while !doc.index.done.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = std::path::Path::new(&args[1]);
    let runs: usize = args.get(2).and_then(|r| r.parse().ok()).unwrap_or(3);
    let size = std::fs::metadata(path)?.len();
    println!("{} · {:.1} MB", path.display(), size as f64 / 1e6);
    for run in 1..=runs {
        let t = Instant::now();
        let doc = Doc::open(path, &Opts::default(), || {})?;
        let opened = t.elapsed();
        while doc.rows() == 0 && !doc.index.done.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_micros(100));
        }
        let first = t.elapsed();
        wait(&doc);
        let all = t.elapsed();
        let ms = doc.index.ms.load(Ordering::Relaxed);
        println!(
            "run {run}: open {:.1} ms · first batch {:.1} ms · index {ms} ms ({:.2} GB/s) · done {:.0} ms · {} rows × {} cols · delim {:?} · header {} ({}) · bad {}",
            opened.as_secs_f64() * 1e3,
            first.as_secs_f64() * 1e3,
            size as f64 / 1e6 / ms.max(1) as f64,
            all.as_secs_f64() * 1e3,
            doc.rows(),
            doc.names.len(),
            doc.delim,
            doc.header,
            doc.reason,
            doc.index.bad.load(Ordering::Relaxed),
        );
    }
    if args.iter().any(|a| a == "--check") {
        let doc = Doc::open(path, &Opts::default(), || {})?;
        wait(&doc);
        let (b, d, q) = (&doc.data[..], doc.delim.as_bytes(), doc.quote.as_bytes());
        let (mut i, mut kept) = (if b.starts_with(b"\xEF\xBB\xBF") { 3 } else { 0 }, 0usize);
        while i < b.len() {
            let (row, next) = parse_record(b, i, d, q);
            if row.len() > 1 || !row[0].is_empty() {
                kept += 1;
            }
            i = next;
        }
        let indexed = doc.index.starts.lock().unwrap().len();
        println!("check: parse {kept} records · index {indexed} records · {}", if kept == indexed { "equal" } else { "DIFFERENT" });
    }
    if args.iter().any(|a| a == "--query") {
        let doc = Doc::open(path, &Opts::default(), || {})?;
        wait(&doc);
        let t = Instant::now();
        while doc.rows() > 0 && doc.index.width.load(Ordering::Relaxed) == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
        println!("width pass: {} columns, {:.0} ms after done", doc.index.width.load(Ordering::Relaxed), t.elapsed().as_secs_f64() * 1e3);
        let scan = doc.scan().unwrap();
        let stop = AtomicBool::new(false);
        let time = |label: &str, f: &dyn Fn() -> usize| {
            let t = Instant::now();
            let n = f();
            println!("{label}: {n} in {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
        };
        time("search all columns \"oslo\"", &|| scan.query("oslo", &[], &[], &stop).len());
        time("search all columns \"zzzz\"", &|| scan.query("zzzz", &[], &[], &stop).len());
        time("search column 4 \"oslo\"", &|| scan.query("oslo", &[4], &[], &stop).len());
        let age = Filter { col: 6, op: Op::Gt, a: "50".into(), b: String::new(), or: false };
        time("filter age > 50", &|| scan.query("", &[], std::slice::from_ref(&age), &stop).len());
        let all: Vec<u32> = (0..scan.rows() as u32).collect();
        for (col, name) in [(7, "score"), (7, "score, cached"), (3, "email"), (3, "email, cached")] {
            time(&format!("sort {name}"), &|| scan.sort(&all, &[SortKey { col, dir: Dir::Asc }]).len());
        }
        let cols: Vec<usize> = (0..20).collect();
        let s = Sel { ar: 0, ac: 0, fr: 9_999, fc: 19 };
        time("copy 10,000 × 20 as TSV, bytes", &|| to_tsv(&doc.cells(None, &cols, s, true)).len());
        let out = path.with_file_name("dsv-reader-export.csv");
        let all_cols: Vec<usize> = (0..doc.names.len()).collect();
        let t = Instant::now();
        let mut w = std::io::BufWriter::new(std::fs::File::create(&out)?);
        scan.write(&mut w, None, &all_cols, ",", "\n", "\"")?;
        std::io::Write::flush(&mut w)?;
        drop(w);
        let mb = std::fs::metadata(&out)?.len() as f64 / 1e6;
        println!("export every row: {:.0} ms, {mb:.1} MB", t.elapsed().as_secs_f64() * 1e3);
        let back = Doc::open(&out, &Opts::default(), || {})?;
        wait(&back);
        let step = (doc.rows() / 1000).max(1);
        let same = back.rows() == doc.rows() && back.names == doc.names && (0..doc.rows()).step_by(step).all(|i| back.row(i) == doc.row(i));
        println!("export check: {} rows back, names and every {step}th row {}", back.rows(), if same { "equal" } else { "DIFFERENT" });
        drop(back);
        std::fs::remove_file(&out)?;
    }
    if let Some(col) = args.iter().position(|a| a == "--sort").and_then(|i| args.get(i + 1)?.parse::<usize>().ok()) {
        let doc = Doc::open(path, &Opts::default(), || {})?;
        wait(&doc);
        let scan = doc.scan().unwrap();
        let all: Vec<u32> = (0..doc.rows() as u32).collect();
        let t = Instant::now();
        let sorted = scan.sort(&all, &[SortKey { col, dir: Dir::Asc }]);
        let firsts: Vec<String> = sorted.iter().take(3).map(|&r| doc.row(r as usize)[col].clone()).collect();
        println!("sort {}: {} rows in {:.0} ms · first {firsts:?}", doc.names[col], sorted.len(), t.elapsed().as_secs_f64() * 1e3);
    }
    if let Some(i) = args.iter().position(|a| a == "--profile") {
        let distinct = if args.get(i + 1).map(String::as_str) == Some("exact") { Distinct::Exact } else { Distinct::Approx };
        let doc = Doc::open(path, &Opts::default(), || {})?;
        wait(&doc);
        let scan = doc.scan().unwrap();
        let stop = AtomicBool::new(false);
        let t = Instant::now();
        let only: Option<usize> = args.get(i + 2).and_then(|c| c.parse().ok());
        for c in (0..doc.names.len()).filter(|c| only.is_none_or(|o| o == *c)) {
            let tc = Instant::now();
            let p = scan.profile(None, None, c, distinct, &stop);
            println!("profile {:?} column {c} {}: {} rows, distinct {}, {:.0} ms", distinct, doc.names[c], p.rows, p.distinct, tc.elapsed().as_secs_f64() * 1e3);
        }
        println!("profile {:?}: {} columns in {:.0} ms", distinct, doc.names.len(), t.elapsed().as_secs_f64() * 1e3);
    }
    Ok(())
}
