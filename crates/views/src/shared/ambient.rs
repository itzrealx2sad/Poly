// SPDX-License-Identifier: GPL-3.0-or-later
// Portions Copyright (C) Sonora Contributors
// Modifications Copyright (C) 2026 Poly Project Contributors

//! The ambient background: a flowing colour field behind fullscreen, five soft
//! blobs under a heavy blur, darkened so lyrics stay readable. `Root` owns the
//! one entity, paints it under everything, title bar included, and paints no
//! background of its own beneath it: the field stands in for the page and
//! carries the window's own opacity, so a see-through window stays see-through
//! in fullscreen.
//!
//! `ambient` decides whether it is painted at all, and with it whether the
//! fullscreen controls frost what they float over. `ambient_motion` decides
//! whether the field drifts.
//!
//! The colours are the theme's `tint` and `tint_secondary`, which fullscreen
//! samples off the cover whatever the adaptive theme setting says, resolved
//! through `Theme::accent` rather than read off `Theme::selection`, which lags
//! a track change by the length of the theme's own fade. Art with no colour
//! leaves no tint and the stage stays neutral. `painted` eases the colours on
//! screen toward those, so a cover change washes in rather than cuts.

use std::cell::Cell;
use std::f32::consts::TAU;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{App, Bounds, Context, Hsla, Render, Rgba, Window, canvas, div, point, px};
use state::Sonora;
use ui::{ActiveTheme as _, Theme};

/// How many blobs make up the field.
const BLOBS: usize = 5;
/// The widest blur, in device pixels, the renderer still runs at full resolution. It picks
/// the buffer from the radius: four or less keeps every pixel, eight halves the frame, and
/// anything wider drops to a quarter. The field is already drawn at `DOWNSCALE`, so a wider
/// blur would shrink it a second time and the upscale would crawl over every seam. The discs
/// carry the falloff instead.
const BLUR_FULL: f32 = 8.;
/// How many times smaller the field is drawn than it is shown. The layer is
/// laid out at this fraction of the window and the compositor scales it back
/// up with a bilinear sample, so the discs and the blur cost a sixteenth of
/// the pixels. Nothing in the field is sharper than the blur, so the upscale
/// shows nothing.
const DOWNSCALE: f32 = 4.;
/// How much of itself the field keeps under the dark overlay that holds lyrics readable over
/// it. The overlay is folded into the colours rather than painted as a sheet of its own: over
/// an opaque stack multiplying every layer by what it lets through is the same picture, and a
/// black sheet this strong was most of what a see-through window's alpha went on.
const SHADE: f32 = 0.20;

const GRID_SIZE: usize = 6;
const GRID_MAP: [[usize; GRID_SIZE]; GRID_SIZE] = [
    [0, 1, 3, 2, 0, 4],
    [2, 4, 0, 1, 3, 1],
    [1, 0, 2, 4, 1, 0],
    [3, 2, 1, 0, 4, 2],
    [0, 4, 3, 2, 0, 1],
    [4, 1, 0, 3, 2, 4],
];

const WASH: f32 = 0.8;

pub(crate) struct Ambient {
    started: Instant,
    stepped: Instant,
    bounds: Rc<Cell<Bounds<gpui::Pixels>>>,
    painted: Option<[Hsla; BLOBS]>,
    animating: bool,
}

impl Ambient {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let settings = Sonora::global(cx).settings.clone();
        // Turning the drift back on has no frame of its own to land in: with
        // motion off nothing asks for one.
        cx.observe(&settings, |_, _, cx| cx.notify()).detach();

        let now = Instant::now();
        Self {
            started: now,
            stepped: now,
            bounds: Rc::new(Cell::new(Bounds::default())),
            painted: None,
            animating: false,
        }
    }

    /// Eases the painted colours one frame toward `target` and returns them.
    /// The step is the real time since the last frame, so a gap long enough to
    /// mean the field was off screen lands on the target outright and entering
    /// fullscreen never washes in from whatever played before. With motion off
    /// there are no frames to ease over, so the target is taken as it is.
    fn wash(&mut self, target: [Hsla; BLOBS], animates: bool) -> [Hsla; BLOBS] {
        let now = Instant::now();
        let step = now.duration_since(self.stepped).as_secs_f32();
        self.stepped = now;

        let painted = match self.painted {
            Some(painted) if animates => {
                let delta = 1. - (-step / WASH).exp();
                std::array::from_fn(|index| blend(painted[index], target[index], delta))
            }
            _ => target,
        };
        self.painted = Some(painted);
        painted
    }

    /// Resolves the five blob colours from the theme: the cover's leading hue
    /// carried from a light highlight down to a dark shadow, plus its runner-up
    /// where the art names one. Art with no colour leaves no tints behind, and
    /// then the stage is quiet neutrals.
    fn colors(theme: &Theme) -> [Hsla; BLOBS] {
        let neutral = |light: f32| Hsla {
            h: 0.06,
            s: 0.12,
            l: light,
            a: 1.,
        };
        let floor = match theme.pitch_black {
            true => 0.02,
            false => (0.13 + theme.background.l * 0.8).clamp(0.13, 0.30),
        };

        let Some(tint) = theme.tint else {
            return match theme.pitch_black {
                true => [
                    neutral(0.02),
                    neutral(0.04),
                    neutral(0.05),
                    neutral(0.02),
                    neutral(0.03),
                ],
                false => [
                    neutral(floor),
                    neutral(floor + 0.025),
                    neutral(floor + 0.05),
                    neutral(floor + 0.015),
                    neutral(floor + 0.04),
                ],
            };
        };
        let accent = theme.accent(tint);
        let is_neon_mono = tint.s < 0.28;
        let base = match is_neon_mono {
            true => Hsla {
                h: accent.h,
                s: accent.s.clamp(0.06, 0.32),
                l: (0.78 + (accent.l - 0.5) * 0.3).clamp(0.68, 0.94),
                a: 1.,
            },
            false => Hsla {
                h: accent.h,
                s: (accent.s * 0.9 + 0.04).clamp(0.45, 0.75),
                l: (0.40 + (accent.l - 0.45) * 0.5).clamp(0.28, 0.52),
                a: 1.,
            },
        };
        let second = match theme.tint_secondary {
            Some(second) => match is_neon_mono {
                true => Hsla {
                    h: second.h,
                    s: second.s.clamp(0.08, 0.36),
                    l: (0.72 + (second.l - 0.5) * 0.3).clamp(0.58, 0.88),
                    a: 1.,
                },
                false => Hsla {
                    h: second.h,
                    s: (second.s * 0.9).clamp(0.4, 0.72),
                    l: (0.38 + (second.l - 0.45) * 0.5).clamp(0.28, 0.5),
                    a: 1.,
                },
            },
            None => match theme.pitch_black {
                true => Hsla {
                    h: accent.h,
                    s: 0.08,
                    l: 0.03,
                    a: 1.,
                },
                false => neutral(floor + 0.03),
            },
        };
        match is_neon_mono {
            true => [
                base,
                Hsla {
                    l: (base.l + 0.06).clamp(0.72, 0.96),
                    s: (base.s + 0.04).clamp(0.08, 0.38),
                    ..base
                },
                second,
                Hsla {
                    l: if theme.pitch_black { 0.02 } else { 0.16 },
                    s: 0.04,
                    ..base
                },
                Hsla {
                    l: if theme.pitch_black { 0.01 } else { 0.12 },
                    s: 0.04,
                    ..base
                },
            ],
            false => [
                base,
                Hsla {
                    l: (base.l + 0.14).clamp(0.3, 0.62),
                    s: (base.s - 0.08).clamp(0.4, 0.75),
                    ..base
                },
                second,
                Hsla {
                    l: (base.l - 0.13).clamp(0.18, 0.45),
                    s: (base.s + 0.04).clamp(0.45, 0.8),
                    ..base
                },
                Hsla {
                    h: (base.h + 0.97).rem_euclid(1.),
                    l: (base.l - 0.16).clamp(0.16, 0.4),
                    s: (base.s - 0.02).clamp(0.4, 0.78),
                    ..base
                },
            ],
        }
    }
}

impl Render for Ambient {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        // Without motion the field is a still gradient: no frame requests, so
        let animates = window.is_window_active()
            && ui::motion::animates(cx)
            && Sonora::global(cx).settings.read(cx).ambient_motion();
        if animates && !self.animating {
            self.animating = true;
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_millis(30)).await;
                    let running = this.update(cx, |this, cx| {
                        let active = Sonora::global(cx).settings.read(cx).ambient_motion()
                            && ui::motion::animates(cx);
                        if active && this.animating {
                            cx.notify();
                            true
                        } else {
                            this.animating = false;
                            false
                        }
                    });
                    if !running.unwrap_or(false) {
                        break;
                    }
                }
            }).detach();
        } else if !animates && self.animating {
            self.animating = false;
        }
        let elapsed = match animates {
            true => self.started.elapsed().as_secs_f32(),
            false => 0.,
        };
        let colors = self.wash(Self::colors(&theme), animates);
        // The field stands in for the page rather than sitting on it, so it carries the
        // window's own opacity and `Root` paints no background beneath it. One opacity thins
        // every layer at once; a quad still adds to what it covers, which is why the blobs
        // read a shade denser than the gaps between them.
        let clarity = match theme.transparent {
            true => theme.background.a,
            false => 1.,
        };
        // Geometry resolves against the layer's own pixel bounds, measured a
        // frame ago by the canvas, so the discs stay circular at any window
        // aspect ratio. The field is laid out at a fraction of those bounds
        // and scaled back up from the top left corner.
        let bounds = self.bounds.get();
        let wide = bounds.size.width.as_f32().max(1.) / DOWNSCALE;
        let high = bounds.size.height.as_f32().max(1.) / DOWNSCALE;
        // The field reaches into the corners the chrome rounds, and GPUI clips only to a
        // rectangle, so the sheet takes the window's radius itself.
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        let radius = crate::chrome::window_radius(Sonora::global(cx).settings.read(cx), cx, window);
        #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
        let radius: Option<gpui::Pixels> = None;

        div()
            .id("ambient")
            .absolute()
            .inset_0()
            .overflow_hidden()
            .when_some(radius, |this, radius| this.rounded(radius))
            .opacity(clarity)
            .child(
                canvas(
                    {
                        let bounds = self.bounds.clone();
                        move |got, _, _| bounds.set(got)
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .when_some(radius, |this, radius| this.rounded(radius))
                    .bg(match theme.pitch_black {
                        true => Hsla { h: 0., s: 0., l: 0., a: 1. },
                        false => shaded(Hsla {
                            a: 1.,
                            ..theme.background
                        }),
                    }),
            )
            // The blur spreads the blobs past any shape they could be cut to, so a rounded window
            // fades them out toward every edge instead. The fade sits on an unblurred layer the
            // size of the window, since a blurred one measures it from past its own blur reach.
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .when_some(radius, |this, radius| {
                        this.fade_edges(radius * 2., radius * 2.)
                            .fade_sides(radius * 2., radius * 2.)
                    })
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top_0()
                            .w(px(wide))
                            .h(px(high))
                            .layer_scale(DOWNSCALE)
                            .layer_scale_origin(point(0., 0.))
                            .blur(px(BLUR_FULL / window.scale_factor()))
                            .children((0..GRID_SIZE).flat_map(|row| {
                                let colors = colors;
                                (0..GRID_SIZE).map(move |col| {
                                    let cell_w = wide / GRID_SIZE as f32;
                                    let cell_h = high / GRID_SIZE as f32;
                                    let primary_idx = GRID_MAP[row][col];
                                    let secondary_idx = (primary_idx + 1 + (row + col) % 3) % BLOBS;
                                    let color = match animates {
                                        true => {
                                            let period = 18. + ((row * 7 + col * 11) % 15) as f32;
                                            let phase = row as f32 * 1.3 + col as f32 * 1.7;
                                            let spin = TAU * elapsed / period;
                                            let t = 0.5 + 0.5 * (spin + phase).sin();
                                            blend(colors[primary_idx], colors[secondary_idx], t * 0.45)
                                        }
                                        false => colors[primary_idx],
                                    };
                                    div()
                                        .absolute()
                                        .left(px(col as f32 * cell_w))
                                        .top(px(row as f32 * cell_h))
                                        .w(px(cell_w + 1.5))
                                        .h(px(cell_h + 1.5))
                                        .bg(shaded(color))
                                })
                            })),
                    ),
            )
            // Every buffer the field passes through holds eight bits a channel, and a gradient
            // this wide and this dark steps through only a few dozen of them, so its steps read
            // as bands that slide with the blobs. The dither scatters each step over
            // neighbouring pixels instead.
            .child(ui::grain(window))
    }
}

/// Whether the ambient background is on. Fullscreen reads it for the frosted
/// glass on its controls as well, which only has the field to blur.
pub(crate) fn shown(cx: &App) -> bool {
    Sonora::global(cx).settings.read(cx).ambient()
}

/// A colour as the dark overlay leaves it. Black at `SHADE` over an opaque colour is that
/// colour's channels cut by what the sheet lets through, so cutting them here is the same
/// picture with one layer fewer to spend the window's alpha on.
fn shaded(color: Hsla) -> Hsla {
    let rgba = Rgba::from(color);
    let kept = 1. - SHADE;

    Hsla::from(Rgba {
        r: rgba.r * kept,
        g: rgba.g * kept,
        b: rgba.b * kept,
        a: rgba.a,
    })
}

/// Straight-line blend between two colours through RGB, so a wash between two
/// unrelated hues passes through grey rather than through every hue between
/// them.
fn blend(from: Hsla, to: Hsla, delta: f32) -> Hsla {
    let (from, to) = (Rgba::from(from), Rgba::from(to));
    let channel = |from: f32, to: f32| from + (to - from) * delta;

    Hsla::from(Rgba {
        r: channel(from.r, to.r),
        g: channel(from.g, to.g),
        b: channel(from.b, to.b),
        a: channel(from.a, to.a),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tinted(hue: f32) -> Theme {
        let mut theme = Theme::dark();
        theme.tint = Some(Hsla {
            h: hue,
            s: 0.6,
            l: 0.4,
            a: 1.,
        });
        theme.selection = Hsla {
            h: hue,
            s: 0.7,
            l: 0.44,
            a: 1.,
        };
        theme.tint_secondary = Some(Hsla {
            h: (hue + 0.5).rem_euclid(1.),
            s: 0.6,
            l: 0.4,
            a: 1.,
        });
        theme
    }

    #[test]
    fn accent_theme_yields_its_family() {
        let colors = Ambient::colors(&tinted(0.78));

        for (index, color) in colors.iter().enumerate() {
            // The runner-up sits in slot two; everything else is family.
            if index == 2 {
                let gap = (color.h - 0.28).abs().min(1. - (color.h - 0.28).abs());
                assert!(gap < 0.06, "second hue was {}", color.h);
                continue;
            }
            let gap = (color.h - 0.78).abs().min(1. - (color.h - 0.78).abs());
            assert!(gap < 0.06, "hue was {}", color.h);
        }
        assert!(colors[1].l > colors[0].l, "highlight lifts");
        assert!(colors[3].l < colors[0].l, "shadow sinks");
    }

    #[test]
    fn untinted_theme_yields_neutrals() {
        let colors = Ambient::colors(&Theme::dark());

        assert!(colors.iter().all(|color| color.s < 0.2));
    }
}
