// SPDX-License-Identifier: GPL-3.0-or-later
// Portions Copyright (C) Sonora Contributors
// Modifications Copyright (C) 2026 Poly Project Contributors

use router::{Destination, Link};
use std::time::Duration;
use ui::ActiveTheme as _;

use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt, ClickEvent, Context, Entity, FontWeight, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ScrollWheelEvent, SharedString, Window,
    div, px,
};
use i18n::t;
use input::{ToggleFullscreen, ToggleLyrics, ToggleQueue};
use state::{AppSettings, Playback, Queue, SideTab, Sonora};
use ui::{
    Artwork, Button, ExplicitBadge, Popup, Room, Scrollbar, Scrubber, ScrubberState, clock,
    tabular,
};

use crate::shared::menus::ItemMenu;
use crate::shared::transport::{NOTCH, like, moved, percent, transport, volume_icon};

const VOLUME_TIGHT: f32 = 72.;
const CLOCK_SHORT: f32 = 3.4;
const CLOCK_LONG: f32 = 5.4;

pub(crate) struct PlayerBar {
    playback: Entity<Playback>,
    queue: Entity<Queue>,
    settings: Entity<AppSettings>,
    track_menu: ItemMenu,
    context_menu: Option<(music::Track, Point<Pixels>)>,
    seek: ScrubberState,
    volume: ScrubberState,
    pending: Option<f32>,
    over_seek: Option<f32>,
    over_volume: Option<f32>,
    volume_held: bool,
    muted: Option<f32>,
}

impl PlayerBar {
    pub fn new(playback: Entity<Playback>, queue: Entity<Queue>, cx: &mut Context<Self>) -> Self {
        let library = Sonora::global(cx).library.clone();
        let settings = Sonora::global(cx).settings.clone();
        cx.observe(&playback, |_, _, cx| cx.notify()).detach();
        cx.observe(&queue, |_, _, cx| cx.notify()).detach();
        cx.observe(&library, |_, _, cx| cx.notify()).detach();
        cx.observe(&settings, |_, _, cx| cx.notify()).detach();

        let me = cx.entity_id();
        let playlist_scrollbar = cx.new(|_| Scrollbar::inset().watching(me));

        Self {
            playback,
            queue,
            settings,
            track_menu: ItemMenu::new(playlist_scrollbar, cx),
            context_menu: None,
            seek: ScrubberState::new("seek"),
            volume: ScrubberState::new("volume"),
            pending: None,
            over_seek: None,
            over_volume: None,
            volume_held: false,
            muted: None,
        }
    }

    fn open_context_menu(
        &mut self,
        track: music::Track,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.track_menu.reset(cx);
        self.context_menu = Some((track, position));
        cx.notify();
    }

    /// Opens the track menu on whatever is playing when the click lands, so a render does not
    /// have to hold a copy of the track for a menu that is rarely asked for.
    fn open_playing_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(track) = self.playback.read(cx).track().cloned() else {
            return;
        };
        self.open_context_menu(track, position, cx);
    }

    fn commit_seek(&mut self, cx: &mut Context<Self>) {
        let Some(fraction) = self.pending.take() else {
            return;
        };
        self.playback
            .update(cx, |playback, cx| playback.seek_fraction(fraction, cx));
    }

    fn hover(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let pad = cx.theme().metrics.pad;
        let seek = self.seek.hovered(event.position, pad);
        let volume = self.volume.hovered(event.position, pad);

        if moved(self.over_seek, seek) || moved(self.over_volume, volume) {
            self.over_seek = seek;
            self.over_volume = volume;
            cx.notify();
        }
    }

    fn turn_volume(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta = event.delta.pixel_delta(window.line_height()).y;
        if delta == Pixels::ZERO {
            return;
        }
        cx.stop_propagation();

        let notch = match delta > Pixels::ZERO {
            true => NOTCH,
            false => -NOTCH,
        };
        let level = (self.playback.read(cx).volume() + notch).clamp(0., 1.);
        self.muted = None;
        self.playback
            .update(cx, |playback, cx| playback.set_volume(level, cx));
    }

    fn sound(&self, width: Pixels, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let empty = theme.muted_foreground.opacity(0.3);
        let level = self.playback.read(cx).volume();
        let showing = self.over_volume.is_some() || self.volume_held;
        let bubble = showing.then(|| (level, percent(level)));
        let restore = self.muted.unwrap_or(0.7);

        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .on_scroll_wheel(cx.listener(Self::turn_volume))
            .child(
                Button::new("volume")
                    .ghost()
                    .small()
                    .icon(volume_icon(level))
                    .tooltip_above(match level <= 0.001 {
                        true => "player-unmute",
                        false => "player-mute",
                    })
                    .tint(theme.muted_foreground)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let wanted = match level <= 0.001 {
                            true => restore,
                            false => 0.,
                        };
                        this.muted = match wanted {
                            0. => Some(level),
                            _ => None,
                        };
                        this.playback
                            .update(cx, |playback, cx| playback.set_volume(wanted, cx));
                    })),
            )
            .child(
                div().w(width).flex_none().child(
                    Scrubber::new(&self.volume, level)
                        .colors(theme.progress_bar, empty, theme.foreground)
                        .when_some(bubble, |this, (at, text)| this.bubble(at, text))
                        .on_move(cx.listener(|this, fraction: &f32, _, cx| {
                            let level = *fraction;
                            this.volume_held = true;
                            this.muted = None;
                            this.playback
                                .update(cx, |playback, cx| playback.set_volume(level, cx));
                        }))
                        .on_release(cx.listener(|this, _: &MouseUpEvent, _, cx| {
                            this.volume_held = false;
                            cx.notify();
                        })),
                ),
            )
    }

    fn fullscreen_button(&self) -> Button {
        Button::new("toggle-fullscreen")
            .ghost()
            .small()
            .icon("icons/maximize.svg")
            .tooltip_above("player-fullscreen")
            .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleFullscreen), cx))
    }
}

impl PlayerBar {
    pub(crate) fn height(window: &Window, _cx: &gpui::App) -> Pixels {
        let span = Room::of(window.viewport_size().width);
        match !span.fits(Room::Roomy) {
            true => ui::snapped(px(112.), window),
            false => ui::snapped(px(88.), window),
        }
    }
}

impl Render for PlayerBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let muted = theme.muted_foreground;
        let empty = muted.opacity(0.3);
        let span = Room::of(window.viewport_size().width);
        let stacked = !span.fits(Room::Roomy);
        let height = Self::height(window, cx);
        let clock_text = theme.text(ui::Text::Tiny);

        let playback = self.playback.read(cx);
        let seekable = playback.track().is_some();
        let progress = self.pending.unwrap_or_else(|| playback.progress());
        let elapsed = playback.position();
        let total = playback
            .track()
            .map(|track| track.duration)
            .unwrap_or(Duration::ZERO);
        let clock_width = clock_text
            * match total.as_secs() >= 3600 {
                true => CLOCK_LONG,
                false => CLOCK_SHORT,
            };

        let seek_bubble = self
            .over_seek
            .or(self.pending)
            .map(|at| (at, clock(total.mul_f32(at))));

        let clock_label = |value: Duration, align_end: bool| {
            div()
                .child(clock(value))
                .w(clock_width)
                .flex_none()
                .whitespace_nowrap()
                .text_size(clock_text)
                .font_features(tabular())
                .text_color(muted)
                .when_else(align_end, |this| this.text_right(), |this| this.text_left())
        };

        let seek = div()
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .child(clock_label(elapsed, true))
            .child(
                div().flex_1().min_w_0().child(
                    Scrubber::new(&self.seek, progress)
                        .colors(theme.progress_bar, empty, theme.foreground)
                        .enabled(seekable)
                        .when_some(seek_bubble, |this, (at, text)| this.bubble(at, text))
                        .on_move(cx.listener(|this, fraction: &f32, _, cx| {
                            this.pending = Some(*fraction);
                            cx.notify();
                        }))
                        .on_release(
                            cx.listener(|this, _: &MouseUpEvent, _, cx| this.commit_seek(cx)),
                        ),
                ),
            )
            .child(clock_label(total, false));

        let track = playback.track();
        let cover = track.and_then(|track| track.cover.clone());
        let explicit = track.is_some_and(|track| track.explicit);
        let like = like(track, &self.playback, cx);

        let left_section = div()
            .flex()
            .items_center()
            .gap(px(16.))
            .flex_1()
            .min_w_0()
            .pr(px(16.))
            .child(
                div()
                    .id("now-playing-artwork")
                    .size(px(56.))
                    .flex_none()
                    .overflow_hidden()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(theme.border.opacity(0.4))
                    .when_some(
                        track.and_then(|track| track.album_id.clone()),
                        |this, album| this.link(Destination::Album(album.into())),
                    )
                    .when(track.is_some(), |this| {
                        this.on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                window.prevent_default();
                                this.open_playing_menu(event.position, cx);
                            }),
                        )
                    })
                    .child(Artwork::new(cover).size(px(56.))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap_1()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .min_w_0()
                            .child(match track {
                                Some(t) => {
                                    let name_len = t.name.chars().count();
                                    let reduce_motion = cx.reduce_motion();
                                    if name_len > 22 && !reduce_motion {
                                        let slide_distance =
                                            px(((name_len - 20) as f32 * 8.5).min(280.));
                                        div()
                                            .id("now-playing-track-marquee-container")
                                            .overflow_hidden()
                                            .max_w(px(220.))
                                            .child(
                                                div()
                                                    .id("now-playing-track")
                                                    .flex()
                                                    .whitespace_nowrap()
                                                    .child(SharedString::from(t.name.clone()))
                                                    .text_size(theme.text(ui::Text::Body))
                                                    .font_weight(FontWeight::BOLD)
                                                    .text_color(theme.foreground)
                                                    .with_animation(
                                                        SharedString::from(format!("track-marquee-{}", t.name)),
                                                        Animation::new(Duration::from_secs(10)).repeat(),
                                                        move |this, delta| {
                                                            let offset = if delta < 0.30 {
                                                                Pixels::ZERO
                                                            } else if delta < 0.70 {
                                                                let progress = (delta - 0.30) / 0.40;
                                                                let eased = (1.0 - (progress * std::f32::consts::PI).cos()) / 2.0;
                                                                -slide_distance * eased
                                                            } else if delta < 0.90 {
                                                                -slide_distance
                                                            } else {
                                                                let progress = (delta - 0.90) / 0.10;
                                                                let eased = (1.0 - (progress * std::f32::consts::PI).cos()) / 2.0;
                                                                -slide_distance * (1.0 - eased)
                                                            };
                                                            this.ml(offset)
                                                        },
                                                    ),
                                            )
                                    } else {
                                        div()
                                            .id("now-playing-track")
                                            .child(SharedString::from(t.name.clone()))
                                            .text_size(theme.text(ui::Text::Body))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(theme.foreground)
                                            .truncate()
                                    }
                                }
                                None => div()
                                    .id("now-playing-idle")
                                    .child(t!("player-nothing-playing"))
                                    .text_size(theme.text(ui::Text::Body))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(muted)
                                    .truncate(),
                            })
                            .when_some(track, |this, t| {
                                let album_text = if t.album.is_empty() {
                                    t.name.clone()
                                } else {
                                    t.album.clone()
                                };
                                this.child(
                                    div()
                                        .child("·")
                                        .text_color(muted)
                                        .text_size(theme.text(ui::Text::Small)),
                                )
                                .child(
                                    div()
                                        .child(SharedString::from(format!(
                                            "{} · {}",
                                            album_text, t.artists
                                        )))
                                        .text_size(theme.text(ui::Text::Small))
                                        .text_color(muted)
                                        .truncate(),
                                )
                            })
                            .when(explicit, |this| {
                                this.child(div().flex_none().child(ExplicitBadge::new()))
                            })
                            .child(like),
                    )
                    .child(seek),
            );

        let (open, tab) = {
            let settings = self.settings.read(cx);
            (settings.sidebar_right_open(), settings.sidebar_right_tab())
        };
        let lyrics_active = open && tab == SideTab::Lyrics;
        let queue_active = open && tab == SideTab::Queue;

        let right_section = div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(10.))
            .child(transport(&self.playback, &self.queue, false, cx))
            .child(
                Button::new("player-lyrics")
                    .ghost()
                    .small()
                    .icon("icons/mic-2.svg")
                    .tooltip_above("lyrics-title")
                    .selected(lyrics_active)
                    .tint(match lyrics_active {
                        true => theme.foreground,
                        false => theme.muted_foreground,
                    })
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(ToggleLyrics), cx);
                    }),
            )
            .child(
                Button::new("player-queue")
                    .ghost()
                    .small()
                    .icon("icons/list-music.svg")
                    .tooltip_above("queue-title")
                    .selected(queue_active)
                    .tint(match queue_active {
                        true => theme.foreground,
                        false => theme.muted_foreground,
                    })
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(ToggleQueue), cx);
                    }),
            )
            .child(
                Button::new("player-more")
                    .ghost()
                    .small()
                    .icon("icons/more-horizontal.svg")
                    .tooltip_above("menu-more")
                    .tint(muted)
                    .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                        this.open_playing_menu(event.position(), cx);
                    })),
            )
            .child(self.sound(px(VOLUME_TIGHT), cx))
            .child(self.fullscreen_button());

        let ambient = crate::shared::ambient::shown(cx);

        let base = div()
            .flex()
            .w_full()
            .h(height)
            .flex_none()
            .px(px(20.))
            .rounded(px(16.))
            .border_1()
            .border_color(theme.border)
            .when_else(
                ambient || ui::blurring(cx),
                |this| ui::glass(this, cx),
                |this| this.bg(theme.secondary),
            )
            .shadow_lg()
            .on_mouse_move(cx.listener(Self::hover));

        let context_menu = self.context_menu.clone().map(|(track, position)| {
            Popup::new(position, self.track_menu.for_track(&track, cx)).on_close(cx.listener(
                |this, _, _, cx| {
                    this.context_menu = None;
                    cx.notify();
                },
            ))
        });

        let content = match stacked {
            true => base
                .flex_col()
                .justify_center()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .w_full()
                        .child(left_section)
                        .child(transport(&self.playback, &self.queue, false, cx)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .w_full()
                        .child(self.sound(px(VOLUME_TIGHT), cx))
                        .child(self.fullscreen_button()),
                ),
            false => base
                .items_center()
                .justify_between()
                .child(left_section)
                .child(right_section),
        };

        content.when_some(context_menu, |this, menu| this.child(menu))
    }
}
