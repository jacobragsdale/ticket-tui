//! The columns a list screen shows: which ones, in what order, how wide, and
//! what the Columns overlay needs to edit them without knowing whose they are.

use ratatui::layout::Constraint;

use crate::session::SessionColumn;

/// The two columns the selection marker (`› `) is always given, whether or
/// not the row under the cursor is on screen.
pub const SELECTION_WIDTH: u16 = 2;

/// The gutter the check and bookmark markers sit in, on the tables that have
/// one.
pub const MARKER_WIDTH: u16 = 4;

/// The scrollbar's own column, at the right edge of every list table. It is
/// reserved whether or not the list overflows, so a table does not shuffle
/// sideways as rows arrive.
pub const SCROLLBAR_WIDTH: u16 = 1;

/// The blank column between two neighbouring cells.
pub const COLUMN_SPACING: u16 = 1;

/// The fewest characters a flexible column is squeezed to before the table
/// starts dropping optional columns from the right, unless that column asks
/// for something else. Below this a title stops being a title — "Serialize
/// se" — and whatever took the room is worth less than what lost it.
pub const MIN_FLEXIBLE_WIDTH: u16 = 24;

/// One screen's set of columns. Work items sort and arrange by `SortField`;
/// repositories, pull requests and runs bring their own enum and get the same
/// table, the same header sorting and the same Columns overlay for free.
///
/// `key` is the identity: it is what the session file records and what a
/// `SortHeader` pointer target carries, so a screen can resolve a clicked
/// header back to its own column.
pub trait ColumnId: Copy + Eq + Sized + 'static {
    /// Every column the table offers, in the order it opens with.
    fn all() -> &'static [Self];

    /// The column that key names, if this table has one. An unknown key comes
    /// out of a session file written by an older build, and is dropped.
    fn from_key(key: &str) -> Option<Self> {
        Self::all()
            .iter()
            .copied()
            .find(|column| column.key() == key)
    }

    fn key(self) -> &'static str;

    /// What the header cell says, which is not always the name the sort popup
    /// uses: a header has less room.
    fn label(self) -> &'static str;

    fn default_width(self) -> u16;

    fn default_visible(self) -> bool;

    /// Numbers read better against the right edge of their cell.
    fn right_aligned(self) -> bool;

    /// Whether the column stays whatever happens: auto-hide never takes a
    /// pinned column away, and the overlay will not hide one.
    fn pinned(self) -> bool;

    /// The one column that takes whatever width is left over. Its stored width
    /// is ignored and it cannot be resized.
    fn flexible(self) -> bool;

    /// The fewest characters this column is squeezed to while there is still
    /// an optional column to drop. Only the flexible column is asked. The
    /// default is what a title needs; a table whose flexible column holds
    /// something shorter — a repository or a pipeline name — says so.
    fn min_flexible_width(self) -> u16 {
        MIN_FLEXIBLE_WIDTH
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ColumnConfig<C> {
    pub id: C,
    pub visible: bool,
    pub width: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableLayout<C> {
    pub columns: Vec<ColumnConfig<C>>,
}

impl<C: ColumnId> Default for TableLayout<C> {
    fn default() -> Self {
        Self {
            columns: C::all()
                .iter()
                .map(|id| ColumnConfig {
                    id: *id,
                    visible: id.default_visible(),
                    width: id.default_width(),
                })
                .collect(),
        }
    }
}

impl<C: ColumnId> TableLayout<C> {
    #[must_use]
    pub fn to_session_columns(&self) -> Vec<SessionColumn> {
        self.columns
            .iter()
            .map(|column| SessionColumn {
                id: column.id.key().to_owned(),
                visible: column.visible,
                width: column.width,
            })
            .collect()
    }

    #[must_use]
    pub fn from_session_columns(columns: &[SessionColumn]) -> Self {
        let mut layout = Self::default();
        if columns.is_empty() {
            return layout;
        }
        let mut restored: Vec<ColumnConfig<C>> = columns
            .iter()
            .filter_map(|column| {
                Some(ColumnConfig {
                    id: C::from_key(&column.id)?,
                    visible: column.visible,
                    width: column.width,
                })
            })
            .collect();
        for default in &layout.columns {
            if !restored.iter().any(|column| column.id == default.id) {
                restored.push(*default);
            }
        }
        layout.columns = restored;
        layout
    }

    /// The width the columns and the gaps between them share, inside a pane
    /// `inner_width` wide: what the selection marker, the gutter and the
    /// scrollbar take is spent before a column sees any of it.
    #[must_use]
    pub fn available_width(inner_width: u16, marker: bool) -> u16 {
        let gutter = if marker {
            MARKER_WIDTH + COLUMN_SPACING
        } else {
            0
        };
        inner_width
            .saturating_sub(SELECTION_WIDTH)
            .saturating_sub(SCROLLBAR_WIDTH)
            .saturating_sub(gutter)
    }

    /// The columns this table draws in `available` cells, dropping the
    /// right-most unpinned one for as long as the flexible column would
    /// otherwise fall under [`MIN_FLEXIBLE_WIDTH`]. A pinned column never
    /// goes, so a table always keeps its identity and its title.
    #[must_use]
    pub fn visible_columns(&self, available: u16) -> Vec<ColumnConfig<C>> {
        let mut columns: Vec<_> = self
            .columns
            .iter()
            .copied()
            .filter(|column| column.visible)
            .collect();
        while columns.len() > 2 && required_width(&columns) > available {
            if let Some(index) = columns.iter().rposition(|column| !column.id.pinned()) {
                columns.remove(index);
            } else {
                break;
            }
        }
        columns
    }

    #[must_use]
    pub fn constraint(column: ColumnConfig<C>) -> Constraint {
        if column.id.flexible() || column.width == 0 {
            Constraint::Fill(1)
        } else {
            Constraint::Length(column.width)
        }
    }
}

/// What the Columns overlay needs of a layout, whichever screen's it is. The
/// overlay draws and edits rows by index and never names a column type.
pub trait ColumnLayout {
    fn count(&self) -> usize;
    fn label(&self, index: usize) -> &'static str;
    fn is_visible(&self, index: usize) -> bool;
    fn width(&self, index: usize) -> u16;
    /// Whether the column takes the width left over, which is what the overlay
    /// shows as `fill` and refuses to resize.
    fn flexible(&self, index: usize) -> bool;
    fn toggle_visible(&mut self, index: usize);
    /// Moves one column and answers where it landed.
    fn move_column(&mut self, index: usize, delta: isize) -> usize;
    fn resize(&mut self, index: usize, delta: i16);
}

impl<C: ColumnId> ColumnLayout for TableLayout<C> {
    fn count(&self) -> usize {
        self.columns.len()
    }

    fn label(&self, index: usize) -> &'static str {
        self.columns
            .get(index)
            .map_or("", |column| column.id.label())
    }

    fn is_visible(&self, index: usize) -> bool {
        self.columns.get(index).is_some_and(|column| column.visible)
    }

    fn width(&self, index: usize) -> u16 {
        self.columns.get(index).map_or(0, |column| column.width)
    }

    fn flexible(&self, index: usize) -> bool {
        self.columns
            .get(index)
            .is_some_and(|column| column.id.flexible())
    }

    fn toggle_visible(&mut self, index: usize) {
        if let Some(column) = self.columns.get_mut(index)
            && !column.id.pinned()
        {
            column.visible = !column.visible;
        }
    }

    fn move_column(&mut self, index: usize, delta: isize) -> usize {
        if self.columns.is_empty() {
            return 0;
        }
        let next = index
            .saturating_add_signed(delta)
            .min(self.columns.len() - 1);
        if next != index {
            self.columns.swap(index, next);
        }
        next
    }

    fn resize(&mut self, index: usize, delta: i16) {
        let Some(column) = self.columns.get_mut(index) else {
            return;
        };
        if column.id.flexible() {
            return;
        }
        let width = i16::try_from(column.width).unwrap_or(i16::MAX);
        column.width = width.saturating_add(delta).clamp(3, 40) as u16;
    }
}

/// What these columns need to draw with the flexible one still readable: every
/// fixed width, the flexible column's own minimum, and a gap between each
/// pair.
fn required_width<C: ColumnId>(columns: &[ColumnConfig<C>]) -> u16 {
    let spacing = COLUMN_SPACING.saturating_mul(
        u16::try_from(columns.len())
            .unwrap_or(u16::MAX)
            .saturating_sub(1),
    );
    columns
        .iter()
        .map(|column| {
            if column.id.flexible() || column.width == 0 {
                column.id.min_flexible_width()
            } else {
                column.width.max(3)
            }
        })
        .fold(spacing, u16::saturating_add)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SortField;

    /// What the flexible column is left with once the fixed ones and the gaps
    /// between them are paid for.
    fn flexible_width<C: ColumnId>(columns: &[ColumnConfig<C>], available: u16) -> u16 {
        let spacing = COLUMN_SPACING * (columns.len() as u16 - 1);
        let fixed: u16 = columns
            .iter()
            .filter(|column| !column.id.flexible() && column.width > 0)
            .map(|column| column.width)
            .sum();
        available.saturating_sub(spacing).saturating_sub(fixed)
    }

    #[test]
    fn columns_drop_from_the_right_before_the_title_is_squeezed() {
        let layout = TableLayout::<SortField>::default();
        for pane in [140_u16, 110, 90, 60] {
            // What the pane leaves the columns: its own border, the selection
            // marker, the check-and-bookmark gutter and the scrollbar.
            let available = TableLayout::<SortField>::available_width(pane - 2, true);
            let columns = layout.visible_columns(available);
            let visible: Vec<_> = columns.iter().map(|column| column.id).collect();

            assert_eq!(visible[0], SortField::Id, "the pinned columns stay");
            assert_eq!(visible[1], SortField::Title);
            assert!(
                flexible_width(&columns, available) >= MIN_FLEXIBLE_WIDTH,
                "{pane} columns left the title {} wide with {visible:?}",
                flexible_width(&columns, available)
            );
            // Whatever went, went off the right-hand end.
            let mut ordered = visible.clone();
            ordered.sort_by_key(|id| {
                SortField::all()
                    .iter()
                    .position(|candidate| candidate == id)
                    .unwrap()
            });
            assert_eq!(ordered, visible, "the columns keep their order");
        }

        let wide = TableLayout::<SortField>::available_width(200, true);
        assert!(
            layout
                .visible_columns(wide)
                .iter()
                .any(|column| column.id == SortField::Assignee),
            "a wide enough table keeps every column it was given"
        );

        // Narrower than the two pinned columns want, and there is nothing left
        // to drop: the title takes what is there rather than the table
        // dropping the column that says which work item a row is.
        let cramped = TableLayout::<SortField>::available_width(38, true);
        let columns = layout.visible_columns(cramped);
        assert_eq!(
            columns.iter().map(|column| column.id).collect::<Vec<_>>(),
            vec![SortField::Id, SortField::Title]
        );
        assert!(flexible_width(&columns, cramped) > 0);
    }

    #[test]
    fn child_progress_is_an_opt_in_column_the_overlay_turns_on_and_off() {
        let mut layout = TableLayout::<SortField>::default();
        let index = layout
            .columns
            .iter()
            .position(|column| column.id == SortField::Progress)
            .expect("the layout offers a Progress column");
        assert!(
            !layout.columns[index].visible,
            "nobody asked for it, so it starts hidden"
        );
        assert!(
            !layout
                .visible_columns(200)
                .iter()
                .any(|column| column.id == SortField::Progress)
        );

        layout.toggle_visible(index);
        assert!(layout.columns[index].visible);
        assert!(
            layout
                .visible_columns(200)
                .iter()
                .any(|column| column.id == SortField::Progress)
        );

        layout.toggle_visible(index);
        assert!(!layout.columns[index].visible);
    }

    #[test]
    fn editing_the_columns_leaves_the_pinned_ones_alone() {
        let mut layout = TableLayout::<SortField>::default();
        let area = layout
            .columns
            .iter()
            .position(|column| column.id == SortField::Area)
            .unwrap();

        layout.toggle_visible(area);
        assert!(layout.columns[area].visible);

        let moved = layout.move_column(area, -1);
        assert_eq!(layout.columns[moved].id, SortField::Area);

        let title = 1;
        assert_eq!(layout.columns[title].id, SortField::Title);
        layout.resize(title, 10);
        assert_eq!(layout.columns[title].width, 0);
        layout.toggle_visible(title);
        assert!(layout.columns[title].visible);
    }

    #[test]
    fn a_second_column_set_gets_the_same_layout_and_the_same_session_shape() {
        use crate::app::repos::RepoColumn;

        let mut layout = TableLayout::<RepoColumn>::default();
        assert_eq!(
            layout
                .visible_columns(200)
                .into_iter()
                .map(|column| column.id)
                .collect::<Vec<_>>(),
            vec![
                RepoColumn::Name,
                RepoColumn::DefaultBranch,
                RepoColumn::PullRequests,
                RepoColumn::Pipelines,
                RepoColumn::Local,
            ],
        );

        let local = 4;
        layout.resize(local, 2);
        let stored = layout.to_session_columns();
        assert_eq!(stored[0].id, "name");
        assert_eq!(stored[local].width, 22);

        let restored = TableLayout::<RepoColumn>::from_session_columns(&stored);
        assert_eq!(restored, layout, "the file round-trips through the keys");

        layout.toggle_visible(0);
        assert!(
            layout.is_visible(0),
            "the pinned column cannot be turned off"
        );
        layout.resize(0, 5);
        assert_eq!(layout.width(0), 0, "and the flexible one cannot be resized");
    }
}
