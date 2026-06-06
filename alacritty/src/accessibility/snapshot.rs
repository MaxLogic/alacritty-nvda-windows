//! Visible terminal text snapshots for accessibility providers.

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::Flags;

/// Immutable snapshot of the terminal's visible text.
#[derive(Clone, Debug)]
pub struct VisibleTerminalSnapshot {
    text: String,
    rows: Vec<SnapshotRow>,
    columns: usize,
    cursor: Point<usize>,
}

#[derive(Clone, Debug)]
struct SnapshotRow {
    text: String,
    start_offset: usize,
    column_offsets: Vec<usize>,
}

impl VisibleTerminalSnapshot {
    /// Build a snapshot from the visible terminal viewport.
    pub fn from_term<T>(term: &Term<T>) -> Self {
        let grid = term.grid();
        let screen_lines = grid.screen_lines();
        let columns = grid.columns();
        let display_offset = grid.display_offset();
        let mut rows = Vec::with_capacity(screen_lines);
        let mut text = String::new();

        for row in 0..screen_lines {
            let line = Line(row as i32) - display_offset;

            if !rows.is_empty() {
                text.push('\n');
            }

            let start_offset = text.len();
            let row = SnapshotRow::new(&grid[line], start_offset);

            text.push_str(&row.text);
            rows.push(row);
        }

        let cursor = alacritty_terminal::term::point_to_viewport(display_offset, grid.cursor.point)
            .unwrap_or_else(|| Point::new(0, Column(0)));

        Self { text, rows, columns, cursor }
    }

    /// Newline-separated visible terminal text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Visible text for one row without the row separator.
    pub fn row_text(&self, row: usize) -> Option<&str> {
        self.rows.get(row).map(|row| row.text.as_str())
    }

    /// Number of visible terminal rows.
    pub fn screen_lines(&self) -> usize {
        self.rows.len()
    }

    /// Number of terminal columns.
    pub fn columns(&self) -> usize {
        self.columns
    }

    /// Cursor position relative to the visible viewport.
    pub fn cursor(&self) -> Point<usize> {
        self.cursor
    }

    /// Convert a viewport row/column to a byte offset into [`Self::text`].
    pub fn offset_for_point(&self, point: Point<usize>) -> Option<usize> {
        let row = self.rows.get(point.line)?;
        let column = point.column.0.min(self.columns);
        Some(row.start_offset + row.column_offsets[column].min(row.text.len()))
    }

    /// Convert a byte offset into [`Self::text`] to the nearest viewport row/column.
    pub fn point_for_offset(&self, offset: usize) -> Option<Point<usize>> {
        if offset > self.text.len() {
            return None;
        }

        let row_index = self
            .rows
            .partition_point(|row| row.start_offset + row.text.len() < offset)
            .min(self.rows.len().saturating_sub(1));
        let row = self.rows.get(row_index)?;
        let row_offset = offset.saturating_sub(row.start_offset).min(row.text.len());
        let lower_bound =
            row.column_offsets.partition_point(|column_offset| *column_offset < row_offset);
        let column = if row.column_offsets.get(lower_bound) == Some(&row_offset) {
            lower_bound
        } else {
            lower_bound.saturating_sub(1)
        }
        .min(self.columns);

        Some(Point::new(row_index, Column(column)))
    }
}

impl SnapshotRow {
    fn new(
        row: &alacritty_terminal::grid::Row<alacritty_terminal::term::cell::Cell>,
        start_offset: usize,
    ) -> Self {
        let mut text = String::with_capacity(row.len());
        let mut column_offsets = Vec::with_capacity(row.len() + 1);

        for cell in row {
            column_offsets.push(text.len());

            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            } else if cell.flags.contains(Flags::HIDDEN) {
                text.push(' ');
            } else {
                text.push(cell.c);
                if let Some(zerowidth) = cell.zerowidth() {
                    text.extend(zerowidth);
                }
            }
        }

        column_offsets.push(text.len());
        text.truncate(text.trim_end_matches(' ').len());

        Self { text, start_offset, column_offsets }
    }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line, Point};
    use alacritty_terminal::term::cell::Flags;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::term::{Config, Term};

    use super::VisibleTerminalSnapshot;

    fn term(columns: usize, screen_lines: usize) -> Term<()> {
        Term::new(Config::default(), &TermSize::new(columns, screen_lines), ())
    }

    #[test]
    fn snapshot_text_contains_visible_rows_with_newline_separators() {
        let mut term = term(4, 3);
        term.grid_mut()[Line(0)][Column(0)].c = 'a';
        term.grid_mut()[Line(0)][Column(1)].c = 'b';
        term.grid_mut()[Line(1)][Column(0)].c = 'c';
        term.grid_mut()[Line(1)][Column(3)].c = 'd';

        let snapshot = VisibleTerminalSnapshot::from_term(&term);

        assert_eq!(snapshot.text(), "ab\nc  d\n");
        assert_eq!(snapshot.row_text(0), Some("ab"));
        assert_eq!(snapshot.row_text(1), Some("c  d"));
        assert_eq!(snapshot.row_text(2), Some(""));
        assert_eq!(snapshot.row_text(3), None);
    }

    #[test]
    fn snapshot_maps_rows_columns_and_offsets() {
        let mut term = term(4, 3);
        term.grid_mut()[Line(0)][Column(0)].c = 'a';
        term.grid_mut()[Line(0)][Column(1)].c = 'b';
        term.grid_mut()[Line(1)][Column(0)].c = 'c';
        term.grid_mut()[Line(1)][Column(3)].c = 'd';

        let snapshot = VisibleTerminalSnapshot::from_term(&term);

        assert_eq!(snapshot.offset_for_point(Point::new(0, Column(0))), Some(0));
        assert_eq!(snapshot.offset_for_point(Point::new(0, Column(2))), Some(2));
        assert_eq!(snapshot.offset_for_point(Point::new(1, Column(0))), Some(3));
        assert_eq!(snapshot.offset_for_point(Point::new(1, Column(3))), Some(6));
        assert_eq!(snapshot.offset_for_point(Point::new(2, Column(0))), Some(8));
        assert_eq!(snapshot.offset_for_point(Point::new(3, Column(0))), None);

        assert_eq!(snapshot.point_for_offset(0), Some(Point::new(0, Column(0))));
        assert_eq!(snapshot.point_for_offset(2), Some(Point::new(0, Column(2))));
        assert_eq!(snapshot.point_for_offset(3), Some(Point::new(1, Column(0))));
        assert_eq!(snapshot.point_for_offset(6), Some(Point::new(1, Column(3))));
        assert_eq!(snapshot.point_for_offset(8), Some(Point::new(2, Column(0))));
        assert_eq!(
            snapshot.point_for_offset(snapshot.text().len()),
            Some(Point::new(2, Column(0)))
        );
        assert_eq!(snapshot.point_for_offset(snapshot.text().len() + 1), None);
    }

    #[test]
    fn snapshot_handles_wide_spacer_hidden_empty_rows_and_cursor() {
        let mut term = term(5, 3);
        term.grid_mut().cursor.point = Point::new(Line(1), Column(2));
        term.grid_mut()[Line(0)][Column(0)].c = '中';
        term.grid_mut()[Line(0)][Column(0)].flags.insert(Flags::WIDE_CHAR);
        term.grid_mut()[Line(0)][Column(1)].flags.insert(Flags::WIDE_CHAR_SPACER);
        term.grid_mut()[Line(0)][Column(2)].c = 'x';
        term.grid_mut()[Line(0)][Column(2)].flags.insert(Flags::HIDDEN);
        term.grid_mut()[Line(0)][Column(4)].c = 'z';

        let snapshot = VisibleTerminalSnapshot::from_term(&term);

        assert_eq!(snapshot.screen_lines(), term.screen_lines());
        assert_eq!(snapshot.columns(), term.columns());
        assert_eq!(snapshot.cursor(), Point::new(1, Column(2)));
        assert_eq!(snapshot.row_text(0), Some("中  z"));
        assert_eq!(snapshot.row_text(1), Some(""));
        assert_eq!(snapshot.text(), "中  z\n\n");
        assert_eq!(snapshot.offset_for_point(Point::new(0, Column(0))), Some(0));
        assert_eq!(snapshot.offset_for_point(Point::new(0, Column(1))), Some("中".len()));
        assert_eq!(snapshot.offset_for_point(Point::new(0, Column(2))), Some("中".len()));
        assert_eq!(snapshot.point_for_offset("中".len()), Some(Point::new(0, Column(1))));
    }

    #[test]
    fn snapshot_preserves_visible_zerowidth_characters() {
        let mut term = term(4, 1);
        term.grid_mut()[Line(0)][Column(0)].c = 'e';
        term.grid_mut()[Line(0)][Column(0)].push_zerowidth('\u{301}');
        term.grid_mut()[Line(0)][Column(1)].c = 'x';

        let snapshot = VisibleTerminalSnapshot::from_term(&term);

        assert_eq!(snapshot.text(), "e\u{301}x");
        assert_eq!(snapshot.offset_for_point(Point::new(0, Column(0))), Some(0));
        assert_eq!(snapshot.offset_for_point(Point::new(0, Column(1))), Some("e\u{301}".len()));
        assert_eq!(snapshot.point_for_offset("e\u{301}".len()), Some(Point::new(0, Column(1))));
    }
}
