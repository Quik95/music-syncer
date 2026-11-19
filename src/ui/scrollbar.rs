use std::{cell::Cell, rc::Rc};

use gpui::{
    App, Bounds, DispatchPhase, Element, ElementId, GlobalElementId, HitboxBehavior,
    InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, ScrollEvent, ScrollHandle, Style, Window, fill, point, px, size,
};

use super::theme;

const WIDTH: Pixels = px(10.);
const MIN_THUMB_HEIGHT: Pixels = px(24.);

/// Vertical scrollbar driving `handle`. Fills the height it is laid out with; renders an empty
/// track when the content fits. Supports dragging the thumb, clicking the track and wheel scrolling.
pub fn vertical_scrollbar(id: impl Into<ElementId>, handle: ScrollHandle) -> VerticalScrollbar {
    VerticalScrollbar {
        id: id.into(),
        handle,
    }
}

pub struct VerticalScrollbar {
    id: ElementId,
    handle: ScrollHandle,
}

/// Distance from the thumb top to the pointer while the thumb is dragged.
type DragGrab = Rc<Cell<Option<Pixels>>>;

#[derive(Clone, Copy)]
struct Geometry {
    track: Bounds<Pixels>,
    thumb: Bounds<Pixels>,
    max_scroll: Pixels,
}

impl Geometry {
    fn new(track: Bounds<Pixels>, handle: &ScrollHandle) -> Option<Self> {
        let max_scroll = handle.max_offset().y;
        let viewport = handle.bounds().size.height;
        if max_scroll <= px(0.) || viewport <= px(0.) {
            return None;
        }
        let thumb_height = (track.size.height * (viewport / (viewport + max_scroll)))
            .max(MIN_THUMB_HEIGHT)
            .min(track.size.height);
        let travel = track.size.height - thumb_height;
        let progress = (-handle.offset().y / max_scroll).clamp(0., 1.);
        let thumb = Bounds::new(
            point(track.origin.x, track.origin.y + travel * progress),
            size(track.size.width, thumb_height),
        );
        Some(Self {
            track,
            thumb,
            max_scroll,
        })
    }

    /// Scroll so that the thumb top lands `grab` above the pointer.
    fn scroll_to_pointer(&self, handle: &ScrollHandle, pointer_y: Pixels, grab: Pixels) {
        let travel = self.track.size.height - self.thumb.size.height;
        if travel <= px(0.) {
            return;
        }
        let thumb_top = (pointer_y - self.track.origin.y - grab).clamp(px(0.), travel);
        let offset = handle.offset();
        handle.set_offset(point(offset.x, -(self.max_scroll * (thumb_top / travel))));
    }
}

impl IntoElement for VerticalScrollbar {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for VerticalScrollbar {
    type RequestLayoutState = ();
    type PrepaintState = Option<gpui::Hitbox>;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let style = Style {
            flex_shrink: 0.,
            size: size(WIDTH.into(), gpui::relative(1.).into()),
            ..Style::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        Some(window.insert_hitbox(bounds, HitboxBehavior::Normal))
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        _cx: &mut App,
    ) {
        let (Some(id), Some(hitbox)) = (id, hitbox.take()) else {
            return;
        };
        let grab = window.with_element_state(id, |grab: Option<DragGrab>, _| {
            let grab = grab.unwrap_or_default();
            (grab.clone(), grab)
        });

        window.paint_quad(fill(bounds, *theme::SCROLLBAR_TRACK));
        let Some(geometry) = Geometry::new(bounds, &self.handle) else {
            grab.set(None);
            return;
        };
        let hovered = hitbox.is_hovered(window);
        let thumb_color = if hovered || grab.get().is_some() {
            *theme::SCROLLBAR_THUMB_ACTIVE
        } else {
            *theme::SCROLLBAR_THUMB
        };
        window.paint_quad(fill(geometry.thumb, thumb_color).corner_radii(WIDTH / 2.));

        window.on_mouse_event({
            let (handle, grab, hitbox) = (self.handle.clone(), grab.clone(), hitbox.clone());
            move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble
                    || event.button != MouseButton::Left
                    || !hitbox.is_hovered(window)
                {
                    return;
                }
                let offset = if geometry.thumb.contains(&event.position) {
                    event.position.y - geometry.thumb.origin.y
                } else {
                    let centered = geometry.thumb.size.height / 2.;
                    geometry.scroll_to_pointer(&handle, event.position.y, centered);
                    centered
                };
                grab.set(Some(offset));
                cx.stop_propagation();
                window.refresh();
            }
        });
        window.on_mouse_event({
            let (handle, grab, hitbox) = (self.handle.clone(), grab.clone(), hitbox.clone());
            move |event: &MouseMoveEvent, phase, window, _cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                match grab.get() {
                    Some(offset) if event.dragging() => {
                        geometry.scroll_to_pointer(&handle, event.position.y, offset);
                        window.refresh();
                    }
                    _ if hitbox.is_hovered(window) != hovered => window.refresh(),
                    _ => {}
                }
            }
        });
        window.on_mouse_event({
            let grab = grab.clone();
            move |_: &MouseUpEvent, phase, window, _cx| {
                if phase == DispatchPhase::Bubble && grab.take().is_some() {
                    window.refresh();
                }
            }
        });
        window.on_mouse_event({
            let handle = self.handle.clone();
            move |event: &ScrollEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || !hitbox.should_handle_scroll(window) {
                    return;
                }
                let delta = event.delta.pixel_delta(window.line_height()).y;
                let offset = handle.offset();
                let y = (offset.y + delta).clamp(-geometry.max_scroll, px(0.));
                handle.set_offset(point(offset.x, y));
                cx.stop_propagation();
                window.refresh();
            }
        });
    }
}
