use std::ops::Range;

use gpui::{AnyElement, Context, SharedString, Window, div, prelude::*, rgb, uniform_list};

use super::theme;
use crate::listing::{EntryKind, Listing};

/// Path header above a scrollable list of the directory's entries.
pub struct DirColumn {
    listing: Listing,
    title: SharedString,
}

impl DirColumn {
    pub fn new(listing: Listing) -> Self {
        let title = listing.path.display().to_string().into();
        Self { listing, title }
    }

    fn body(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.listing.entries {
            Ok(entries) if entries.is_empty() => div().px_2().child("(empty)").into_any_element(),
            Ok(entries) => uniform_list(
                "entries",
                entries.len(),
                cx.processor(|this, range: Range<usize>, _window, _cx| {
                    let Ok(entries) = &this.listing.entries else {
                        return Vec::new();
                    };
                    entries[range]
                        .iter()
                        .map(|entry| match entry.kind {
                            EntryKind::Directory => div()
                                .px_2()
                                .truncate()
                                .text_color(rgb(theme::DIRECTORY))
                                .child(format!("{}/", entry.name)),
                            EntryKind::File => div().px_2().truncate().child(entry.name.clone()),
                        })
                        .collect()
                }),
            )
            .size_full()
            .into_any_element(),
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
