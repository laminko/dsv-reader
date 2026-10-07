# Changelog

## 0.10.0 — 2026-10-07

New:

- Column types Date & Time, for a date with a time of day, and Time, for `h:mm`, `h:mm:ss`, and `h:mm:ss.ffffff` (a duration or a time of day). The app detects both, and both are in the column menu's Type list.
- A time column sorts, filters (=, ≠, >, ≥, <, ≤, between), and profiles by its value. It shows as a duration (`1:02:03` or `1h 2m 3s`) or a time of day (`14:05`, `14:05:09`, `2:05 PM`).
- More date and time forms: month names (`5 Feb 2024`, `Feb 5, 2024`), compact dates (`20230505T090548Z`), 12-hour times (`2:05 PM`), and a zone on a time alone (`09:05:48+00`). The app ignores the zone. A 2-digit year (`01/02/03`) reads after you set the type by hand and select the order in "Read as".
- The column menu shows how the app reads the file, with example values: "Read as" `31/01/2024`, `01/31/2024`, or `2024-01-31` for dates, and `1.5` or `1,5` for numbers. When a type that you set reads only some cells, a line gives the count ("Reads 0 of 744 values"). The other cells show as in the file.
- Profile from the command line: `dsv-reader profile FILE` writes the Profile as JSON, with no window. Options: `--columns`, `--first N`, `--approx`. Exit codes: 0, 1 (a bad option or column), 2 (the file cannot be read).
- Profile one column: "Profile this column" in the column menu, and "None" beside "All columns" in the Profile's Columns menu.
- Zebra rows, on by default (Font and size → Zebra rows). Dates show in teal, and zeros in grey. The row numbers are on a lighter grey than the column names.
- A softer Dark theme: less glare, no pure black or white. The title bar now follows the app's theme, not only the Mac's.
- A click on a field in the Record viewer selects that cell, so ⌘C copies its value. A drag over part of the text, then ⌘C, copies only that part.
- The Record viewer can be below the grid while the Profile stays on the right. Each keeps its own size.

Faster:

- The Profile computes only the cards on the screen and the next card. The other cards compute when you scroll to them. On a 1.1 GB file with 20 columns, the first screen is ready in about 0.7 s.
- A file that is not in the file cache indexes about 3 times faster: 0.92–0.96 s, not 2.8–3.2 s, for 1.1 GB. The first rows still show in about 3 ms.
- A first sort of a text column uses about 40 % less memory: 326 MB, not 544 MB, for the email column of a 1.1 GB file. The order does not change.

Fixed:

- A row number of 7 digits or more showed cut with "…" (for example, "1,999,9…").
- A date with a space before its time zone, as `2026-01-12 04:40:00.587 +0700`, was read as text.
- A date and time with an hour-only zone, as `2023-05-05 09:05:48+00`, was read as text.
- A long field in the Record viewer now scrolls on its own. Before, the wheel scrolled the whole record too.
- "1 rows", "1 characters", and other counts of one now read "1 row", "1 character".

## 0.9.0 — 2026-10-06

The first public release.

- A native macOS app in Rust, on gpui-kit. A 1.1 GB file (6,550,000 rows × 20 columns) shows its first rows in about 31 ms; the full index takes about 207 ms.
- Delimiter, header row, and column types are detected. Twelve encodings.
- Search in all columns or some. Filters by column type (for text: contains, equals, is exactly, starts with, empty), joined with "and" or "or". A sort on several columns; text sorts with numbers in number order.
- Select cells, rows, or columns, and copy them as tab-separated text. Go to row.
- A Record viewer: the whole active cell, or the active row as a form. Its text can be selected and copied.
- A Profile: a table of statistics for each column, and its value distribution, over all rows (the first 1,000 above 100,000 rows). A click on a value or a bar filters to its rows.
- Column types and cell formats. Move, resize, fit, and hide columns.
- Save writes the column order and the columns you keep; Export writes the rows you see as CSV.
- Finder opens `.csv .tsv .tab .psv .dsv .ssv` with the app bundle. A second file asks "Open in". A quit asks first when the column order is not saved.
- System, Light, and Dark themes. A font menu for the grid. A narrow window moves toolbar controls into a ⋯ menu.
