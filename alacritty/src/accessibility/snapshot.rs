//! Visible terminal text snapshots for accessibility providers.

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Term, point_to_viewport};

/// Immutable snapshot of the terminal's visible text.
#[derive(Clone, Debug, PartialEq)]
pub struct VisibleTerminalSnapshot {
    text: String,
    rows: Vec<SnapshotRow>,
    columns: usize,
    cursor: Point<usize>,
}

#[derive(Clone, Debug, PartialEq)]
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

    #[cfg(test)]
    pub(crate) fn from_text_for_tests(text: &str, columns: usize, screen_lines: usize) -> Self {
        let mut rows = Vec::with_capacity(screen_lines);
        let mut snapshot_text = String::new();

        for line in 0..screen_lines {
            if line > 0 {
                snapshot_text.push('\n');
            }

            let row_text = text.split('\n').nth(line).unwrap_or_default().to_owned();
            let start_offset = snapshot_text.len();
            snapshot_text.push_str(&row_text);
            rows.push(SnapshotRow::from_text(row_text, start_offset, columns));
        }

        Self { text: snapshot_text, rows, columns, cursor: Point::new(0, Column(0)) }
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

    /// Convert the terminal's active visible selection to byte offsets into [`Self::text`].
    pub fn selection_offsets<T>(&self, term: &Term<T>) -> Vec<(usize, usize)> {
        let Some(selection) =
            term.selection.as_ref().and_then(|selection| selection.to_range(term))
        else {
            return Vec::new();
        };
        let display_offset = term.grid().display_offset();

        if selection.is_block {
            let start_column = selection.start.column.0.min(selection.end.column.0);
            let end_column = selection.start.column.0.max(selection.end.column.0);
            return (selection.start.line.0..=selection.end.line.0)
                .filter_map(|line| {
                    let row = point_to_viewport(display_offset, Point::new(Line(line), Column(0)))?;
                    let start =
                        self.offset_for_point(Point::new(row.line, Column(start_column)))?;
                    let end =
                        self.offset_for_point(Point::new(row.line, Column(end_column + 1)))?;
                    (start < end).then_some((start, end))
                })
                .collect();
        }

        let visible_top = -(display_offset as i32);
        let visible_bottom = visible_top + self.rows.len().saturating_sub(1) as i32;
        if selection.end.line.0 < visible_top || selection.start.line.0 > visible_bottom {
            return Vec::new();
        }

        let start_line = selection.start.line.0.max(visible_top);
        let end_line = selection.end.line.0.min(visible_bottom);
        let start_column =
            if selection.start.line.0 < visible_top { Column(0) } else { selection.start.column };
        let end_column = if selection.end.line.0 > visible_bottom {
            Column(self.columns)
        } else {
            Column((selection.end.column.0 + 1).min(self.columns))
        };

        let start = point_to_viewport(display_offset, Point::new(Line(start_line), start_column))
            .and_then(|point| self.offset_for_point(point));
        let end = point_to_viewport(display_offset, Point::new(Line(end_line), end_column))
            .and_then(|point| self.offset_for_point(point));

        match (start, end) {
            (Some(start), Some(end)) if start < end => vec![(start, end)],
            _ => Vec::new(),
        }
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

    #[cfg(test)]
    fn from_text(text: String, start_offset: usize, columns: usize) -> Self {
        let mut column_offsets = Vec::with_capacity(columns + 1);
        let mut char_offsets: Vec<usize> = text.char_indices().map(|(index, _)| index).collect();
        char_offsets.push(text.len());

        for column in 0..=columns {
            column_offsets.push(*char_offsets.get(column).unwrap_or(&text.len()));
        }

        Self { text, start_offset, column_offsets }
    }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line, Point, Side};
    use alacritty_terminal::selection::{Selection, SelectionType};
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

    #[test]
    fn snapshot_clips_selection_to_visible_text() {
        let mut term = term(4, 2);
        term.grid_mut()[Line(0)][Column(0)].c = 'a';
        term.grid_mut()[Line(0)][Column(1)].c = 'b';
        term.selection = Some(Selection::new(
            SelectionType::Simple,
            Point::new(Line(-1), Column(0)),
            Side::Left,
        ));
        term.selection.as_mut().unwrap().update(Point::new(Line(0), Column(1)), Side::Right);

        let snapshot = VisibleTerminalSnapshot::from_term(&term);

        assert_eq!(snapshot.selection_offsets(&term), vec![(0, 2)]);
    }
}
