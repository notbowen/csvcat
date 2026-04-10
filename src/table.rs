use unicode_width::UnicodeWidthStr;
use crate::reader::CsvData;

pub struct TableLayout {
    pub col_widths: Vec<usize>,
}

impl TableLayout {
    /// Compute the display width for each column, capped at `max_col_width`.
    pub fn compute(data: &CsvData, max_col_width: usize) -> Self {
        let ncols = data.headers.len();
        let mut col_widths = vec![0usize; ncols];

        for (i, h) in data.headers.iter().enumerate() {
            col_widths[i] = h.width();
        }

        for row in &data.rows {
            for (i, cell) in row.iter().enumerate().take(ncols) {
                let w = cell.width().min(max_col_width);
                if w > col_widths[i] {
                    col_widths[i] = w;
                }
            }
        }

        // Ensure every column is at least 1 character wide
        for w in &mut col_widths {
            *w = (*w).max(1);
        }

        Self { col_widths }
    }
}
