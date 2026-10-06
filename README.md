# DSV Reader

A native macOS app in Rust. It opens CSV, TSV, and other delimited files, and it is fast on large files.
It is a port of [bb-plugin-dsv](https://github.com/laminko/bb-plugin-dsv) 0.10.1. Detection and type rules are the same.
The window is built on [gpui-kit](https://crates.io/crates/gpui-kit) 0.7.1 (0.9.0; egui before).

![DSV Reader with a synthetic 1.1 GB CSV file of 6,550,000 rows × 20 columns: a search for "oslo" shows 818,353 rows, Go to row 2,000,000 marked row 1,999,996, and the Viewer pane shows the active cell](screenshot.png)

The image shows a synthetic file from `examples/gen.rs`. The names, cities, and `@example.com` emails are not real.

## Speed

Release build, Apple Silicon Mac, macOS 27.0.1, warm file cache, 2026-10-06.

| The app, 1.1 GB file, 6,550,000 × 20 (3 runs) | Result |
|---|---|
| Open → first rows | 30.8–34.9 ms |
| Full index | 206–208 ms |
| Draw a page scroll, p95 (28 rows a frame, 60 Hz display) | 4.9–5.1 ms |
| Peak memory footprint, open and scroll | 199 MB |

The plugin took 297–833 ms to the first paint of a 100,000 × 20 file.

Search, filters, and sort on the 1.1 GB file (`bench.rs --query`, worst of 2 runs):

| Operation | Time |
|---|---|
| Search all columns | 516 ms |
| Search one column | 117 ms |
| Filter `age > 50` | 191 ms |
| First sort of a number column | 402 ms |
| First sort of a text column | 527 ms |
| Sort again (ranks cached) | 177 ms |
| Copy 10,000 rows × 20 columns as TSV | 23 ms |
| Go to row 6,000,000 in sorted rows | 0.47–0.48 ms |
| Export every row (1,101.7 MB) | 1,739 ms |

A scan runs on its own thread. A first text sort alone peaks at 569 MB (`bench.rs --sort 3`, the email
column); the whole `--query` run peaks at 945 MB with the export.

The grid paints only the rows and columns on screen. At 1,000 columns, open → first rows took 72.4–77.7 ms.
Most of that is the header and type detection on the first 1,001 rows.

Profile of all 20 columns × 6,550,000 rows: Approximate 7.7 s, first card at 0.6 s, peak 297 MB; Exact
11.8 s, first card at 0.7 s, peak 352 MB. The longest gap between frames was 27.6 and 34.3 ms.

How:

1. The file is mapped, not read. A UTF-8 file is never copied.
2. One pass with `memchr` finds the start of each record. It keeps 8 bytes per row.
3. The index runs on a thread. The grid shows rows from the first batch.
4. Only the rows on screen are parsed.

## Features

- **Open:** a path on the command line, drag and drop, ⌘O, a double-click on the empty window, or from Finder with the app bundle (double-click, Open With, a drop on the Dock icon). With a file already open, Open, a drop, or Finder asks "Open in": Current window, or New window (a new instance of the app).
- **Delimiter:** detected from `,` tab `;` `|` and the file extension. You can pick one, or type any single character.
- **Header row:** Auto, On, or Off. The status bar shows the reason for Auto.
- **Typed columns:** int, float, bool, date, or string, from the first 1,000 rows. Numbers align right.
- **Format:** text qualifier (`"`, `'`, none), and encoding (12 choices). Decimal symbol and date order are found per column.
- **Column type and cell format:** right-click a column name. Type: Auto (what detection found), Text, Number (with its decimal symbol), Date (with its day and month order), or Yes-No. Format: Number (thousands separator, decimals), Percent, Currency (symbol, decimals), Duration (the unit of the cell; 1:02:03 or 1h 2m 3s), Date (4 patterns), Date & Time (plus 14:05, 14:05:09, or 2:05 PM), Yes-No (Yes/No or ✓/×). Formats change only the screen: Copy and Export write the file's text. They reset when the file closes.
- **Encoding:** Auto reads a UTF-16 BOM, else UTF-8. The status bar counts the characters that could not be read.
- **Font:** a menu of the system font files and a size box (8–32 pt), for the grid. Kept after a restart. Thai, Myanmar, Chinese, and Korean fall back to system fonts.
- **Search** in all columns, or only in the columns you pick. Case is ignored. ⌘F focuses it.
- **Filters:** add as many as you need. The operators follow the column type; for text: contains, equals (case ignored), is exactly (case kept), starts with, is empty, not empty. Rows join with "and" or "or" (click the word): "and" binds first, so `a and b or c` is `(a and b) or c`.
- **Sort:** click the arrow in a column name for ascending, descending, then off. Shift+click adds the column as the next sort level.
- **Columns:** show or hide each column (Columns menu). Drag a column edge to resize it. Double-click the edge to fit the column to the longest values on screen and its name. Drag a column name to move the column; a line shows where it lands. "Reset order" puts the file order back.
- **Select and copy, as in a spreadsheet:** cells, rows (click the row numbers), or columns (click a name). Copy gives tab-separated text, with or without the column names, up to 1,000,000 rows.
- **Viewer:** a pane on the right of the grid or below it. The Cell tab shows the whole active cell and its character count. The Record tab shows the active row as a form; First, Previous, Next, and Last stay above the form. Its text can be selected and copied (⌘C). The side and the size are kept.
- **Go to row:** moves a row number to the top, marks it, and makes it the active row. In filtered rows, it goes to the nearest row above it.
- **Export:** writes the rows you see (search, filters, and sort applied) and the shown columns as a UTF-8 CSV file. The open file does not change.
- **Save:** writes the file itself. It asks first: overwrite the open file, or write a new file whose name you type, in the same folder. It writes every row, in file order, with the columns you see in the order you see. It keeps the delimiter, the record delimiter, and the text qualifier. It refuses when the file changed on disk after it was read (size or modified time), and when the new name is in use. The status bar shows "modified" until a Save writes the order you see. A quit with a modified order asks first.
- **Profile:** a pane with a statistics table per column. Every type: count, empty, not empty, distinct, unique (seen once), repeated, and the most common value. Text: min and max in code-point order, shortest, longest, and mean length. Numbers: min, max, mean, median, standard deviation, sum. Dates: earliest, latest, and the span. Yes-No: true and false. Then the value distribution: the top 10 values as bars, or a histogram of 20 bars for numbers and dates. A click on a value ("is exactly") or a bar ("between" its first and last value) adds the filter for its rows; over all rows, the filter keeps exactly the rows the bar counts.
  Rows: all (the default up to 100,000 rows) or the first 1,000 (the default above), with or without the search and filters. A profile of the first 1,000 rows says so on its counts, and a click on it filters all rows. Distinct: Exact or Approximate; the pane marks the recommended one (Approximate above 1,000,000 rows). A Columns menu picks the columns.
  Two threads fill the cards: the basic statistics first, then the Exact value counts, one column at a time ("…" until they come). With the Viewer also on, the side pane splits in two; drag the line between them.
- **Toolbar:** one row. A narrow window moves the controls from the right end into a ⋯ menu; then the parsing pill shows only the delimiter. The window is at least 640 × 400.
- **Theme:** System (the default; it follows macOS), Light, or Dark. Kept after a restart.
- **Status bar:** rows, index progress, columns, delimiter, record delimiter, encoding, and times.

## Keys

| Key | Action |
|---|---|
| ⌘O | Open a file |
| ⌘F | Focus Search |
| ⌘G | Focus Go to row |
| ⌘S | Export the rows you see as a CSV file |
| ⌘Q | Quit (asks first when the column order is not saved) |
| Arrows / Shift+arrows | Move the active cell / extend the selection |
| Page Up / Page Down | Move one screen |
| ⌘A | Select all |
| ⌘C | Copy the selection |
| Esc | Clear the selection |
| Click a sort arrow | Sort: ascending, descending, off |
| Shift+click a sort arrow | Add a sort level; again: descending; again: remove it |
| Click / drag a column name | Select the column / move it |

## Build and run

Rust 1.95 or later, on macOS.

```sh
brew install rustup && rustup default stable   # rustup is keg-only: add /opt/homebrew/opt/rustup/bin to PATH
cargo build --release
target/release/dsv-reader path/to/file.csv
```

## Install as a Mac app

Download `DSV-Reader-<version>-macos-arm64.zip` from the GitHub Releases page, unzip it, and drag the app to Applications. It is ad-hoc signed, not notarized: the first time, right-click the app and choose Open. Or build it:

```sh
./bundle.sh        # builds target/release/DSV Reader.app with its icon (ad-hoc signed); installs nothing
                   # it also gives the bare binary target/release/dsv-reader the icon in Finder
```

Drag the app to Applications. Finder then lists it in Open With for `.csv .tsv .tab .psv .dsv .ssv`.
It does not take the default from Numbers or Excel. For `.tab`, `.psv`, `.dsv`, and `.ssv`, which no other
app on this Mac claims, macOS makes it the default. Get Info → Open with → Change All sets the default.

## Develop

```sh
cargo test                                                 # 89 tests: 81 in lib (59 from logic.test.ts), 8 in the app
cargo run --release --example gen -- big.csv 6550000      # a 1.1 GB synthetic file
cargo run --release --example bench -- big.csv 3 --check  # index times, and index count = parse count
cargo run --release --example bench -- big.csv 1 --query  # search, filter, sort, copy, and export times
cargo run --release --example bench -- big.csv 0 --sort 3 # one sort alone, for its peak memory with /usr/bin/time -l
cargo run --release --example bench -- big.csv 0 --profile approx  # profile every column (exact; or add a column number)
DSV_BENCH=1 target/release/dsv-reader big.csv             # open → first rows, then quit
DSV_BENCH=1 DSV_SCROLL=1 target/release/dsv-reader big.csv # scroll one page a frame, 300 frames: draw times
```

`rustfmt.toml` keeps lines up to 200 characters. CI (`.github/workflows/ci.yml`, macOS) runs `cargo fmt --check`, `cargo clippy -- -D warnings`, the tests, and a build check with Rust 1.95.

No Xcode is needed. The Metal shaders compile when the app starts (`gpui-pre-platform` feature `runtime_shaders`). The first build compiles about 450 crates.

`src/lib.rs` holds the parser, the index, detection, search, filters, sort, copy, and the profile, with no GUI. `src/main.rs` is the window. `src/grid.rs` is the grid, painted by hand (gpui-kit's own table drew a page scroll in p95 21.7–21.9 ms). `src/toolbar.rs` is the toolbar and its popovers and dialogs. `src/pane.rs` is the Record viewer, the Profile, and the filter rows. `src/menu.rs` is the column menu. `src/file.rs` holds the settings, Save, and Export. `src/mac.rs` adds the quit question to gpui's app delegate and gives the bare binary its Dock icon.

Bench hooks (with `DSV_BENCH=1`):

| Variable | What it does |
|---|---|
| `DSV_SCROLL=1` | Scroll one page a frame, 300 frames, in a 1600 × 1000 window; `DSV_FRAMES` sets more |
| `DSV_SEARCH=text` | Search, then print the scan time and rows |
| `DSV_SORT=column` | Sort on that column, with the search |
| `DSV_GOTO=row` | Go to that row, then print the time |
| `DSV_COPY=rows` | Time Copy of the first rows, all columns, as TSV; the clipboard is left alone |
| `DSV_PROFILE=1000\|all\|auto` | Open the Profile, wait for it, and print its time, the first card's time, and the longest frame gap; `DSV_DISTINCT=exact\|approx` sets its choice |
| `DSV_TOP=4`, `4,8`, `4\|8`, `9:3` | After `DSV_PROFILE`: filter on column 4's most common value as a click does; one per column joined by and (`,`) or or (`\|`); `9:3` is bar 3 of column 9 |
| `DSV_HOLD=1` | Keep the window open after the report, for a screenshot |

A window behind other windows gets throttled frames from macOS, and a window on another Space gets none: the benches wait until the window is in front.

Without `DSV_BENCH`:

| Variable | What it does |
|---|---|
| `DSV_THEME=light\|dark` | Start in that theme without saving it |
| `DSV_SIZE=WxH` | Start at that window size |
| `DSV_VIEWER=0` | Start with the Record viewer closed |
| `DSV_OPEN=parse\|scope\|columns\|font\|save` | Open that popover or the Save dialog, for a screenshot |
| `DSV_PICK=path`, `DSV_OPEN_IN=current\|new` | After the first rows, send a file as Open would, and answer "Open in" |
| `DSV_MODIFY=1` | Hide the last column, so a quit asks first |

## Limits

- No cell editing. Save writes the column order and the columns you keep, not new values.
- Save writes UTF-8 and leaves a hidden column out of the file. A Save over the open file writes a new file and renames it: the permissions and extended attributes of the old file are not copied.
- Save finds a change on disk by size and modified time. A change that keeps both is not found.
- Export and Save write a cell's bytes as they are. A byte that is not UTF-8 stays as it is.
- A double-click on the side pane's edge does nothing.
- The exact column count comes from a width pass after the index: 316–362 ms on the 1.1 GB file. Until then, it comes from the first 1,001 rows.
- Text sort puts digit runs in number order and ignores case. It does not fold accents, and punctuation sorts by code point. The plugin used the ICU collator.
- A first sort of a text column needs memory: 569 MB peak footprint for the email column of the 1.1 GB file.
- Exact distinct counts compare 64-bit hashes: for 6.5 million different values, about 1 chance in a million that two count as one.
- A file in an encoding other than UTF-8 is decoded before the first paint.
- The file loads once. If it shrinks on disk while open, the app can crash (the file is mapped).
- On close, the app exits before the window is destroyed. This avoids an AppKit Touch Bar crash on macOS.
- A cold file can index slowly: one run took 2,834 ms for 1.1 GB.
- One window shows one file. "New window" starts a new instance of the app, with its own Dock icon.
- The bundle is ad-hoc signed, not notarized: on another Mac, Gatekeeper asks before it opens.
- macOS only. Windows and Linux are not tested.

## License

MIT: see [LICENSE](LICENSE). The IBM Plex Sans fonts in `assets/fonts` are under the SIL Open Font License 1.1 ([OFL-IBMPlexSans.txt](assets/fonts/OFL-IBMPlexSans.txt)).
