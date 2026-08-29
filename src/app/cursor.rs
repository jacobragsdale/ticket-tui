//! `ListCursor`: where a list's cursor is and how far the list is scrolled.
//! Every picker, every overlay and every screen's table keeps one, so moving a
//! cursor and keeping it on screen is written once.

use crate::pointer::ScrollState;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ListCursor {
    /// Which row the cursor is on, counted over whatever the list is showing
    /// now — a filtered picker counts the rows its query left.
    pub index: usize,
    pub scroll: ScrollState,
}

impl ListCursor {
    /// Puts the cursor on one row and scrolls it into view.
    pub const fn focus(&mut self, index: usize) {
        self.index = index;
        self.scroll.ensure_visible(index);
    }

    /// Moves the cursor by `delta` rows, stopping at either end of a list of
    /// `count` rows. An empty list puts it back at the top.
    pub const fn move_by(&mut self, delta: isize, count: usize) {
        if count == 0 {
            self.index = 0;
            self.scroll.scroll_to(0);
            return;
        }
        let index = self.index.saturating_add_signed(delta);
        self.focus(if index > count - 1 { count - 1 } else { index });
    }

    /// The same, a screenful at a time.
    pub const fn page(&mut self, direction: isize, count: usize) {
        let step = self.scroll.page_step() as isize;
        self.move_by(direction * step, count);
    }

    /// Back to the first row, as reopening a list does.
    pub const fn reset(&mut self) {
        self.index = 0;
        self.scroll.scroll_to(0);
    }

    /// Re-clamps the cursor after the list under it has changed length.
    pub const fn clamp(&mut self, count: usize) {
        if count == 0 {
            self.reset();
        } else if self.index > count - 1 {
            self.focus(count - 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor(viewport: usize) -> ListCursor {
        let mut cursor = ListCursor::default();
        cursor.scroll.viewport = viewport;
        cursor.scroll.content = 20;
        cursor
    }

    #[test]
    fn a_cursor_stops_at_both_ends_and_scrolls_itself_into_view() {
        let mut list = cursor(5);
        list.move_by(-1, 20);
        assert_eq!((list.index, list.scroll.offset), (0, 0), "the top holds");

        list.move_by(7, 20);
        assert_eq!(list.index, 7);
        assert_eq!(list.scroll.offset, 3, "the row is the last one on screen");

        list.move_by(50, 20);
        assert_eq!(list.index, 19, "and the bottom holds");

        list.page(-1, 20);
        assert_eq!(
            list.index, 15,
            "a page is a screenful less the row of overlap"
        );
        list.reset();
        assert_eq!((list.index, list.scroll.offset), (0, 0));
    }

    #[test]
    fn a_shorter_list_pulls_the_cursor_back_onto_it() {
        let mut list = cursor(5);
        list.move_by(9, 20);
        assert_eq!(list.index, 9);

        list.clamp(4);
        assert_eq!(list.index, 3, "the last row of the shorter list");

        list.clamp(0);
        assert_eq!((list.index, list.scroll.offset), (0, 0), "and none of it");
    }
}
