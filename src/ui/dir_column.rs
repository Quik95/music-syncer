use std::{ops::Range, path::PathBuf};

use gpui::{
    AnyElement, App, ClickEvent, Context, FocusHandle, Focusable, MouseButton, NavigationDirection,
    ScrollStrategy, SharedString, Task, UniformListScrollHandle, Window, actions, div, prelude::*,
    rgb, uniform_list,
};

use super::{
    scrollbar::vertical_scrollbar,
    theme,
    track_preview::{self, COVER_PIXELS, Preview},
};
use crate::{
    cursor::{Cursor, Move},
    listing::{EntryKind, Listing},
    track,
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
        OpenSelected,
        OpenParent,
    ]
);

/// Path header above a scrollable list of the directory's entries, with one active entry.
/// An active audio file gets a cover and metadata panel below the list.
pub struct DirColumn {
    listing: Listing,
    title: SharedString,
    scroll: UniformListScrollHandle,
    cursor: Cursor,
    focus_handle: FocusHandle,
    preview: Option<ActivePreview>,
}

/// Preview of the active audio file. While a newly activated file is being read, the
/// previous file's content stays visible to avoid flicker when moving through a list.
struct ActivePreview {
    path: PathBuf,
    content: Option<Preview>,
    _load: Task<()>,
}

impl DirColumn {
    pub fn new(listing: Listing, cx: &mut Context<Self>) -> Self {
        let title = title(&listing);
        let cursor = Cursor::new(entry_count(&listing));
        let mut column = Self {
            listing,
            title,
            scroll: UniformListScrollHandle::new(),
            cursor,
            focus_handle: cx.focus_handle(),
            preview: None,
        };
        column.update_preview(cx);
        column
    }

    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.cursor.select(index, entry_count(&self.listing));
        window.focus(&self.focus_handle, cx);
        self.update_preview(cx);
        cx.notify();
    }

    fn move_cursor(&mut self, step: Move, cx: &mut Context<Self>) {
        self.cursor.apply(step, entry_count(&self.listing));
        if let Some(index) = self.cursor.index() {
            self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
        }
        self.update_preview(cx);
        cx.notify();
    }

    /// Starts reading the active entry's metadata if it is an audio file not yet previewed.
    fn update_preview(&mut self, cx: &mut Context<Self>) {
        let path = self
            .cursor
            .index()
            .and_then(|index| self.listing.file(index))
            .filter(|path| track::is_audio(path));

        if self.preview.as_ref().map(|preview| &preview.path) == path.as_ref() {
            return;
        }

        let Some(path) = path else {
            if let Some(content) = self.preview.take().and_then(|preview| preview.content) {
                release(content, cx);
            }
            return;
        };

        let load = cx.spawn({
            let path = path.clone();
            async move |this, cx| {
                let read_path = path.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move { track::read(&read_path, COVER_PIXELS) })
                    .await;
                let content = match result {
                    Ok(track) => Preview::loaded(Box::new(track)),
                    Err(error) => {
                        tracing::warn!(path = %path.display(), error = format!("{error:#}"), "cannot read track");
                        Preview::Failed(format!("Cannot read metadata: {error:#}"))
                    }
                };
                _ = this.update(cx, |this, cx| {
                    let Some(preview) = this.preview.as_mut().filter(|p| p.path == path) else {
                        return;
                    };
                    if let Some(old) = preview.content.replace(content) {
                        release(old, cx);
                    }
                    cx.notify();
                });
            }
        });

        let content = self.preview.take().and_then(|preview| preview.content);
        self.preview = Some(ActivePreview {
            path,
            content,
            _load: load,
        });
    }

    /// Enters the active entry if it is a directory; files are ignored for now.
    fn open_selected(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self
            .cursor
            .index()
            .and_then(|index| self.listing.subdirectory(index))
        else {
            return;
        };
        let listing = Listing::read(path);
        let first = Cursor::new(entry_count(&listing));
        self.show(listing, first, ScrollStrategy::Top, cx);
    }

    /// Goes to the parent directory with the directory just left as the active entry.
    fn open_parent(&mut self, cx: &mut Context<Self>) {
        let Some(parent) = self.listing.path.parent() else {
            return;
        };
        let listing = Listing::read(parent.to_path_buf());
        let len = entry_count(&listing);
        let mut cursor = Cursor::new(len);
        if let Some(index) = self
            .listing
            .path
            .file_name()
            .and_then(|name| listing.position(name))
        {
            cursor.select(index, len);
        }
        self.show(listing, cursor, ScrollStrategy::Center, cx);
    }

    fn show(
        &mut self,
        listing: Listing,
        cursor: Cursor,
        strategy: ScrollStrategy,
        cx: &mut Context<Self>,
    ) {
        tracing::debug!(path = %listing.path.display(), "opening directory");
        self.title = title(&listing);
        self.listing = listing;
        self.cursor = cursor;
        if let Some(index) = cursor.index() {
            self.scroll.scroll_to_item(index, strategy);
        }

        self.update_preview(cx);
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
                                    move |this, event: &ClickEvent, window, cx| {
                                        this.select(index, window, cx);
                                        if event.click_count() == 2 {
                                            this.open_selected(cx);
                                        }
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

fn title(listing: &Listing) -> SharedString {
    listing.path.display().to_string().into()
}

fn entry_count(listing: &Listing) -> usize {
    listing.entries.as_ref().map_or(0, Vec::len)
}

/// Frees the preview's cover from the sprite atlas. Deferred because the window being
/// updated is detached from the app until the update ends, so it would be skipped.
fn release(mut preview: Preview, cx: &mut App) {
    if let Some(cover) = preview.take_cover() {
        cx.defer(move |cx| cx.drop_image(cover, None));
    }
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
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Back),
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus_handle, cx);
                    this.open_parent(cx);
                }),
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
            .on_action(cx.listener(|this, _: &OpenSelected, _, cx| {
                this.open_selected(cx);
            }))
            .on_action(cx.listener(|this, _: &OpenParent, _, cx| {
                this.open_parent(cx);
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
            .children(
                self.preview
                    .as_ref()
                    .and_then(|preview| preview.content.as_ref())
                    .map(track_preview::render),
            )
    }
}
