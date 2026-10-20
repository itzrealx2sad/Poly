// SPDX-License-Identifier: GPL-3.0-or-later
// Portions Copyright (C) Sonora Contributors
// Modifications Copyright (C) 2026 Poly Project Contributors

use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{Context, Entity, Pixels, Render, StyleRefinement, Window, div, point, px};
use state::{AppSettings, Playback, Queue, SideTab, Sonora};
use ui::{ActiveTheme as _, MIN_CONTENT, Panel, Room, Side};

use crate::chrome::Aside;

const MIN_WIDTH: Pixels = px(240.);
const MAX_WIDTH: Pixels = px(560.);

fn fills_content(width: Pixels) -> bool {
    !Room::of(width).fits(Room::Wide)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MotionPhase {
    Entering,
    Exiting,
}

#[derive(Clone, Copy)]
struct SidebarTransition {
    started: Instant,
    span: Duration,
    phase: MotionPhase,
}

impl SidebarTransition {
    fn new(phase: MotionPhase, prev: Option<&Self>) -> Self {
        let span = match phase {
            MotionPhase::Entering => Duration::from_millis(200),
            MotionPhase::Exiting => Duration::from_millis(160),
        };
        let started = match prev {
            Some(prev) if prev.phase != phase && prev.running() => {
                let prev_p = prev.progress();
                let carry = 1.0 - prev_p;
                Instant::now() - span.mul_f32(carry.clamp(0.0, 1.0))
            }
            _ => Instant::now(),
        };
        Self {
            started,
            span,
            phase,
        }
    }

    fn running(&self) -> bool {
        self.started.elapsed() < self.span
    }

    fn progress(&self) -> f32 {
        if self.span.is_zero() {
            return 1.;
        }
        (self.started.elapsed().as_secs_f32() / self.span.as_secs_f32()).clamp(0., 1.)
    }

    fn scale(&self) -> f32 {
        let p = self.progress();
        match self.phase {
            MotionPhase::Entering => {
                let t = 1.0 - p;
                0.92 + 0.08 * (1.0 - t * t * t)
            }
            MotionPhase::Exiting => {
                let t = 1.0 - p;
                0.92 + 0.08 * t * t
            }
        }
    }

    fn opacity(&self) -> f32 {
        let p = self.progress();
        match self.phase {
            MotionPhase::Entering => p.clamp(0.0, 1.0),
            MotionPhase::Exiting => (1.0 - p).clamp(0.0, 1.0),
        }
    }
}

pub(crate) struct SidebarRight {
    aside: Entity<Aside>,
    settings: Entity<AppSettings>,
    width: Pixels,
    open: bool,
    transition: Option<SidebarTransition>,
}

impl SidebarRight {
    pub(crate) fn new(
        queue: Entity<Queue>,
        playback: Entity<Playback>,
        cx: &mut Context<Self>,
    ) -> Self {
        let settings = Sonora::global(cx).settings.clone();
        let width = px(settings.read(cx).sidebar_right_width()).clamp(MIN_WIDTH, MAX_WIDTH);
        let open = settings.read(cx).sidebar_right_open();
        let tab = settings.read(cx).sidebar_right_tab();
        let aside = cx.new(|cx| Aside::new(queue, playback, tab, cx));

        Self {
            aside,
            settings,
            width,
            open,
            transition: None,
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open || self.transition.is_some()
    }

    pub(crate) fn is_visible(&self) -> bool {
        self.open || self.transition.is_some()
    }

    pub(crate) fn is_transitioning(&self) -> bool {
        self.transition.as_ref().map_or(false, |t| t.running())
    }

    pub(crate) fn factor(&self) -> f32 {
        match self.transition {
            Some(ref transition) => {
                let p = transition.progress();
                match transition.phase {
                    MotionPhase::Entering => {
                        let t = 1.0 - p;
                        1.0 - t * t * t
                    }
                    MotionPhase::Exiting => {
                        let t = 1.0 - p;
                        t * t * t
                    }
                }
            }
            None => if self.open { 1.0 } else { 0.0 },
        }
    }

    pub(crate) fn width(&self) -> Pixels {
        self.width
    }

    pub(crate) fn available(window: &Window) -> bool {
        !fills_content(window.viewport_size().width)
    }

    pub(crate) fn covers_content(&self, _window: &Window) -> bool {
        false
    }

    #[allow(dead_code)]
    pub(crate) fn occupied_width(&self, window: &Window) -> Pixels {
        match self.open && Self::available(window) {
            false => Pixels::ZERO,
            true => self.width,
        }
    }

    /// The width the left sidebar has to leave free for this one. It counts an open sidebar
    /// even while the window is too narrow to show it, so the left sidebar collapses first and
    /// stays collapsed when this one hides in turn.
    pub(crate) fn reserved_width(&self) -> Pixels {
        match self.open {
            false => Pixels::ZERO,
            true => self.width,
        }
    }

    pub(crate) fn toggle(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.close(cx);
        } else {
            let tab = self.aside.read(cx).tab();
            self.show(tab, cx);
        }
    }

    pub(crate) fn show(&mut self, tab: SideTab, cx: &mut Context<Self>) {
        if self.open && self.aside.read(cx).tab() == tab && self.transition.as_ref().map_or(true, |t| t.phase == MotionPhase::Entering) {
            self.close(cx);
            return;
        }
        if self.aside.read(cx).tab() != tab {
            self.settings
                .update(cx, |settings, cx| settings.set_sidebar_right_tab(tab, cx));
        }
        self.aside.update(cx, |aside, cx| aside.show(tab, cx));
        self.open = true;
        if cx.reduce_motion() {
            self.transition = None;
        } else {
            self.transition = Some(SidebarTransition::new(MotionPhase::Entering, self.transition.as_ref()));
        }
        self.remember(cx);
        cx.notify();
    }

    pub(crate) fn close(&mut self, cx: &mut Context<Self>) {
        if !self.open && self.transition.is_none() {
            return;
        }
        self.open = false;
        if cx.reduce_motion() {
            self.transition = None;
            self.aside.update(cx, |aside, cx| aside.dismiss(cx));
        } else {
            self.transition = Some(SidebarTransition::new(MotionPhase::Exiting, self.transition.as_ref()));
        }
        self.remember(cx);
        cx.notify();
    }

    fn remember(&self, cx: &mut Context<Self>) {
        let open = self.open;
        self.settings
            .update(cx, |settings, cx| settings.set_sidebar_right_open(open, cx));
    }

    fn persist(&self, cx: &mut Context<Self>) {
        let width = self.width / px(1.);
        self.settings.update(cx, |settings, cx| {
            settings.set_sidebar_right_width(width, cx)
        });
    }
}

impl Render for SidebarRight {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.is_open() || !Self::available(window) {
            return div().into_any_element();
        }

        let theme = *cx.theme();
        let me = cx.entity_id();
        let (opacity, scale) = match self.transition {
            Some(transition) if transition.running() => {
                cx.on_next_frame(window, |_, _, cx| cx.notify());
                (transition.opacity(), transition.scale())
            }
            Some(transition) if transition.phase == MotionPhase::Exiting => {
                self.transition = None;
                self.aside.update(cx, |aside, cx| aside.dismiss(cx));
                window.defer(cx, move |_, cx| cx.notify(me));
                return div().into_any_element();
            }
            Some(_) => {
                self.transition = None;
                cx.notify();
                (1., 1.)
            }
            None => (1., 1.),
        };

        let aside = match opacity < 1. {
            true => self.aside.clone().into_any_element(),
            false => self
                .aside
                .clone()
                .cached(StyleRefinement::default().size_full())
                .into_any_element(),
        };

        let panel = Panel::new("sidebar-right", Side::Right, self.width)
            .limits(MIN_WIDTH, MAX_WIDTH)
            .reach(super::cap(MIN_WIDTH, MAX_WIDTH, MIN_CONTENT, window));

        let panel = if self.transition.is_none() {
            panel.on_resize(cx.listener(|this, width: &Pixels, _, cx| {
                this.width = *width;
                this.persist(cx);
                cx.notify();
            }))
        } else {
            panel
        };

        let ambient = crate::shared::ambient::shown(cx) && self.settings.read(cx).adaptive_theme();
        let is_trans = self.is_transitioning();

        panel
            .rounded(px(16.))
            .border_1()
            .border_color(theme.border)
            .when_else(
                ambient || ui::blurring(cx),
                |this| ui::glass(this, cx),
                |this| this.bg(theme.secondary),
            )
            .shadow_lg()
            .overflow_hidden()
            .when(is_trans, |this| {
                this.layer_scale(scale)
                    .layer_scale_origin(point(0.5, 0.5))
                    .opacity(opacity)
            })
            .child(aside)
            .into_any_element()
    }
}
