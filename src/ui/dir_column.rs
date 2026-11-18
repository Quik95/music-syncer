use std::ops::Range;

use gpui::{
    AnyElement, Context, SharedString, UniformListScrollHandle, Window, div, prelude::*, rgb,
    uniform_list,
};

use super::{scrollbar::vertical_scrollbar, theme};
use crate::listing::{EntryKind, Listing};

/// Path header above a scrollable list of the directory's entries.
pub struct DirColumn {
    listing: Listing,
    title: SharedString,
    scroll: UniformListScrollHandle,
}

impl DirColumn {
    pub fn new(listing: Listing) -> Self {
        let title = listing.path.display().to_string().into();
        Self {
            listing,
            title,
            scroll: UniformListScrollHandle::new(),
        }
    }

    fn body(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.listing.entries {
            Ok(entries) if entries.is_empty() => div().px_2().child("(empty)").into_any_element(),
            Ok(entries) => {
                let list = uniform_list(
                    "entries",
                    entries.len(),
                    cx.processor(|this, range: Range<usize>, _window, _cx| {
                        let Ok(entries) = &this.listing.entries else {
                            return Vec::new();
                        };
                        let first = range.start;
                        entries[range]
                            .iter()
                            .enumerate()
                            .map(|(offset, entry)| {
                                // Hover state is tracked per element id; without it the
                                // highlight only refreshes on unrelated redraws.
                                let row = div()
                                    .id(first + offset)
                                    .px_2()
                                    .truncate()
                                    .hover(|style| style.bg(rgb(theme::ROW_HOVER)));
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

impl Render for DirColumn {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
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
                    .border_color(rgb(theme::BORDER))
                    .truncate()
                    .child(self.title.clone()),
            )
            .child(div().flex_1().min_h_0().child(self.body(cx)))
    }
}
