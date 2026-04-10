use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers},
    execute, queue,
    style::{Attribute, Color, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor},
    terminal::{self, ClearType},
};
use std::io::{self, BufWriter, Write};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::reader::CsvData;
use crate::table::TableLayout;

// ── public entry points ────────────────────────────────────────────────────

/// Enter the interactive `less`-like viewer.
pub fn run_viewer(data: &CsvData, max_col_width: usize, zebra: bool) -> anyhow::Result<()> {
    let layout = TableLayout::compute(data, max_col_width);
    let stdout = io::stdout();
    // 64 KiB write buffer — one syscall per frame on most terminals
    let mut out = BufWriter::with_capacity(64 * 1024, stdout.lock());

    terminal::enable_raw_mode()?;
    execute!(out, terminal::EnterAlternateScreen, cursor::Hide, terminal::DisableLineWrap)?;

    let result = event_loop(data, &layout, &mut out, zebra);

    // Always restore terminal state even on error
    let _ = execute!(out, terminal::EnableLineWrap, terminal::LeaveAlternateScreen, cursor::Show);
    let _ = terminal::disable_raw_mode();

    result
}

/// Render the table directly to stdout and exit.
pub fn print_table(data: &CsvData, max_col_width: usize, zebra: bool) -> anyhow::Result<()> {
    let layout = TableLayout::compute(data, max_col_width);
    let stdout = io::stdout();
    let mut out = BufWriter::with_capacity(64 * 1024, stdout.lock());
    let term_width = terminal::size().map(|(w, _)| w as usize).unwrap_or(120);

    let vis = VisibleCols::compute(&layout.col_widths, 0, term_width);
    if vis.is_empty() {
        return Ok(());
    }

    render_top_border(&mut out, &vis)?;
    render_header_row(&mut out, &data.headers, &vis)?;
    render_mid_border(&mut out, &vis)?;
    for (i, row) in data.rows.iter().enumerate() {
        render_data_row(&mut out, row, &vis, i, zebra)?;
    }
    render_bot_border(&mut out, &vis)?;
    out.flush()?;
    Ok(())
}

// ── event loop ─────────────────────────────────────────────────────────────

fn event_loop(
    data: &CsvData,
    layout: &TableLayout,
    out: &mut impl Write,
    zebra: bool,
) -> anyhow::Result<()> {
    let mut row_offset: usize = 0;
    let mut col_offset: usize = 0;
    let mut prev_size = (0u16, 0u16);

    loop {
        let (tw, th) = terminal::size()?;

        // Full clear only when the terminal is resized
        if (tw, th) != prev_size {
            queue!(out, terminal::Clear(ClearType::All))?;
            prev_size = (tw, th);
        }

        render_frame(
            data, layout, out,
            row_offset, col_offset,
            tw as usize, th as usize,
            zebra,
        )?;

        match event::read()? {
            Event::Key(key) => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('c') if ctrl => break,

                    KeyCode::Down  | KeyCode::Char('j') => {
                        let max = max_row_offset(data.rows.len(), th as usize);
                        if row_offset < max { row_offset += 1; }
                    }
                    KeyCode::Up    | KeyCode::Char('k') => {
                        row_offset = row_offset.saturating_sub(1);
                    }
                    KeyCode::Right | KeyCode::Char('l') => {
                        // Only scroll if the last visible column is not already the last column
                        let vis = VisibleCols::compute(&layout.col_widths, col_offset, tw as usize);
                        let last_visible = vis.indices.last().copied().unwrap_or(col_offset);
                        if last_visible + 1 < layout.col_widths.len() {
                            col_offset += 1;
                        }
                    }
                    KeyCode::Left  | KeyCode::Char('h') => {
                        col_offset = col_offset.saturating_sub(1);
                    }
                    KeyCode::PageDown => {
                        let step = visible_rows(th as usize);
                        let max = max_row_offset(data.rows.len(), th as usize);
                        row_offset = (row_offset + step).min(max);
                    }
                    KeyCode::PageUp => {
                        let step = visible_rows(th as usize);
                        row_offset = row_offset.saturating_sub(step);
                    }
                    KeyCode::Home | KeyCode::Char('g') => {
                        row_offset = 0; col_offset = 0;
                    }
                    KeyCode::End | KeyCode::Char('G') => {
                        row_offset = max_row_offset(data.rows.len(), th as usize);
                    }
                    _ => {}
                }
            }
            Event::Resize(_, _) => {} // handled at top of loop
            _ => {}
        }
    }

    Ok(())
}

// ── geometry helpers ───────────────────────────────────────────────────────

/// How many data rows fit on screen.
/// Frame overhead: top border + header + mid separator + bottom border + status = 5 lines.
fn visible_rows(term_height: usize) -> usize {
    term_height.saturating_sub(5)
}

fn max_row_offset(total_rows: usize, term_height: usize) -> usize {
    total_rows.saturating_sub(visible_rows(term_height))
}

// ── visible-column computation ─────────────────────────────────────────────

struct VisibleCols {
    /// Original column indices
    indices: Vec<usize>,
    /// Effective display widths (may be clamped for the last partial column)
    widths: Vec<usize>,
}

impl VisibleCols {
    fn compute(col_widths: &[usize], col_offset: usize, term_width: usize) -> Self {
        let mut indices = Vec::new();
        let mut widths = Vec::new();
        // -1 for the leading '│'
        let mut remaining = term_width.saturating_sub(1);

        for i in col_offset..col_widths.len() {
            // Each cell occupies: ' ' + content + ' ' + '│'  =  width + 3
            let needed = col_widths[i] + 3;

            if needed <= remaining {
                indices.push(i);
                widths.push(col_widths[i]);
                remaining -= needed;
            } else if remaining >= 4 {
                // Column doesn't fully fit: use remaining space so fit_cell adds '…'.
                indices.push(i);
                widths.push(remaining - 3);
                break;
            } else {
                break;
            }
        }

        Self { indices, widths }
    }

    fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

// ── frame renderer ─────────────────────────────────────────────────────────

fn render_frame(
    data: &CsvData,
    layout: &TableLayout,
    out: &mut impl Write,
    row_offset: usize,
    col_offset: usize,
    term_width: usize,
    term_height: usize,
    zebra: bool,
) -> anyhow::Result<()> {
    queue!(out, cursor::MoveTo(0, 0))?;

    let vis = VisibleCols::compute(&layout.col_widths, col_offset, term_width);
    if vis.is_empty() {
        out.flush()?;
        return Ok(());
    }

    let n_data = visible_rows(term_height);
    let end_row = (row_offset + n_data).min(data.rows.len());
    let rows_shown = end_row - row_offset;

    render_top_border(out, &vis)?;
    render_header_row(out, &data.headers, &vis)?;
    render_mid_border(out, &vis)?;

    for (i, row_idx) in (row_offset..end_row).enumerate() {
        render_data_row(out, &data.rows[row_idx], &vis, i, zebra)?;
    }

    // Blank rows so the bottom border always sits at a fixed position
    for fill_i in rows_shown..n_data {
        render_empty_row(out, &vis, fill_i, zebra)?;
    }

    render_bot_border(out, &vis)?;

    // ── status bar ────────────────────────────────────────────────────────
    let status = format!(
        " Rows {}-{}/{} │ Cols {}-{}/{} │ hjkl/arrows navigate │ PgUp/PgDn │ g/G home/end │ q quit ",
        if data.rows.is_empty() { 0 } else { row_offset + 1 },
        end_row,
        data.rows.len(),
        col_offset + 1,
        col_offset + vis.indices.len(),
        data.headers.len(),
    );

    queue!(
        out,
        cursor::MoveTo(0, (term_height - 1) as u16),
        // Reverse swaps the terminal's own fg/bg — works on any color scheme
        SetAttribute(Attribute::Reverse),
        SetAttribute(Attribute::Bold),
    )?;
    let display = truncate_to_width(&status, term_width);
    // Pad to fill the entire status line
    write!(out, "{:<width$}", display, width = term_width)?;
    queue!(out, SetAttribute(Attribute::Reset))?;

    out.flush()?;
    Ok(())
}

// ── border / row renderers ─────────────────────────────────────────────────

fn render_top_border(out: &mut impl Write, vis: &VisibleCols) -> anyhow::Result<()> {
    write!(out, "┌")?;
    for (i, &w) in vis.widths.iter().enumerate() {
        write!(out, "{}", "─".repeat(w + 2))?;
        write!(out, "{}", if i + 1 < vis.widths.len() { "┬" } else { "┐" })?;
    }
    write!(out, "\r\n")?;
    Ok(())
}

fn render_mid_border(out: &mut impl Write, vis: &VisibleCols) -> anyhow::Result<()> {
    write!(out, "├")?;
    for (i, &w) in vis.widths.iter().enumerate() {
        write!(out, "{}", "─".repeat(w + 2))?;
        write!(out, "{}", if i + 1 < vis.widths.len() { "┼" } else { "┤" })?;
    }
    write!(out, "\r\n")?;
    Ok(())
}

fn render_bot_border(out: &mut impl Write, vis: &VisibleCols) -> anyhow::Result<()> {
    write!(out, "└")?;
    for (i, &w) in vis.widths.iter().enumerate() {
        write!(out, "{}", "─".repeat(w + 2))?;
        write!(out, "{}", if i + 1 < vis.widths.len() { "┴" } else { "┘" })?;
    }
    write!(out, "\r\n")?;
    Ok(())
}

fn render_header_row(
    out: &mut impl Write,
    headers: &[String],
    vis: &VisibleCols,
) -> anyhow::Result<()> {
    write!(out, "│")?;
    for (&col_i, &w) in vis.indices.iter().zip(vis.widths.iter()) {
        let cell = headers.get(col_i).map(String::as_str).unwrap_or("");
        let fitted = fit_cell(cell, w);
        queue!(
            out,
            SetAttribute(Attribute::Bold),
            SetForegroundColor(Color::Cyan),
        )?;
        write!(out, " {} ", fitted)?;
        queue!(out, SetAttribute(Attribute::Reset))?;
        write!(out, "│")?;
    }
    write!(out, "\r\n")?;
    Ok(())
}

const ZEBRA_BG: Color = Color::AnsiValue(236);

fn render_data_row(
    out: &mut impl Write,
    row: &[String],
    vis: &VisibleCols,
    display_idx: usize,
    zebra: bool,
) -> anyhow::Result<()> {
    let is_alt = zebra && display_idx % 2 == 1;
    if is_alt {
        queue!(out, SetBackgroundColor(ZEBRA_BG))?;
    }
    write!(out, "│")?;
    for (&col_i, &w) in vis.indices.iter().zip(vis.widths.iter()) {
        let cell = row.get(col_i).map(String::as_str).unwrap_or("");
        let fitted = fit_cell(cell, w);
        write!(out, " {} │", fitted)?;
    }
    if is_alt {
        queue!(out, ResetColor)?;
    }
    write!(out, "\r\n")?;
    Ok(())
}

fn render_empty_row(
    out: &mut impl Write,
    vis: &VisibleCols,
    display_idx: usize,
    zebra: bool,
) -> anyhow::Result<()> {
    let is_alt = zebra && display_idx % 2 == 1;
    if is_alt {
        queue!(out, SetBackgroundColor(ZEBRA_BG))?;
    }
    write!(out, "│")?;
    for &w in &vis.widths {
        write!(out, " {:width$} │", "", width = w)?;
    }
    if is_alt {
        queue!(out, ResetColor)?;
    }
    write!(out, "\r\n")?;
    Ok(())
}

// ── text helpers ───────────────────────────────────────────────────────────

/// Fit `s` into exactly `width` display columns.
/// Pads with spaces if too short; truncates with '…' if too long.
fn fit_cell(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let w = s.width();
    if w == width {
        return s.to_owned();
    }
    if w < width {
        let mut out = s.to_owned();
        out.push_str(&" ".repeat(width - w));
        return out;
    }
    // Truncate: accumulate chars until adding the next would overflow
    let mut result = String::with_capacity(width + 4);
    let mut cur = 0usize;
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if cur + cw + 1 > width {
            break;
        }
        result.push(ch);
        cur += cw;
    }
    result.push('…');
    cur += 1;
    if cur < width {
        result.push_str(&" ".repeat(width - cur));
    }
    result
}

/// Truncate `s` so its display width ≤ `max_width` (ASCII-safe for status bar).
fn truncate_to_width(s: &str, max_width: usize) -> &str {
    if s.width() <= max_width {
        return s;
    }
    // Binary-search on byte boundary for the right width
    let mut end = 0;
    let mut cur = 0;
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if cur + cw > max_width {
            break;
        }
        cur += cw;
        end += ch.len_utf8();
    }
    &s[..end]
}
