use ratatui::layout::Constraint;

use crate::model::SortField;
use crate::session::SessionColumn;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ColumnConfig {
    pub id: SortField,
    pub visible: bool,
    pub width: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableLayout {
    pub columns: Vec<ColumnConfig>,
    pub auto_hide: bool,
}

impl Default for TableLayout {
    fn default() -> Self {
        Self {
            columns: vec![
                ColumnConfig {
                    id: SortField::Id,
                    visible: true,
                    width: 7,
                },
                ColumnConfig {
                    id: SortField::Title,
                    visible: true,
                    width: 0,
                },
                ColumnConfig {
                    id: SortField::State,
                    visible: true,
                    width: 10,
                },
                ColumnConfig {
                    id: SortField::Type,
                    visible: true,
                    width: 13,
                },
                ColumnConfig {
                    id: SortField::Priority,
                    visible: true,
                    width: 4,
                },
                ColumnConfig {
                    id: SortField::Changed,
                    visible: true,
                    width: 10,
                },
                ColumnConfig {
                    id: SortField::Assignee,
                    visible: true,
                    width: 16,
                },
                ColumnConfig {
                    id: SortField::Organization,
                    visible: false,
                    width: 12,
                },
                ColumnConfig {
                    id: SortField::Project,
                    visible: false,
                    width: 10,
                },
                ColumnConfig {
                    id: SortField::Area,
                    visible: false,
                    width: 16,
                },
                ColumnConfig {
                    id: SortField::Iteration,
                    visible: false,
                    width: 16,
                },
                ColumnConfig {
                    id: SortField::Created,
                    visible: false,
                    width: 10,
                },
                ColumnConfig {
                    id: SortField::Tags,
                    visible: false,
                    width: 16,
                },
            ],
            auto_hide: true,
        }
    }
}

impl TableLayout {
    #[must_use]
    pub fn to_session_columns(&self) -> Vec<SessionColumn> {
        self.columns
            .iter()
            .map(|column| SessionColumn {
                id: column.id,
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
        let mut restored: Vec<ColumnConfig> = columns
            .iter()
            .map(|column| ColumnConfig {
                id: column.id,
                visible: column.visible,
                width: column.width,
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
    pub fn visible_columns(&self, inner_width: u16) -> Vec<ColumnConfig> {
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
            if let Some(index) = columns
                .iter()
                .rposition(|column| !matches!(column.id, SortField::Id | SortField::Title))
            {
                columns.remove(index);
            } else {
                break;
            }
        }
        columns
    }

    pub fn toggle_visible(&mut self, index: usize) {
        if let Some(column) = self.columns.get_mut(index)
            && !matches!(column.id, SortField::Id | SortField::Title)
        {
            column.visible = !column.visible;
            self.auto_hide = false;
        }
    }

    pub fn move_column(&mut self, index: usize, delta: isize) -> usize {
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

    pub fn resize(&mut self, index: usize, delta: i16) {
        let Some(column) = self.columns.get_mut(index) else {
            return;
        };
        if column.id == SortField::Title {
            return;
        }
        let width = i16::try_from(column.width).unwrap_or(i16::MAX);
        column.width = width.saturating_add(delta).clamp(3, 40) as u16;
        self.auto_hide = false;
    }

    #[must_use]
    pub fn constraint(column: ColumnConfig) -> Constraint {
        if column.id == SortField::Title || column.width == 0 {
            Constraint::Fill(1)
        } else {
            Constraint::Length(column.width)
        }
    }
}

fn required_width(columns: &[ColumnConfig]) -> u16 {
    let spacing = columns.len().saturating_sub(1) as u16;
    let widths: u16 = columns
        .iter()
        .map(|column| {
            if column.id == SortField::Title {
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

    #[test]
    fn auto_hide_drops_trailing_optional_columns_when_narrow() {
        let layout = TableLayout::default();
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
    fn toggling_disables_auto_hide_while_title_stays_visible() {
        let mut layout = TableLayout::default();
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
}
