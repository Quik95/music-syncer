use std::ops::Range;

use gpui::{
    AnyElement, App, ClickEvent, Context, FocusHandle, Focusable, MouseButton, ScrollStrategy,
    SharedString, UniformListScrollHandle, Window, actions, div, prelude::*, rgb, uniform_list,
};

use super::{scrollbar::vertical_scrollbar, theme};
use crate::{
    cursor::{Cursor, Move},
    listing::{EntryKind, Listing},
};

pub const KEY_CONTEXT: &str = "DirColumn";

actions!(
    dir_column,
    [
        SelectPrevious,
        SelectNext,
        SelectPreviousPage,
        SelectNextPage,
        SelectFirst,
        SelectLast,
    ]
);

/// Path header above a scrollable list of the directory's entries, with one active entry.
pub struct DirColumn {
    listing: Listing,
    title: SharedString,
    scroll: UniformListScrollHandle,
    cursor: Cursor,
    focus_handle: FocusHandle,
}

impl DirColumn {
    pub fn new(listing: Listing, cx: &mut Context<Self>) -> Self {
        let title = listing.path.display().to_string().into();
        let cursor = Cursor::new(entry_count(&listing));
        Self {
            listing,
            title,
            scroll: UniformListScrollHandle::new(),
            cursor,
            focus_handle: cx.focus_handle(),
        }
    }

    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.cursor.select(index, entry_count(&self.listing));
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn move_cursor(&mut self, step: Move, cx: &mut Context<Self>) {
        self.cursor.apply(step, entry_count(&self.listing));
        if let Some(index) = self.cursor.index() {
            self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    /// Rows fully visible in the viewport during the last layout, at least one.
    fn page_rows(&self) -> usize {
        let len = entry_count(&self.listing);
        let state = self.scroll.0.borrow();
        let Some(size) = state.last_item_size.filter(|_| len > 0) else {
            return 1;
        };
        let row_height = size.contents.height / len as f32;
        let rows = (size.item.height / row_height).floor();
        // Saturating float-to-int cast; NaN (zero-height rows) becomes 0.
        (rows as usize).saturating_sub(1).max(1)
    }

    fn body(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.listing.entries {
            Ok(entries) if entries.is_empty() => div().px_2().child("(empty)").into_any_element(),
            Ok(entries) => {
                let list = uniform_list(
                    "entries",
                    entries.len(),
                    cx.processor(|this, range: Range<usize>, window, cx| {
                        let Ok(entries) = &this.listing.entries else {
                            return Vec::new();
                        };
                        let active = this.cursor.index();
                        let active_bg = if this.focus_handle.is_focused(window) {
                            theme::ROW_ACTIVE
                        } else {
                            theme::ROW_ACTIVE_UNFOCUSED
                        };
                        let first = range.start;
                        entries[range]
                            .iter()
                            .enumerate()
                            .map(|(offset, entry)| {
                                let index = first + offset;
                                // Hover state is tracked per element id; without it the
                                // highlight only refreshes on unrelated redraws.
                                let row = div().id(index).px_2().truncate().on_click(cx.listener(
                                    move |this, _: &ClickEvent, window, cx| {
                                        this.select(index, window, cx);
                                    },
                                ));
                                let row = if active == Some(index) {
                                    row.bg(rgb(active_bg))
                                } else {
                                    row.hover(|style| style.bg(rgb(theme::ROW_HOVER)))
                                };
                                match entry.kind {
                                    EntryKind::Directory => row
                                        .text_color(rgb(theme::DIRECTORY))
                                        .child(format!("{}/", entry.name)),
                                    EntryKind::File => row.child(entry.name.clone()),
                                }
                            })
                            .collect()
                    }),
                )
                .flex_1()
                .min_w_0()
                .h_full()
                .track_scroll(&self.scroll);
                let scrollbar =
                    vertical_scrollbar("scrollbar", self.scroll.0.borrow().base_handle.clone());
                div()
                    .flex()
                    .size_full()
                    .child(list)
                    .child(scrollbar)
                    .into_any_element()
            }
            Err(error) => div()
                .px_2()
                .text_color(rgb(theme::ERROR))
                .child(format!("Cannot read directory: {error}"))
                .into_any_element(),
        }
    }
}

fn entry_count(listing: &Listing) -> usize {
    listing.entries.as_ref().map_or(0, Vec::len)
}

impl Focusable for DirColumn {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DirColumn {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header_border = if self.focus_handle.is_focused(window) {
            theme::FOCUS
        } else {
            theme::BORDER
        };
        div()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| window.focus(&this.focus_handle, cx)),
            )
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                this.move_cursor(Move::Up(1), cx);
            }))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                this.move_cursor(Move::Down(1), cx);
            }))
            .on_action(cx.listener(|this, _: &SelectPreviousPage, _, cx| {
                this.move_cursor(Move::Up(this.page_rows()), cx);
            }))
            .on_action(cx.listener(|this, _: &SelectNextPage, _, cx| {
                this.move_cursor(Move::Down(this.page_rows()), cx);
            }))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| {
                this.move_cursor(Move::First, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| {
                this.move_cursor(Move::Last, cx);
            }))
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex_none()
                    .px_2()
                    .py_1()
                    .bg(rgb(theme::HEADER_BACKGROUND))
                    .border_b_1()
                    .border_color(rgb(header_border))
                    .truncate()
                    .child(self.title.clone()),
            )
            .child(div().flex_1().min_h_0().child(self.body(cx)))
    }
}
