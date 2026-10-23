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
use std::time::Instant;

use gpui::prelude::*;
use gpui::{App, Bounds, Context, Hsla, Render, Rgba, Window, canvas, div, point, px, rgba};
use state::Sonora;
use ui::{ActiveTheme as _, Theme};

const BLOBS: usize = 5;
const BLUR_FULL: f32 = 14.;
const DOWNSCALE: f32 = 4.;
const SHADE: f32 = 0.35;
const DISCS: usize = 3;
const DISC_FAINT: f32 = 0.04;
const DISC_STRONG: f32 = 0.36;
const WASH: f32 = 0.8;

struct BlobSpec {
    color_idx: usize,
    secondary_idx: usize,
    base_x: f32,
    base_y: f32,
    size: f32,
    period_x: f32,
    period_y: f32,
    phase_x: f32,
    phase_y: f32,
    amp_x: f32,
    amp_y: f32,
    pulse_period: f32,
    pulse_amp: f32,
}

const SPECS: [BlobSpec; 5] = [
    BlobSpec {
        color_idx: 0,
        secondary_idx: 1,
        base_x: 0.35,
        base_y: 0.38,
        size: 1.35,
        period_x: 26.,
        period_y: 34.,
        phase_x: 0.0,
        phase_y: 1.2,
        amp_x: 0.16,
        amp_y: 0.13,
        pulse_period: 18.,
        pulse_amp: 0.08,
    },
    BlobSpec {
        color_idx: 2,
        secondary_idx: 0,
        base_x: 0.65,
        base_y: 0.62,
        size: 1.30,
        period_x: 32.,
        period_y: 24.,
        phase_x: 2.1,
        phase_y: 0.5,
        amp_x: 0.17,
        amp_y: 0.14,
        pulse_period: 22.,
        pulse_amp: 0.09,
    },
    BlobSpec {
        color_idx: 1,
        secondary_idx: 2,
        base_x: 0.22,
        base_y: 0.26,
        size: 1.10,
        period_x: 21.,
        period_y: 28.,
        phase_x: 3.5,
        phase_y: 4.1,
        amp_x: 0.20,
        amp_y: 0.16,
        pulse_period: 15.,
        pulse_amp: 0.11,
    },
    BlobSpec {
        color_idx: 3,
        secondary_idx: 4,
        base_x: 0.78,
        base_y: 0.74,
        size: 1.15,
        period_x: 35.,
        period_y: 29.,
        phase_x: 1.8,
        phase_y: 3.2,
        amp_x: 0.19,
        amp_y: 0.15,
        pulse_period: 24.,
        pulse_amp: 0.09,
    },
    BlobSpec {
        color_idx: 4,
        secondary_idx: 0,
        base_x: 0.72,
        base_y: 0.28,
        size: 1.05,
        period_x: 24.,
        period_y: 30.,
        phase_x: 4.7,
        phase_y: 2.3,
        amp_x: 0.22,
        amp_y: 0.17,
        pulse_period: 19.,
        pulse_amp: 0.10,
    },
];

pub(crate) struct Ambient {
    started: Instant,
    stepped: Instant,
    bounds: Rc<Cell<Bounds<gpui::Pixels>>>,
    painted: Option<[Hsla; BLOBS]>,
}

impl Ambient {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let settings = Sonora::global(cx).settings.clone();
        cx.observe(&settings, |_, _, cx| cx.notify()).detach();

        let now = Instant::now();
        Self {
            started: now,
            stepped: now,
            bounds: Rc::new(Cell::new(Bounds::default())),
            painted: None,
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
                s: accent.s.clamp(0.04, 0.20),
                l: (0.22 + (accent.l - 0.5) * 0.12).clamp(0.16, 0.28),
                a: 1.,
            },
            false => Hsla {
                h: accent.h,
                s: (accent.s * 0.85 + 0.04).clamp(0.40, 0.70),
                l: (0.28 + (accent.l - 0.45) * 0.25).clamp(0.20, 0.36),
                a: 1.,
            },
        };
        let second = match theme.tint_secondary {
            Some(second) => match is_neon_mono {
                true => Hsla {
                    h: second.h,
                    s: second.s.clamp(0.04, 0.20),
                    l: (0.18 + (second.l - 0.5) * 0.10).clamp(0.14, 0.24),
                    a: 1.,
                },
                false => Hsla {
                    h: second.h,
                    s: (second.s * 0.85).clamp(0.35, 0.65),
                    l: (0.25 + (second.l - 0.45) * 0.25).clamp(0.18, 0.34),
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
                    l: (base.l + 0.06).clamp(0.20, 0.34),
                    s: (base.s + 0.04).clamp(0.06, 0.24),
                    ..base
                },
                second,
                Hsla {
                    l: if theme.pitch_black { 0.02 } else { 0.12 },
                    s: 0.04,
                    ..base
                },
                Hsla {
                    l: if theme.pitch_black { 0.01 } else { 0.08 },
                    s: 0.04,
                    ..base
                },
            ],
            false => [
                base,
                Hsla {
                    l: (base.l + 0.08).clamp(0.24, 0.40),
                    s: (base.s - 0.06).clamp(0.35, 0.65),
                    ..base
                },
                second,
                Hsla {
                    l: (base.l - 0.08).clamp(0.14, 0.26),
                    s: (base.s + 0.04).clamp(0.40, 0.70),
                    ..base
                },
                Hsla {
                    h: (base.h + 0.97).rem_euclid(1.),
                    l: (base.l - 0.10).clamp(0.12, 0.22),
                    s: (base.s - 0.02).clamp(0.35, 0.70),
                    ..base
                },
            ],
        }
    }
}

impl Render for Ambient {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let animates = window.is_window_active()
            && ui::motion::animates(cx)
            && Sonora::global(cx).settings.read(cx).ambient_motion();
        if animates {
            window.request_animation_frame();
        }
        let elapsed = match animates {
            true => self.started.elapsed().as_secs_f32(),
            false => 0.,
        };
        let colors = self.wash(Self::colors(&theme), animates);
        let clarity = match theme.transparent {
            true => theme.background.a,
            false => 1.,
        };
        let bounds = self.bounds.get();
        let wide = bounds.size.width.as_f32().max(1.) / DOWNSCALE;
        let high = bounds.size.height.as_f32().max(1.) / DOWNSCALE;
        let dim = wide.max(high);

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
                            .children(SPECS.iter().map(|spec| {
                                let spin_x = TAU * elapsed / spec.period_x;
                                let spin_y = TAU * elapsed / spec.period_y;
                                let x = match animates {
                                    true => spec.base_x + spec.amp_x * (spin_x + spec.phase_x).sin(),
                                    false => spec.base_x,
                                };
                                let y = match animates {
                                    true => spec.base_y + spec.amp_y * (spin_y + spec.phase_y).cos(),
                                    false => spec.base_y,
                                };
                                let pulse_spin = TAU * elapsed / spec.pulse_period;
                                let grown = match animates {
                                    true => dim * spec.size * (1. + spec.pulse_amp * (pulse_spin + spec.phase_x).sin()),
                                    false => dim * spec.size,
                                };
                                let color_spin = TAU * elapsed / (spec.period_x * 0.75);
                                let color_t = match animates {
                                    true => 0.5 + 0.35 * (color_spin + spec.phase_y).sin(),
                                    false => 0.,
                                };
                                let color = blend(colors[spec.color_idx], colors[spec.secondary_idx], color_t);
                                div()
                                    .absolute()
                                    .left(px(x * wide - grown / 2.))
                                    .top(px(y * high - grown / 2.))
                                    .size(px(grown))
                                    .children((0..DISCS).map(move |step| {
                                        let progress = step as f32 / (DISCS as f32 - 1.);
                                        let radius_factor = 1.0 - (step as f32 / DISCS as f32) * 0.75;
                                        let opacity = DISC_FAINT + progress * progress * (DISC_STRONG - DISC_FAINT);
                                        let stepped = grown * radius_factor;
                                        div()
                                            .absolute()
                                            .left(px((grown - stepped) / 2.))
                                            .top(px((grown - stepped) / 2.))
                                            .size(px(stepped))
                                            .rounded_full()
                                            .bg(shaded(color).opacity(opacity))
                                    }))
                            })),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .when_some(radius, |this, radius| this.rounded(radius))
                    .bg(rgba(0x00000038)),
            )
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
