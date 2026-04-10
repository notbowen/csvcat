# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo build                        # debug build
cargo build --release              # optimized release build
cargo run -- <file.csv>            # run interactive viewer
cargo run -- --print <file.csv>    # print to stdout (no TUI)
cargo run -- -d ';' <file.csv>     # custom delimiter
cargo test                         # run tests
cargo clippy                       # lint
```

## Architecture

Four modules:

- **`reader.rs`** — `load_csv(path, delimiter)` → `CsvData { headers, rows }`. Uses `memmap2` with `MADV_SEQUENTIAL` for fast I/O. Reuses a single `StringRecord` allocation per row.
- **`table.rs`** — `TableLayout::compute(data, max_col_width)` → per-column display widths (unicode-aware, capped at `max_col_width`, min 1).
- **`viewer.rs`** — two entry points:
  - `run_viewer` — interactive TUI using crossterm raw mode + alternate screen
  - `print_table` — render directly to stdout
  - Internal `VisibleCols` struct computes which columns fit given `col_offset` and terminal width. Each cell occupies `width + 3` chars (`' ' + content + ' ' + '│'`). Frame overhead is 5 lines (top border + header + mid sep + bottom border + status bar).
- **`main.rs`** — clap `derive` CLI; dispatches to viewer or printer.

## Key details

- Crate name is `csv` (conflicts with the `csv` crate on crates.io), so the dependency is aliased: `csv-parser = { package = "csv", version = "1" }`. Use `csv_parser::` in code.
- `crossterm` has `use-dev-tty` feature enabled so key events work when stdin is a pipe.
- Terminal attribute resets use `SetAttribute(Attribute::Reset)` (not `ResetColor`) to clear both color and style.
- Zebra stripes use `Attribute::Dim` on odd `display_idx` rows (index into the visible window, not the data).
- `fit_cell` pads short cells with spaces and truncates long ones with `'…'`, targeting exact display-column width via `unicode-width`.
