# Changelog

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
