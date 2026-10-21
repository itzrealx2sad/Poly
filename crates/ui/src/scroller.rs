use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Div, ElementId, Entity, Interactivity, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, ScrollHandle, ScrollWheelEvent, Stateful,
    StyleRefinement, Window, div, point, px,
};

use crate::button::Button;
use crate::glass::{blurring, glass};
use crate::glide::Glide;
use crate::scrollbar::{Scrollbar, activate_middle_scroll, cancel_middle_scroll};
use crate::theme::ActiveTheme as _;

/// How far a region has to be scrolled before the trip back is worth a button, in rows.
const REACH: f32 = 3.;
/// How far a perched control floats off the bottom of its region.
const PERCH: Pixels = px(12.);

#[derive(IntoElement)]
pub struct Scroller {
    base: Div,
    id: ElementId,
    bar: Entity<Scrollbar>,
    children: Vec<AnyElement>,
    present_surface: bool,
    owns_scroll: bool,
}

impl Scroller {
    #[track_caller]
    pub fn new(id: impl Into<ElementId>, bar: &Entity<Scrollbar>) -> Self {
        Self {
            base: div(),
            id: id.into(),
            bar: bar.clone(),
            children: Vec::new(),
            present_surface: true,
            owns_scroll: true,
        }
    }

    /// A region whose child scrolls itself, a `uniform_list` above all. The surface still
    /// carries the wheel, the middle button and the bar, and leaves the offset to the handle
    /// the child tracks.
    #[track_caller]
    pub fn listing(id: impl Into<ElementId>, bar: &Entity<Scrollbar>) -> Self {
        Self {
            owns_scroll: false,
            ..Self::new(id, bar)
        }
    }

    /// Lets a caller merge the scroll presentation into child transforms, avoiding nested
    /// compositor sampling when those children already have their own spring motion.
    pub fn manual_presentation(mut self) -> Self {
        self.present_surface = false;
        self
    }
}

impl Styled for Scroller {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl InteractiveElement for Scroller {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl ParentElement for Scroller {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Scroller {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            mut base,
            id,
            bar,
            children,
            present_surface,
            owns_scroll,
        } = self;

        let scroll = bar.read(cx).scroll().clone();
        let overrides = std::mem::take(base.style());
        bar.read(cx).sync();
        let presentation = bar.read(cx).presentation();
        let gliding = bar.clone();

        let mut surface = middle_scroll(base, &bar)
            .id(id)
            .size_full()
            .when(owns_scroll, |surface| {
                surface
                    .overflow_y_scroll()
                    .restrict_scroll_to_axis()
                    .track_scroll(&scroll)
            })
            .on_scroll_wheel(move |event: &ScrollWheelEvent, window, cx| {
                match event.delta.precise() {
                    true => gliding.update(cx, |bar, _| bar.stirred()),
                    false => gliding.update(cx, |bar, _| bar.nudge(window)),
                }
            })
            .children(children);

        surface.style().refine(&overrides);
        if present_surface {
            surface = surface.layer_translate(presentation);
        }

        div()
            .relative()
            .size_full()
            .min_h_0()
            .child(surface)
            .child(bar)
    }
}

/// Adds browser-style middle-button auto-scrolling to a scrollable surface. A middle click
/// turns the mode on until the next press of any button. A middle press that is held and
/// dragged scrolls only while held, and `Root` ends it on the release.
pub fn middle_scroll(surface: Div, bar: &Entity<Scrollbar>) -> Div {
    surface
        .capture_any_mouse_down({
            let gliding = bar.clone();
            move |event, window, cx| {
                if event.button == MouseButton::Middle {
                    let started = gliding.update(cx, |bar, cx| {
                        bar.middle_scroll_start(event.position, window, cx)
                    });
                    if started {
                        activate_middle_scroll(&gliding, cx);
                    } else {
                        cancel_middle_scroll(cx);
                    }
                    window.refresh();
                    cx.stop_propagation();
                } else if event.button == MouseButton::Left {
                    if cancel_middle_scroll(cx) {
                        window.refresh();
                        cx.stop_propagation();
                    }
                } else if cancel_middle_scroll(cx) {
                    window.refresh();
                    cx.stop_propagation();
                }
            }
        })
        .on_mouse_move({
            let gliding = bar.clone();
            move |event: &MouseMoveEvent, window, cx| {
                gliding.update(cx, |bar, cx| {
                    bar.middle_scroll_move(event.position, window, cx)
                });
            }
        })
}

#[derive(Clone, Copy)]
struct ShelfDrag {
    origin: Point<Pixels>,
    previous: Point<Pixels>,
    start_time: Instant,
    dragged: bool,
}

pub fn shelf_scroll(surface: Stateful<Div>, scroll: &ScrollHandle, glide: &Glide) -> Stateful<Div> {
    let drag = Rc::new(Cell::new(None::<ShelfDrag>));
    let drag_down = drag.clone();
    let drag_move = drag.clone();
    let drag_up = drag.clone();
    let drag_up_out = drag.clone();
    let scroll_move = scroll.clone();
    let scroll_up = scroll.clone();
    let scroll_up_out = scroll.clone();
    let glide_move = glide.clone();
    let glide_up = glide.clone();
    let glide_up_out = glide.clone();

    surface
        .capture_any_mouse_down(move |event, _, _| {
            if event.button == MouseButton::Left {
                drag_down.set(Some(ShelfDrag {
                    origin: event.position,
                    previous: event.position,
                    start_time: Instant::now(),
                    dragged: false,
                }));
            }
        })
        .on_mouse_move(move |event: &MouseMoveEvent, window, _| {
            let Some(mut state) = drag_move.get() else {
                return;
            };
            let moved = event.position.x - state.origin.x;
            if moved.abs() >= px(4.) || state.dragged {
                state.dragged = true;
                let dx = event.position.x - state.previous.x;
                state.previous = event.position;
                drag_move.set(Some(state));
                let target = (scroll_move.offset().x + dx)
                    .clamp(-scroll_move.max_offset().x, Pixels::ZERO);
                scroll_move.set_offset(point(target, scroll_move.offset().y));
                glide_move.sync(&scroll_move);
                window.refresh();
            }
        })
        .capture_any_mouse_up(move |event, window, cx| {
            if event.button != MouseButton::Left {
                return;
            }
            if let Some(state) = drag_up.take() {
                if state.dragged {
                    cx.stop_propagation();
                    let elapsed = state.start_time.elapsed().as_secs_f32().max(0.016);
                    let velocity = (event.position.x - state.previous.x) / px(1.) / elapsed;
                    if velocity.abs() > 40.0 {
                        let impulse = px(velocity * 0.22);
                        let target = (scroll_up.offset().x + impulse)
                            .clamp(-scroll_up.max_offset().x, Pixels::ZERO);
                        glide_up.aim(&scroll_up, point(target, scroll_up.offset().y), window);
                    } else {
                        glide_up.sync(&scroll_up);
                    }
                    window.refresh();
                }
            }
        })
        .on_mouse_up_out(MouseButton::Left, move |_, window, _| {
            if let Some(state) = drag_up_out.take() {
                if state.dragged {
                    glide_up_out.sync(&scroll_up_out);
                    window.refresh();
                }
            }
        })
}

/// The shape every control that floats over a scrolling region takes: a round bordered pill,
/// centred along the bottom. It swallows clicks meant for it rather than the rows behind, and
/// still lets the wheel through. The caller places it with `bottom_*`.
pub fn perched(button: Button, cx: &App) -> Div {
    let theme = *cx.theme();
    let button = match blurring(cx) {
        true => glass(button, cx),
        false => button.bg(theme.popover),
    };

    div()
        .absolute()
        .bottom(PERCH)
        .w_full()
        .flex()
        .justify_center()
        .child(
            div().flex().flex_none().block_mouse_except_scroll().child(
                button
                    .ghost()
                    .small()
                    .rounded_full()
                    .border_1()
                    .border_color(theme.border),
            ),
        )
}

/// The room a scrolling region has to keep under its last row for a perched control to float in
/// without covering it.
pub fn perch_room(cx: &App) -> Pixels {
    PERCH * 2. + cx.theme().metrics.control_small
}

/// A perched button that glides a scrolling region back to its top. It stays away until the
/// region has been scrolled far enough for the trip to be worth one. The parent has to be
/// `relative`.
pub fn return_top(id: impl Into<ElementId>, bar: &Entity<Scrollbar>, cx: &App) -> Option<Div> {
    return_to(id, bar, Pixels::ZERO, "nav-return-top", cx)
}

/// A perched button that glides a scrolling region back to a resting offset, in either
/// direction. It stays away until the region has drifted far enough from that spot for the trip
/// to be worth one. `goal` is how far down the region should sit, in the positive pixels
/// `Scrollbar::offset` reports, and is turned into gpui's negative offset before it is aimed at.
/// `tooltip` is an i18n key. The parent has to be `relative`.
pub fn return_to(
    id: impl Into<ElementId>,
    bar: &Entity<Scrollbar>,
    goal: Pixels,
    tooltip: &'static str,
    cx: &App,
) -> Option<Div> {
    let viewport = bar.read(cx).viewport();
    let reach = (cx.theme().metrics.list_row * REACH).min(viewport / 2.);
    if viewport <= Pixels::ZERO || (bar.read(cx).offset() - goal).abs() < reach {
        return None;
    }
    let bar = bar.clone();

    Some(perched(
        Button::new(id)
            .secondary()
            .icon("icons/undo-2.svg")
            .tooltip(tooltip)
            .on_click(move |_, window, cx| {
                bar.update(cx, |bar, _| bar.aim(-goal, window));
            }),
        cx,
    ))
}
