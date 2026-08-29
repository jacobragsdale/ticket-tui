//! The columns a list screen shows: which ones, in what order, how wide, and
//! what the Columns overlay needs to edit them without knowing whose they are.

use ratatui::layout::Constraint;

use crate::session::SessionColumn;

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
    fn from_key(key: &str) -> Option<Self>;

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
    /// Whether the table is still dropping optional columns to fit a narrow
    /// terminal. Any deliberate change to the layout turns it off.
    pub auto_hide: bool,
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
            auto_hide: true,
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
    pub fn from_session_columns(columns: &[SessionColumn], auto_hide: Option<bool>) -> Self {
        let mut layout = Self::default();
        if columns.is_empty() {
            if let Some(auto_hide) = auto_hide {
                layout.auto_hide = auto_hide;
            }
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
        layout.auto_hide = auto_hide.unwrap_or(false);
        layout
    }

    #[must_use]
    pub fn visible_columns(&self, inner_width: u16) -> Vec<ColumnConfig<C>> {
        let mut columns: Vec<_> = self
            .columns
            .iter()
            .copied()
            .filter(|column| column.visible)
            .collect();
        if !self.auto_hide {
            return columns;
        }
        while columns.len() > 2 {
            let required = required_width(&columns);
            if required <= inner_width {
                break;
            }
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
    fn auto_hide(&self) -> bool;
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
        self.columns.get(index).map_or("", |column| column.id.label())
    }

    fn is_visible(&self, index: usize) -> bool {
        self.columns.get(index).is_some_and(|column| column.visible)
    }

    fn width(&self, index: usize) -> u16 {
        self.columns.get(index).map_or(0, |column| column.width)
    }

    fn auto_hide(&self) -> bool {
        self.auto_hide
    }

    fn toggle_visible(&mut self, index: usize) {
        if let Some(column) = self.columns.get_mut(index)
            && !column.id.pinned()
        {
            column.visible = !column.visible;
            self.auto_hide = false;
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
            self.auto_hide = false;
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
        self.auto_hide = false;
    }
}

fn required_width<C: ColumnId>(columns: &[ColumnConfig<C>]) -> u16 {
    let spacing = columns.len().saturating_sub(1) as u16;
    let widths: u16 = columns
        .iter()
        .map(|column| {
            if column.id.flexible() {
                12
            } else {
                column.width.max(3)
            }
        })
        .sum();
    widths.saturating_add(spacing).saturating_add(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SortField;

    #[test]
    fn auto_hide_drops_trailing_optional_columns_when_narrow() {
        let layout = TableLayout::<SortField>::default();
        let visible: Vec<_> = layout
            .visible_columns(40)
            .into_iter()
            .map(|column| column.id)
            .collect();

        assert_eq!(visible[0], SortField::Id);
        assert_eq!(visible[1], SortField::Title);
        assert!(!visible.contains(&SortField::Assignee));
        assert!(!visible.contains(&SortField::Organization));
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
    fn toggling_disables_auto_hide_while_title_stays_visible() {
        let mut layout = TableLayout::<SortField>::default();
        let org = layout
            .columns
            .iter()
            .position(|column| column.id == SortField::Organization)
            .unwrap();

        layout.toggle_visible(org);
        assert!(layout.columns[org].visible);
        assert!(!layout.auto_hide);

        let moved = layout.move_column(org, -1);
        assert_eq!(layout.columns[moved].id, SortField::Organization);

        let title = 1;
        assert_eq!(layout.columns[title].id, SortField::Title);
        layout.resize(title, 10);
        assert_eq!(layout.columns[title].width, 0);
        layout.toggle_visible(title);
        assert!(layout.columns[title].visible);
    }

    /// A second column set, to prove the layout is not a work-item one. It is
    /// what #668's repositories tab will look like.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum RepoColumn {
        Name,
        Branch,
        Status,
        Size,
    }

    impl ColumnId for RepoColumn {
        fn all() -> &'static [Self] {
            &[Self::Name, Self::Branch, Self::Status, Self::Size]
        }

        fn from_key(key: &str) -> Option<Self> {
            Self::all().iter().copied().find(|column| column.key() == key)
        }

        fn key(self) -> &'static str {
            match self {
                Self::Name => "name",
                Self::Branch => "branch",
                Self::Status => "status",
                Self::Size => "size",
            }
        }

        fn label(self) -> &'static str {
            match self {
                Self::Name => "Repository",
                Self::Branch => "Branch",
                Self::Status => "Status",
                Self::Size => "Size",
            }
        }

        fn default_width(self) -> u16 {
            match self {
                Self::Name => 0,
                Self::Branch => 18,
                Self::Status => 12,
                Self::Size => 8,
            }
        }

        fn default_visible(self) -> bool {
            !matches!(self, Self::Size)
        }

        fn right_aligned(self) -> bool {
            matches!(self, Self::Size)
        }

        fn pinned(self) -> bool {
            matches!(self, Self::Name)
        }

        fn flexible(self) -> bool {
            matches!(self, Self::Name)
        }
    }

    #[test]
    fn a_second_column_set_gets_the_same_layout_and_the_same_session_shape() {
        let mut layout = TableLayout::<RepoColumn>::default();
        assert_eq!(
            layout
                .visible_columns(120)
                .into_iter()
                .map(|column| column.id)
                .collect::<Vec<_>>(),
            vec![RepoColumn::Name, RepoColumn::Branch, RepoColumn::Status],
            "the hidden column stays off until it is asked for"
        );

        let size = 3;
        layout.toggle_visible(size);
        layout.resize(size, 2);
        let stored = layout.to_session_columns();
        assert_eq!(stored[0].id, "name");
        assert_eq!(stored[size].width, 10);

        let restored = TableLayout::<RepoColumn>::from_session_columns(&stored, Some(false));
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
