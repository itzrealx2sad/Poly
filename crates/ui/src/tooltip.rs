use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    Anchor, Animation, AnimationExt as _, AnyView, App, Context, DispatchPhase, FontWeight,
    MouseMoveEvent, Pixels, Point, SharedString, Window, anchored, canvas, div, ease_out_quint,
    point, px,
};

use crate::metrics::Text;
use crate::theme::ActiveTheme as _;

const MARGIN: Pixels = px(8.);
const OFFSET: Pixels = px(6.);

/// Where a tooltip sits relative to the pointer that summoned it.
#[derive(Clone, Copy, Default, PartialEq)]
pub enum Perch {
    #[default]
    Pointer,
    Above,
    Follow,
    Right,
}

pub struct Tooltip {
    text: SharedString,
    raw: bool,
    perch: Perch,
    at: Point<Pixels>,
}

/// Attaches a tooltip to an element with the show delay its perch calls for.
pub trait Tipped: Sized {
    /// Shows the Fluent string `key` on hover.
    fn tip(self, key: impl Into<SharedString>, perch: Perch) -> Self;
    /// Shows `text` on hover as it is, without a Fluent lookup.
    fn tip_label(self, text: impl Into<SharedString>, perch: Perch) -> Self;
}

impl<E: StatefulInteractiveElement> Tipped for E {
    fn tip(self, key: impl Into<SharedString>, perch: Perch) -> Self {
        timed(self.tooltip(Tooltip::build(key, perch)), perch)
    }

    fn tip_label(self, text: impl Into<SharedString>, perch: Perch) -> Self {
        timed(self.tooltip(Tooltip::label(text, perch)), perch)
    }
}

impl Tooltip {
    pub fn new(key: impl Into<SharedString>, at: Point<Pixels>) -> Self {
        Self {
            text: key.into(),
            raw: false,
            perch: Perch::default(),
            at,
        }
    }

    pub fn perch(mut self, perch: Perch) -> Self {
        self.perch = perch;
        self
    }

    pub fn raw(mut self) -> Self {
        self.raw = true;
        self
    }

    fn build(
        key: impl Into<SharedString>,
        perch: Perch,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        let key = key.into();
        move |window, cx| {
            let at = window.mouse_position();
            cx.new(|_| Self::new(key.clone(), at).perch(perch)).into()
        }
    }

    fn label(
        text: impl Into<SharedString>,
        perch: Perch,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        let text = text.into();
        move |window, cx| {
            let at = window.mouse_position();
            cx.new(|_| Self::new(text.clone(), at).perch(perch).raw())
                .into()
        }
    }
}

impl Render for Tooltip {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let follow = self.perch == Perch::Follow;
        let at = match follow {
            true => window.mouse_position(),
            false => self.at,
        };
        let (position, anchor) = match self.perch {
            Perch::Pointer | Perch::Follow => (at + point(OFFSET, OFFSET), Anchor::TopLeft),
            Perch::Above => (
                point(at.x, at.y - theme.metrics.control_small / 2.),
                Anchor::BottomCenter,
            ),
            Perch::Right => (
                point(at.x + OFFSET + px(8.), at.y),
                Anchor::LeftCenter,
            ),
        };

        let content = div()
            .px(px(10.))
            .py(px(5.))
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_md()
            .text_size(theme.text(Text::Small))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.popover_foreground)
            .child(match self.raw {
                true => self.text.clone(),
                false => i18n::lookup(&self.text, None),
            })
            .when(follow, |this| this.child(tracker()));

        let perch = self.perch;
        let animated = content.with_animation(
            cx.entity_id(),
            Animation::new(Duration::from_millis(150)).with_easing(ease_out_quint()),
            move |this, delta| {
                let t = delta.clamp(0., 1.);
                let (dx, dy, origin) = match perch {
                    Perch::Right => (-8. * (1. - t), 0., point(0.0, 0.5)),
                    Perch::Above => (0., 4. * (1. - t), point(0.5, 1.0)),
                    Perch::Pointer | Perch::Follow => (0., -4. * (1. - t), point(0.0, 0.0)),
                };
                this.opacity(t)
                    .relative()
                    .left(px(dx))
                    .top(px(dy))
                    .layer_scale(0.90 + 0.10 * t)
                    .layer_scale_origin(origin)
            },
        );

        anchored()
            .position(position)
            .anchor(anchor)
            .snap_to_window_with_margin(MARGIN)
            .child(animated)
    }
}

fn timed<E: StatefulInteractiveElement>(element: E, perch: Perch) -> E {
    match perch {
        Perch::Follow => element.tooltip_show_delay(Duration::ZERO),
        Perch::Right => element.tooltip_show_delay(Duration::from_millis(80)),
        Perch::Pointer | Perch::Above => element.tooltip_show_delay(Duration::from_millis(150)),
    }
}

/// Redraws the window on every pointer move so a following tooltip renders at the new position.
fn tracker() -> impl IntoElement {
    canvas(
        |_, _, _| {},
        |_, _, window, _| {
            window.on_mouse_event(|_: &MouseMoveEvent, phase, window, _| {
                if phase == DispatchPhase::Bubble {
                    window.refresh();
                }
            })
        },
    )
    .absolute()
}
