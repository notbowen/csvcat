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

pub fn run_viewer(
    data: &CsvData,
    max_col_width: usize,
    zebra: bool,
    show_line_numbers: bool,
) -> anyhow::Result<()> {
    let layout = TableLayout::compute(data, max_col_width);
    let stdout = io::stdout();
    let mut out = BufWriter::with_capacity(64 * 1024, stdout.lock());

    terminal::enable_raw_mode()?;
    execute!(out, terminal::EnterAlternateScreen, cursor::Hide, terminal::DisableLineWrap)?;

    let result = event_loop(data, &layout, &mut out, zebra, show_line_numbers);

    let _ = execute!(out, terminal::EnableLineWrap, terminal::LeaveAlternateScreen, cursor::Show);
    let _ = terminal::disable_raw_mode();

    result
}

pub fn print_table(
    data: &CsvData,
    max_col_width: usize,
    zebra: bool,
    show_line_numbers: bool,
) -> anyhow::Result<()> {
    let layout = TableLayout::compute(data, max_col_width);
    let stdout = io::stdout();
    let mut out = BufWriter::with_capacity(64 * 1024, stdout.lock());
    let term_width = terminal::size().map(|(w, _)| w as usize).unwrap_or(120);

    let ln_w = ln_col_width(data.rows.len(), show_line_numbers);
    let reserved_left = ln_w.map(|w| w + 3).unwrap_or(0);
    let vis = VisibleCols::compute(&layout.col_widths, 0, term_width, reserved_left);
    if vis.is_empty() && ln_w.is_none() {
        return Ok(());
    }

    render_top_border(&mut out, &vis, ln_w)?;
    render_header_row(&mut out, &data.headers, &vis, ln_w)?;
    render_mid_border(&mut out, &vis, ln_w)?;
    for (i, row) in data.rows.iter().enumerate() {
        render_data_row(&mut out, row, &vis, i, zebra, ln_w, i + 1, false, false)?;
    }
    render_bot_border(&mut out, &vis, ln_w)?;
    out.flush()?;
    Ok(())
}

// ── viewer state ───────────────────────────────────────────────────────────

#[derive(PartialEq, Eq)]
enum Mode {
    Normal,
    Searching,
}

struct State {
    row_offset: usize,
    col_offset: usize,
    show_line_numbers: bool,
    mode: Mode,
    /// The active search query (updated in real-time while typing).
    search_query: String,
    /// Row indices (sorted ascending) that contain the search query.
    matches: Vec<usize>,
    /// Index into `matches` of the currently focused match.
    match_pos: Option<usize>,
}

impl State {
    fn new(show_line_numbers: bool) -> Self {
        Self {
            row_offset: 0,
            col_offset: 0,
            show_line_numbers,
            mode: Mode::Normal,
            search_query: String::new(),
            matches: Vec::new(),
            match_pos: None,
        }
    }

    fn update_matches(&mut self, data: &CsvData) {
        if self.search_query.is_empty() {
            self.matches.clear();
            self.match_pos = None;
            return;
        }
        let q = self.search_query.to_lowercase();
        self.matches = data
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.iter().any(|cell| cell.to_lowercase().contains(&q)))
            .map(|(i, _)| i)
            .collect();
        // Keep match_pos valid after matches shrink.
        if let Some(pos) = self.match_pos {
            if pos >= self.matches.len() {
                self.match_pos = if self.matches.is_empty() {
                    None
                } else {
                    Some(self.matches.len() - 1)
                };
            }
        }
    }

    /// Move to the next or previous match, scrolling row_offset to keep it visible.
    fn navigate_match(&mut self, forward: bool, term_height: usize) {
        if self.matches.is_empty() {
            return;
        }
        let next_pos = match self.match_pos {
            None => {
                if forward { 0 } else { self.matches.len() - 1 }
            }
            Some(pos) => {
                if forward {
                    (pos + 1) % self.matches.len()
                } else if pos == 0 {
                    self.matches.len() - 1
                } else {
                    pos - 1
                }
            }
        };
        self.match_pos = Some(next_pos);
        let target_row = self.matches[next_pos];
        let vis_rows = visible_rows(term_height);
        if target_row < self.row_offset || target_row >= self.row_offset + vis_rows {
            // Center the match on screen.
            self.row_offset = target_row.saturating_sub(vis_rows / 2);
        }
    }
}

// ── event loop ─────────────────────────────────────────────────────────────

fn event_loop(
    data: &CsvData,
    layout: &TableLayout,
    out: &mut impl Write,
    zebra: bool,
    show_line_numbers: bool,
) -> anyhow::Result<()> {
    let mut state = State::new(show_line_numbers);
    let mut prev_size = (0u16, 0u16);

    loop {
        let (tw, th) = terminal::size()?;

        if (tw, th) != prev_size {
            queue!(out, terminal::Clear(ClearType::All))?;
            prev_size = (tw, th);
        }

        render_frame(data, layout, out, &state, tw as usize, th as usize, zebra)?;

        match event::read()? {
            Event::Key(key) => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match state.mode {
                    Mode::Searching => match key.code {
                        KeyCode::Char(c) if !ctrl => {
                            state.search_query.push(c);
                            state.update_matches(data);
                        }
                        KeyCode::Backspace => {
                            state.search_query.pop();
                            state.update_matches(data);
                        }
                        KeyCode::Enter => {
                            state.mode = Mode::Normal;
                            if !state.matches.is_empty() {
                                // Jump to first match from the top.
                                state.match_pos = None;
                                state.navigate_match(true, th as usize);
                            }
                        }
                        KeyCode::Esc => {
                            state.mode = Mode::Normal;
                            state.search_query.clear();
                            state.matches.clear();
                            state.match_pos = None;
                        }
                        _ => {}
                    },
                    Mode::Normal => match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('c') if ctrl => break,

                        KeyCode::Char('/') => {
                            state.search_query.clear();
                            state.matches.clear();
                            state.match_pos = None;
                            state.mode = Mode::Searching;
                        }
                        KeyCode::Char('n') => state.navigate_match(true, th as usize),
                        KeyCode::Char('N') => state.navigate_match(false, th as usize),
                        KeyCode::Char('#') => {
                            state.show_line_numbers = !state.show_line_numbers;
                        }

                        KeyCode::Down | KeyCode::Char('j') => {
                            let max = max_row_offset(data.rows.len(), th as usize);
                            if state.row_offset < max {
                                state.row_offset += 1;
                            }
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            state.row_offset = state.row_offset.saturating_sub(1);
                        }
                        KeyCode::Right | KeyCode::Char('l') => {
                            let reserved = ln_col_width(data.rows.len(), state.show_line_numbers)
                                .map(|w| w + 3)
                                .unwrap_or(0);
                            let vis = VisibleCols::compute(
                                &layout.col_widths,
                                state.col_offset,
                                tw as usize,
                                reserved,
                            );
                            let last_visible =
                                vis.indices.last().copied().unwrap_or(state.col_offset);
                            if last_visible + 1 < layout.col_widths.len() {
                                state.col_offset += 1;
                            }
                        }
                        KeyCode::Left | KeyCode::Char('h') => {
                            state.col_offset = state.col_offset.saturating_sub(1);
                        }
                        KeyCode::PageDown => {
                            let step = visible_rows(th as usize);
                            let max = max_row_offset(data.rows.len(), th as usize);
                            state.row_offset = (state.row_offset + step).min(max);
                        }
                        KeyCode::PageUp => {
                            let step = visible_rows(th as usize);
                            state.row_offset = state.row_offset.saturating_sub(step);
                        }
                        KeyCode::Home | KeyCode::Char('g') => {
                            state.row_offset = 0;
                            state.col_offset = 0;
                        }
                        KeyCode::End | KeyCode::Char('G') => {
                            state.row_offset = max_row_offset(data.rows.len(), th as usize);
                        }
                        _ => {}
                    },
                }
            }
            Event::Resize(_, _) => {}
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

/// Display-column width of the line-number gutter, or None if disabled.
fn ln_col_width(total_rows: usize, show: bool) -> Option<usize> {
    if !show {
        return None;
    }
    let digits = if total_rows == 0 {
        1
    } else {
        total_rows.ilog10() as usize + 1
    };
    Some(digits.max(1))
}

// ── visible-column computation ─────────────────────────────────────────────

struct VisibleCols {
    /// Original column indices
    indices: Vec<usize>,
    /// Effective display widths (may be clamped for the last partial column)
    widths: Vec<usize>,
}

impl VisibleCols {
    /// `reserved_left`: chars already consumed by the sticky line-number column
    /// (= `ln_col_width + 3` when line numbers are on, 0 otherwise).
    fn compute(
        col_widths: &[usize],
        col_offset: usize,
        term_width: usize,
        reserved_left: usize,
    ) -> Self {
        let mut indices = Vec::new();
        let mut widths = Vec::new();
        // -1 for the leading '│', minus reserved_left for the line-number gutter
        let mut remaining = term_width.saturating_sub(1 + reserved_left);

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

const ZEBRA_BG: Color = Color::AnsiValue(236);
const MATCH_BG: Color = Color::AnsiValue(58);
const CURRENT_MATCH_BG: Color = Color::AnsiValue(136);

fn render_frame(
    data: &CsvData,
    layout: &TableLayout,
    out: &mut impl Write,
    state: &State,
    term_width: usize,
    term_height: usize,
    zebra: bool,
) -> anyhow::Result<()> {
    // Show the hardware cursor only while the user is typing a search query.
    if state.mode == Mode::Searching {
        queue!(out, cursor::Show)?;
    } else {
        queue!(out, cursor::Hide)?;
    }

    queue!(out, cursor::MoveTo(0, 0))?;

    let ln_w = ln_col_width(data.rows.len(), state.show_line_numbers);
    let reserved_left = ln_w.map(|w| w + 3).unwrap_or(0);
    let vis =
        VisibleCols::compute(&layout.col_widths, state.col_offset, term_width, reserved_left);

    if vis.is_empty() && ln_w.is_none() {
        out.flush()?;
        return Ok(());
    }

    let n_data = visible_rows(term_height);
    let end_row = (state.row_offset + n_data).min(data.rows.len());
    let rows_shown = end_row - state.row_offset;

    let current_match_row = state.match_pos.map(|p| state.matches[p]);

    render_top_border(out, &vis, ln_w)?;
    render_header_row(out, &data.headers, &vis, ln_w)?;
    render_mid_border(out, &vis, ln_w)?;

    for (i, row_idx) in (state.row_offset..end_row).enumerate() {
        // matches is sorted, so binary_search is O(log n).
        let is_match =
            !state.search_query.is_empty() && state.matches.binary_search(&row_idx).is_ok();
        let is_current = current_match_row == Some(row_idx);
        render_data_row(
            out,
            &data.rows[row_idx],
            &vis,
            i,
            zebra,
            ln_w,
            row_idx + 1,
            is_match,
            is_current,
        )?;
    }

    for fill_i in rows_shown..n_data {
        render_empty_row(out, &vis, fill_i, zebra, ln_w)?;
    }

    render_bot_border(out, &vis, ln_w)?;

    render_status_bar(out, data, state, &vis, term_width, term_height)?;

    out.flush()?;
    Ok(())
}

fn render_status_bar(
    out: &mut impl Write,
    data: &CsvData,
    state: &State,
    vis: &VisibleCols,
    term_width: usize,
    term_height: usize,
) -> anyhow::Result<()> {
    queue!(out, cursor::MoveTo(0, (term_height - 1) as u16))?;

    if state.mode == Mode::Searching {
        queue!(out, SetAttribute(Attribute::Reset))?;
        let prompt = format!("/{}", state.search_query);
        let display = truncate_to_width(&prompt, term_width.saturating_sub(1));
        write!(out, "{}", display)?;
        queue!(out, terminal::Clear(ClearType::UntilNewLine))?;
        // Position the blinking cursor right after the typed query.
        let cursor_x = (1 + state.search_query.width()) as u16;
        queue!(out, cursor::MoveTo(cursor_x, (term_height - 1) as u16))?;
    } else {
        let end_row = (state.row_offset + visible_rows(term_height)).min(data.rows.len());

        let match_info = if !state.search_query.is_empty() {
            format!(
                " │ /{} ({}/{})",
                state.search_query,
                state.match_pos.map(|p| p + 1).unwrap_or(0),
                state.matches.len()
            )
        } else {
            String::new()
        };

        let ln_hint = if state.show_line_numbers { " # hide-ln" } else { " # show-ln" };

        let status = format!(
            " Rows {}-{}/{} │ Cols {}-{}/{}{} │ hjkl/arrows │ PgUp/Dn │ g/G │ / search │ n/N │{} │ q ",
            if data.rows.is_empty() { 0 } else { state.row_offset + 1 },
            end_row,
            data.rows.len(),
            state.col_offset + 1,
            state.col_offset + vis.indices.len(),
            data.headers.len(),
            match_info,
            ln_hint,
        );

        queue!(
            out,
            SetAttribute(Attribute::Reverse),
            SetAttribute(Attribute::Bold),
        )?;
        let display = truncate_to_width(&status, term_width);
        write!(out, "{:<width$}", display, width = term_width)?;
        queue!(out, SetAttribute(Attribute::Reset))?;
    }

    Ok(())
}

// ── border / row renderers ─────────────────────────────────────────────────

fn render_top_border(
    out: &mut impl Write,
    vis: &VisibleCols,
    ln_w: Option<usize>,
) -> anyhow::Result<()> {
    write!(out, "┌")?;
    let has_data = !vis.is_empty();
    if let Some(w) = ln_w {
        write!(out, "{}", "─".repeat(w + 2))?;
        if has_data {
            write!(out, "┬")?;
        } else {
            write!(out, "┐\r\n")?;
            return Ok(());
        }
    }
    for (i, &w) in vis.widths.iter().enumerate() {
        write!(out, "{}", "─".repeat(w + 2))?;
        write!(out, "{}", if i + 1 < vis.widths.len() { "┬" } else { "┐" })?;
    }
    write!(out, "\r\n")?;
    Ok(())
}

fn render_mid_border(
    out: &mut impl Write,
    vis: &VisibleCols,
    ln_w: Option<usize>,
) -> anyhow::Result<()> {
    write!(out, "├")?;
    let has_data = !vis.is_empty();
    if let Some(w) = ln_w {
        write!(out, "{}", "─".repeat(w + 2))?;
        if has_data {
            write!(out, "┼")?;
        } else {
            write!(out, "┤\r\n")?;
            return Ok(());
        }
    }
    for (i, &w) in vis.widths.iter().enumerate() {
        write!(out, "{}", "─".repeat(w + 2))?;
        write!(out, "{}", if i + 1 < vis.widths.len() { "┼" } else { "┤" })?;
    }
    write!(out, "\r\n")?;
    Ok(())
}

fn render_bot_border(
    out: &mut impl Write,
    vis: &VisibleCols,
    ln_w: Option<usize>,
) -> anyhow::Result<()> {
    write!(out, "└")?;
    let has_data = !vis.is_empty();
    if let Some(w) = ln_w {
        write!(out, "{}", "─".repeat(w + 2))?;
        if has_data {
            write!(out, "┴")?;
        } else {
            write!(out, "┘\r\n")?;
            return Ok(());
        }
    }
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
    ln_w: Option<usize>,
) -> anyhow::Result<()> {
    write!(out, "│")?;
    if let Some(w) = ln_w {
        let fitted = fit_cell("#", w);
        queue!(
            out,
            SetAttribute(Attribute::Bold),
            SetForegroundColor(Color::DarkGrey)
        )?;
        write!(out, " {} ", fitted)?;
        queue!(out, SetAttribute(Attribute::Reset))?;
        write!(out, "│")?;
    }
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

fn render_data_row(
    out: &mut impl Write,
    row: &[String],
    vis: &VisibleCols,
    display_idx: usize,
    zebra: bool,
    ln_w: Option<usize>,
    row_num: usize,
    is_match: bool,
    is_current_match: bool,
) -> anyhow::Result<()> {
    let is_alt = !is_match && !is_current_match && zebra && display_idx % 2 == 1;

    if is_current_match {
        queue!(out, SetBackgroundColor(CURRENT_MATCH_BG))?;
    } else if is_match {
        queue!(out, SetBackgroundColor(MATCH_BG))?;
    } else if is_alt {
        queue!(out, SetBackgroundColor(ZEBRA_BG))?;
    }

    write!(out, "│")?;

    if let Some(w) = ln_w {
        // Dim grey line number; use only fg/intensity escapes so the row background persists.
        queue!(
            out,
            SetAttribute(Attribute::Dim),
            SetForegroundColor(Color::DarkGrey)
        )?;
        write!(out, " {:>width$} ", row_num, width = w)?;
        // Turn off dim and reset fg only — background is untouched.
        queue!(
            out,
            SetAttribute(Attribute::NormalIntensity),
            SetForegroundColor(Color::Reset)
        )?;
        write!(out, "│")?;
    }

    for (&col_i, &w) in vis.indices.iter().zip(vis.widths.iter()) {
        let cell = row.get(col_i).map(String::as_str).unwrap_or("");
        let fitted = fit_cell(cell, w);
        write!(out, " {} │", fitted)?;
    }

    if is_current_match || is_match || is_alt {
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
    ln_w: Option<usize>,
) -> anyhow::Result<()> {
    let is_alt = zebra && display_idx % 2 == 1;
    if is_alt {
        queue!(out, SetBackgroundColor(ZEBRA_BG))?;
    }
    write!(out, "│")?;
    if let Some(w) = ln_w {
        write!(out, " {:width$} │", "", width = w)?;
    }
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
