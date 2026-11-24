//! Keyboard navigation shared by the views that show a list of rows.

use gpui_kit::{UniformListScrollHandle, actions};

actions!(
    list,
    [
        SelectPrevious,
        SelectNext,
        SelectPreviousPage,
        SelectNextPage,
        SelectFirst,
        SelectLast,
    ]
);

/// Rows of a uniform list fully visible in its viewport during the last layout, at least one.
pub fn page_rows(scroll: &UniformListScrollHandle, len: usize) -> usize {
    let state = scroll.0.borrow();
    let Some(size) = state.last_item_size.filter(|_| len > 0) else {
        return 1;
    };
    let row_height = size.contents.height / len as f32;
    let rows = (size.item.height / row_height).floor();
    // Saturating float-to-int cast; NaN (zero-height rows) becomes 0.
    (rows as usize).saturating_sub(1).max(1)
}

/// Position of the active entry in a list. Always within bounds of the length it was last
/// updated with; `None` only for an empty list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor(Option<usize>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Up(usize),
    Down(usize),
    First,
    Last,
}

impl Cursor {
    /// Starts on the first entry.
    pub fn new(len: usize) -> Self {
        Self((len > 0).then_some(0))
    }

    pub fn index(self) -> Option<usize> {
        self.0
    }

    /// Activates `index`, clamped to the last entry.
    pub fn select(&mut self, index: usize, len: usize) {
        self.0 = len.checked_sub(1).map(|last| index.min(last));
    }

    pub fn apply(&mut self, step: Move, len: usize) {
        let Some(last) = len.checked_sub(1) else {
            self.0 = None;
            return;
        };
        let current = self.0.map(|index| index.min(last));
        self.0 = Some(match (step, current) {
            (Move::First, _) | (Move::Down(_), None) => 0,
            (Move::Last, _) | (Move::Up(_), None) => last,
            (Move::Up(by), Some(index)) => index.saturating_sub(by),
            (Move::Down(by), Some(index)) => index.saturating_add(by).min(last),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moved(start: Option<usize>, step: Move, len: usize) -> Option<usize> {
        let mut cursor = Cursor(start);
        cursor.apply(step, len);
        cursor.index()
    }

    #[test]
    fn steps_stop_at_list_bounds() {
        assert_eq!(moved(Some(0), Move::Up(1), 5), Some(0));
        assert_eq!(moved(Some(4), Move::Down(1), 5), Some(4));
        assert_eq!(moved(Some(2), Move::Up(10), 5), Some(0));
        assert_eq!(moved(Some(2), Move::Down(10), 5), Some(4));
        assert_eq!(moved(Some(2), Move::First, 5), Some(0));
        assert_eq!(moved(Some(2), Move::Last, 5), Some(4));
    }

    #[test]
    fn empty_list_has_no_active_entry() {
        assert_eq!(Cursor::new(0).index(), None);
        assert_eq!(moved(Some(3), Move::Down(1), 0), None);

        let mut cursor = Cursor::new(0);
        cursor.select(0, 0);
        assert_eq!(cursor.index(), None);
    }

    #[test]
    fn stale_index_is_clamped_after_list_shrinks() {
        assert_eq!(moved(Some(9), Move::Up(1), 5), Some(3));
        assert_eq!(moved(Some(9), Move::Down(1), 5), Some(4));

        let mut cursor = Cursor::new(5);
        cursor.select(7, 5);
        assert_eq!(cursor.index(), Some(4));
    }
}
