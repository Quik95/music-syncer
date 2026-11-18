use gpui::{AppContext as _, Context, Entity, Window, div, prelude::*, rgb};

use super::{dir_column::DirColumn, theme};
use crate::listing::Listing;

/// Two directory listings side by side.
pub struct Browser {
    left: Entity<DirColumn>,
    right: Entity<DirColumn>,
}

impl Browser {
    pub fn new(left: Listing, right: Listing, cx: &mut Context<Self>) -> Self {
        Self {
            left: cx.new(|_| DirColumn::new(left)),
            right: cx.new(|_| DirColumn::new(right)),
        }
    }
}

impl Render for Browser {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .size_full()
            .bg(rgb(theme::BACKGROUND))
            .text_color(rgb(theme::TEXT))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .border_r_1()
                    .border_color(rgb(theme::BORDER))
                    .child(self.left.clone()),
            )
            .child(div().flex_1().min_w_0().child(self.right.clone()))
    }
}
