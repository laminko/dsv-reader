//! A synthetic CSV: a header and ROWS rows × 20 columns, the shape of the
//! plugin's 100,000-row sample. Names, cities, and emails are not real.
//! Some notes are quoted: with a comma, a doubled quote, or a line break.
//!
//! cargo run --release --example gen -- PATH ROWS

use std::io::{BufWriter, Write};

const FIRST: [&str; 8] = ["Ann", "Bob", "Cid", "Dee", "Eve", "Fay", "Gus", "Hal"];
const LAST: [&str; 8] = ["Lee", "Ray", "Moss", "Kent", "Diaz", "Nakamura", "Okafor", "Silva"];
const CITY: [&str; 8] = ["Oslo", "Lima", "Rome", "Pune", "Accra", "Hanoi", "Quito", "Perth"];
const COUNTRY: [&str; 8] = ["NO", "PE", "IT", "IN", "GH", "VN", "EC", "AU"];
const DEPT: [&str; 5] = ["Sales", "Ops", "R&D", "Legal", "Support"];

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (path, rows): (&str, u64) = (&args[1], args[2].parse().expect("ROWS is a number"));
    let mut w = BufWriter::with_capacity(1 << 20, std::fs::File::create(path)?);
    writeln!(w, "id,first,last,email,city,country,age,score,active,joined,salary,phone,zip,dept,rating,visits,last_login,balance,verified,note")?;
    let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut rnd = move |n: u64| {
        // xorshift64
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s % n
    };
    for id in 1..=rows {
        let (f, l, c) = (rnd(8) as usize, rnd(8) as usize, rnd(8) as usize);
        let note = match rnd(20) {
            0 => "\"late, then early\"".to_string(),
            1 => "\"said \"\"ok\"\"\"".to_string(),
            2 if rnd(10) == 0 => "\"two\nlines\"".to_string(),
            _ => format!("note {}", rnd(1000)),
        };
        writeln!(
            w,
            "{id},{},{},{}.{}{}@example.com,{},{},{},{}.{:02},{},{}-{:02}-{:02},{}.{:02},+1-555-{:04},{:05},{},{}.{},{},{}-{:02}-{:02}T{:02}:{:02}:00Z,{}.{:02},{},{note}",
            FIRST[f],
            LAST[l],
            FIRST[f].to_lowercase(),
            LAST[l].to_lowercase(),
            id,
            CITY[c],
            COUNTRY[c],
            18 + rnd(60),
            rnd(100),
            rnd(100),
            rnd(2) == 0,
            2000 + rnd(25),
            1 + rnd(12),
            1 + rnd(28),
            20_000 + rnd(180_000),
            rnd(100),
            rnd(10_000),
            rnd(100_000),
            DEPT[rnd(5) as usize],
            1 + rnd(4),
            rnd(10),
            rnd(5000),
            2024 + rnd(2),
            1 + rnd(12),
            1 + rnd(28),
            rnd(24),
            rnd(60),
            rnd(1_000_000) as i64 - 500_000,
            rnd(100),
            rnd(2) == 0,
        )?;
    }
    w.flush()
}
