//! Fluent horizontal slider for the popup.
//!
//! Rail, accent fill and the two-disc thumb follow WinUI's `Slider`: the inner
//! accent disc grows on hover and shrinks while pressed. Dragging keeps
//! tracking outside the rail (window-level mouse listeners), and the wheel
//! nudges the value.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use gpui::{
    AnyElement, App, Bounds, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, ScrollWheelEvent, SharedString,
    StatefulInteractiveElement, Styled, Window, canvas, div, px, relative,
};

use super::{fx, theme::Palette};

/// Height of the pointer target; the rail is centered in it.
pub(crate) const SLIDER_HEIGHT: f32 = 32.0;
const THUMB: f32 = 20.0;

pub(crate) type Handler = Rc<dyn Fn(u8, &mut Window, &mut App)>;
pub(crate) type HoverHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// Which slider (if any) owns the pointer. Shared by all sliders of a view.
pub(crate) type DragState = Rc<RefCell<Option<SharedString>>>;

pub(crate) struct SliderSpec<'a> {
    pub(crate) id: SharedString,
    pub(crate) value: u8,
    pub(crate) step: u8,
    pub(crate) enabled: bool,
    pub(crate) palette: &'a Palette,
    pub(crate) drag: DragState,
    pub(crate) hovered: bool,
    pub(crate) on_hover: HoverHandler,
    pub(crate) on_change: Handler,
}

pub(crate) fn slider(spec: SliderSpec<'_>, fx: &mut fx::Fx) -> AnyElement {
    let SliderSpec {
        id,
        value,
        step,
        enabled,
        palette,
        drag,
        hovered,
        on_hover,
        on_change,
    } = spec;
    let dragging = drag.borrow().as_ref() == Some(&id);
    // Follow the pointer exactly while dragging; glide for clicks, wheel and
    // external changes.
    let target = f32::from(value.min(100)) / 100.0;
    let fraction = if dragging {
        fx.snap(fx::key(("slider", id.as_ref())), target);
        target
    } else {
        fx.value(fx::key(("slider", id.as_ref())), target, fx::FASTER)
    };
    let inner = fx.value(
        fx::key(("slider-thumb", id.as_ref())),
        if dragging {
            10.0
        } else if hovered && enabled {
            14.0
        } else {
            12.0
        },
        fx::FASTER,
    );
    let accent = if enabled {
        palette.accent
    } else {
        palette.text_tertiary
    };
    let bounds = Rc::new(Cell::new(None::<Bounds<Pixels>>));
    let locate = move |x: Pixels, bounds: Bounds<Pixels>| {
        // The thumb's center travels inside the rail inset by its radius.
        let inset = THUMB / 2.0;
        let width = (f32::from(bounds.size.width) - THUMB).max(1.0);
        let t = ((f32::from(x - bounds.left()) - inset) / width).clamp(0.0, 1.0);
        ((t * 100.0 / f32::from(step)).round() * f32::from(step)).clamp(0.0, 100.0) as u8
    };

    let track_bounds = Rc::clone(&bounds);
    let paint_bounds = Rc::clone(&bounds);
    let down_bounds = Rc::clone(&bounds);
    let down_drag = Rc::clone(&drag);
    let down_change = Rc::clone(&on_change);
    let down_id = id.clone();
    let wheel_change = Rc::clone(&on_change);
    let move_id = id.clone();

    let mut element = div()
        .id(SharedString::from(format!("slider-{id}")))
        .relative()
        .w_full()
        .h(px(SLIDER_HEIGHT))
        .flex_none()
        // Rail.
        .child(
            div()
                .absolute()
                .left(px(THUMB / 2.0))
                .right(px(THUMB / 2.0))
                .top(px(SLIDER_HEIGHT / 2.0 - 2.0))
                .h(px(4.0))
                .rounded_full()
                .bg(palette.control_strong_fill)
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(relative(fraction))
                        .rounded_full()
                        .bg(accent),
                ),
        )
        // Thumb: positioned inside a rail-wide layer so `relative` maps the
        // fraction onto the travel range.
        .child(
            div()
                .absolute()
                .left(px(THUMB / 2.0))
                .right(px(THUMB / 2.0))
                .top(px(SLIDER_HEIGHT / 2.0 - THUMB / 2.0))
                .h(px(THUMB))
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left(relative(fraction))
                        .ml(px(-THUMB / 2.0))
                        .size(px(THUMB))
                        .rounded_full()
                        .bg(palette.control_solid)
                        // ControlElevationBorder: a faint light edge, no
                        // drop shadow, like WinUI's Slider thumb.
                        .border_1()
                        .border_color(palette.control_elevation_border)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(div().size(px(inner)).rounded_full().bg(accent)),
                ),
        );
    if !enabled {
        return element.opacity(0.6).into_any_element();
    }
    element = element
        .on_hover(move |hovered: &bool, window, cx| on_hover(*hovered, window, cx))
        .child(
            canvas(
                move |b, _, _| track_bounds.set(Some(b)),
                move |_, _, window, _| {
                    let move_bounds = Rc::clone(&paint_bounds);
                    let move_drag = Rc::clone(&drag);
                    let move_change = Rc::clone(&on_change);
                    let move_id = move_id.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, _, window, cx| {
                        if move_drag.borrow().as_ref() != Some(&move_id) {
                            return;
                        }
                        if event.pressed_button != Some(MouseButton::Left) {
                            *move_drag.borrow_mut() = None;
                            window.refresh();
                            return;
                        }
                        if let Some(bounds) = move_bounds.get() {
                            move_change(locate(event.position.x, bounds), window, cx);
                        }
                    });
                    let up_drag = Rc::clone(&drag);
                    window.on_mouse_event(move |_: &MouseUpEvent, _, window, _| {
                        if up_drag.borrow_mut().take().is_some() {
                            window.refresh();
                        }
                    });
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
        .on_mouse_down(
            MouseButton::Left,
            move |event: &MouseDownEvent, window, cx| {
                *down_drag.borrow_mut() = Some(down_id.clone());
                if let Some(bounds) = down_bounds.get() {
                    down_change(locate(event.position.x, bounds), window, cx);
                }
                window.refresh();
            },
        )
        .on_scroll_wheel(move |event: &ScrollWheelEvent, window, cx| {
            let delta = f32::from(event.delta.pixel_delta(px(20.0)).y);
            if delta.abs() < 0.5 {
                return;
            }
            let notches = (delta / 20.0).round().clamp(-5.0, 5.0) as i32;
            let notches = if notches == 0 {
                delta.signum() as i32
            } else {
                notches
            };
            let next = (i32::from(value) + notches * i32::from(step)).clamp(0, 100) as u8;
            if next != value {
                wheel_change(next, window, cx);
            }
            cx.stop_propagation();
        });
    element.into_any_element()
}
