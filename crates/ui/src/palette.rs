// SPDX-License-Identifier: GPL-3.0-or-later
// Portions Copyright (C) Sonora Contributors
// Modifications Copyright (C) 2026 Poly Project Contributors

use std::f32::consts::TAU;

use gpui::{App, Hsla, RenderImage, Rgba, SharedString, Task};

use crate::artwork::sample_palette;

const BINS: usize = 24;
const SAMPLES: usize = 6000;
const MIN_ALPHA: u8 = 128;
const MIN_SATURATION: f32 = 0.14;
/// Dark covers still name their hue: the ambient background and the selection tint both
/// follow near-black art, so the floor sits well below picture grey.
const MIN_LIGHTNESS: f32 = 0.10;
const MAX_LIGHTNESS: f32 = 0.94;
const MIN_SHARE: f32 = 0.01;
/// A peak also needs this much absolute weight, so a lone splash on an
/// otherwise empty image never names a tint no matter the shares.
const MIN_WEIGHT: f32 = 25.;
/// A runner-up hue only counts when it owns this share of the sampled pixels
/// and stands apart from the lead.
const SECONDARY_SHARE: f32 = 0.02;
const SECONDARY_WEIGHT: f32 = 10.;
const SECONDARY_GAP: usize = 3;
const SECONDARY_HUE: f32 = 0.05;

/// The palette of a frame that is already decoded, for artwork the cache has in
/// hand. Sampling walks at most `SAMPLES` pixels, so this costs nothing beside
/// the decode that produced the frame.
pub(crate) fn of_image(image: &RenderImage) -> CoverPalette {
    image.as_bytes(0).map(sampled).unwrap_or_default()
}

/// The dominant hue of the artwork at `url`, or none for near-greyscale art.
pub fn tint(url: impl Into<SharedString>, cx: &mut App) -> Task<Option<Hsla>> {
    let palette = palette(url, cx);

    cx.spawn(async move |_| palette.await.primary)
}

/// What one artwork extraction names: the hue that leads and, when the art
/// carries a real second family, the runner-up. Sampled once per cover and
/// shared by the theme tint and the ambient background, so the two never disagree.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct CoverPalette {
    pub primary: Option<Hsla>,
    pub secondary: Option<Hsla>,
    pub lightness: f32,
    pub pitch_black: bool,
    pub neon_accent: Option<Hsla>,
}

pub fn palette(url: impl Into<SharedString>, cx: &mut App) -> Task<CoverPalette> {
    sample_palette(url.into(), cx)
}

#[derive(Clone, Copy, Default)]
struct Bin {
    weight: f32,
    x: f32,
    y: f32,
    saturation: f32,
    lightness: f32,
}

impl Bin {
    fn add(&mut self, color: Hsla, weight: f32) {
        let angle = color.h * TAU;
        self.weight += weight;
        self.x += angle.cos() * weight;
        self.y += angle.sin() * weight;
        self.saturation += color.s * weight;
        self.lightness += color.l * weight;
    }

    fn merge(&mut self, other: &Self) {
        self.weight += other.weight;
        self.x += other.x;
        self.y += other.y;
        self.saturation += other.saturation;
        self.lightness += other.lightness;
    }

    fn colour(&self) -> Option<Hsla> {
        (self.weight > 0.).then(|| Hsla {
            h: self.y.atan2(self.x).rem_euclid(TAU) / TAU,
            s: (self.saturation / self.weight).clamp(0., 1.),
            l: (self.lightness / self.weight).clamp(0., 1.),
            a: 1.,
        })
    }
}

struct PaletteSampler<'a> {
    pixels: &'a [u8],
    edge: usize,
    bins: [Bin; BINS],
    sampled: f32,
    lightness: f32,
    bright_samples: f32,
    bright_l: f32,
    bright_s: f32,
    bright_h: f32,
}

impl<'a> PaletteSampler<'a> {
    fn new(pixels: &'a [u8]) -> Self {
        let total_pixels = pixels.len() / 4;
        let edge = total_pixels.isqrt().max(1);
        Self {
            pixels,
            edge,
            bins: [Bin::default(); BINS],
            sampled: 0.,
            lightness: 0.,
            bright_samples: 0.,
            bright_l: 0.,
            bright_s: 0.,
            bright_h: 0.,
        }
    }

    fn sample_pixel(&self, x: usize, y: usize) -> Option<Hsla> {
        let idx = (y * self.edge + x) * 4;
        if idx + 3 < self.pixels.len() {
            let blue = self.pixels[idx];
            let green = self.pixels[idx + 1];
            let red = self.pixels[idx + 2];
            let alpha = self.pixels[idx + 3];
            if alpha >= MIN_ALPHA {
                return Some(Hsla::from(Rgba {
                    r: red as f32 / 255.,
                    g: green as f32 / 255.,
                    b: blue as f32 / 255.,
                    a: 1.,
                }));
            }
        }
        None
    }

    fn corner_lightness(&self) -> f32 {
        let mut total = 0.;
        let mut count = 0.;
        let steps = [0, self.edge / 4, self.edge / 2, (3 * self.edge) / 4, self.edge.saturating_sub(1)];
        for &x in &steps {
            for &y in &[0, self.edge.saturating_sub(1)] {
                if let Some(c) = self.sample_pixel(x, y) {
                    total += c.l;
                    count += 1.;
                }
            }
        }
        for &y in &steps[1..4] {
            for &x in &[0, self.edge.saturating_sub(1)] {
                if let Some(c) = self.sample_pixel(x, y) {
                    total += c.l;
                    count += 1.;
                }
            }
        }
        if count > 0. { total / count } else { 0. }
    }

    fn walk(&mut self) {
        let stride = (self.pixels.len() / 4 / SAMPLES).max(1);
        for &[blue, green, red, alpha] in self.pixels.as_chunks::<4>().0.iter().step_by(stride) {
            if alpha < MIN_ALPHA {
                continue;
            }
            self.sampled += 1.;
            let colour = Hsla::from(Rgba {
                r: red as f32 / 255.,
                g: green as f32 / 255.,
                b: blue as f32 / 255.,
                a: 1.,
            });
            self.lightness += colour.l;
            if colour.l > 0.45 {
                self.bright_samples += 1.;
                self.bright_l += colour.l;
                self.bright_s += colour.s;
                self.bright_h += colour.h;
            }
            if colour.s < MIN_SATURATION || colour.l < MIN_LIGHTNESS || colour.l > MAX_LIGHTNESS {
                continue;
            }
            let index = ((colour.h * BINS as f32) as usize).min(BINS - 1);
            self.bins[index].add(colour, colour.s * (0.3 + 0.7 * colour.l));
        }
    }

    fn finish(self) -> CoverPalette {
        let corner_l = self.corner_lightness();
        let pitch_black = corner_l < 0.08;
        let mean = match self.sampled > 0. {
            true => self.lightness / self.sampled,
            false => 0.,
        };
        let peak = (0..BINS).max_by(|&a, &b| score(&self.bins, a).total_cmp(&score(&self.bins, b)));
        let primary = peak.filter(|&peak| score(&self.bins, peak) >= (self.sampled * MIN_SHARE).max(MIN_WEIGHT));
        let Some(peak) = primary else {
            let neon_accent = if self.bright_samples >= 6. && pitch_black {
                let avg_l = (self.bright_l / self.bright_samples).clamp(0.70, 0.95);
                let avg_s = (self.bright_s / self.bright_samples).min(0.20);
                let avg_h = if self.bright_s / self.bright_samples > 0.05 {
                    self.bright_h / self.bright_samples
                } else {
                    0.55
                };
                Some(Hsla {
                    h: avg_h,
                    s: avg_s.max(0.12),
                    l: avg_l,
                    a: 1.,
                })
            } else {
                None
            };
            return CoverPalette {
                primary: None,
                secondary: None,
                lightness: mean,
                pitch_black,
                neon_accent,
            };
        };
        let mut cluster = self.bins[peak];
        cluster.merge(&self.bins[(peak + BINS - 1) % BINS]);
        cluster.merge(&self.bins[(peak + 1) % BINS]);
        let primary = cluster.colour();
        let runner = (0..BINS)
            .filter(|&index| {
                let gap = index.abs_diff(peak).min(BINS - index.abs_diff(peak));
                gap >= SECONDARY_GAP
                    && score(&self.bins, index) >= (self.sampled * SECONDARY_SHARE).max(SECONDARY_WEIGHT)
            })
            .max_by(|&a, &b| score(&self.bins, a).total_cmp(&score(&self.bins, b)));
        let secondary = runner
            .and_then(|index| {
                let mut cluster = self.bins[index];
                cluster.merge(&self.bins[(index + BINS - 1) % BINS]);
                cluster.merge(&self.bins[(index + 1) % BINS]);
                cluster.colour()
            })
            .filter(|colour| {
                primary.is_some_and(|lead| {
                    (colour.h - lead.h)
                        .abs()
                        .min(1. - (colour.h - lead.h).abs())
                        >= SECONDARY_HUE
                })
            });
        CoverPalette {
            primary,
            secondary,
            lightness: mean,
            pitch_black,
            neon_accent: None,
        }
    }
}

fn sampled(pixels: &[u8]) -> CoverPalette {
    let mut sampler = PaletteSampler::new(pixels);
    sampler.walk();
    sampler.finish()
}

fn score(bins: &[Bin; BINS], index: usize) -> f32 {
    bins[index].weight
        + (bins[(index + BINS - 1) % BINS].weight + bins[(index + 1) % BINS].weight) * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(colours: &[([u8; 3], usize)]) -> Vec<u8> {
        colours
            .iter()
            .flat_map(|&([red, green, blue], count)| {
                std::iter::repeat_n([blue, green, red, 255], count).flatten()
            })
            .collect()
    }

    #[test]
    fn finds_the_dominant_hue() {
        let pixels = image(&[([204, 34, 34], 900), ([32, 32, 32], 100)]);
        let colour = sampled(&pixels).primary.expect("a red image is colourful");

        assert!(colour.h < 0.02 || colour.h > 0.98, "hue was {}", colour.h);
        assert!(colour.s > 0.5, "saturation was {}", colour.s);
    }

    #[test]
    fn averages_across_the_bin_boundary() {
        let pixels = image(&[([255, 0, 60], 500), ([255, 60, 0], 500)]);
        let colour = sampled(&pixels).primary.expect("a red image is colourful");

        assert!(colour.h < 0.02 || colour.h > 0.98, "hue was {}", colour.h);
    }

    #[test]
    fn ignores_greyscale_artwork() {
        let pixels = image(&[([18, 18, 18], 500), ([200, 200, 200], 500)]);

        assert!(sampled(&pixels).primary.is_none());
    }

    #[test]
    fn ignores_a_small_splash_of_colour() {
        let pixels = image(&[([120, 120, 120], 990), ([0, 180, 255], 10)]);

        assert!(sampled(&pixels).primary.is_none());
    }

    #[test]
    fn fragmented_hue_on_black_still_names_it() {
        let pixels = image(&[
            ([170, 110, 220], 200),
            ([140, 70, 200], 200),
            ([120, 20, 20], 100),
            ([8, 8, 8], 500),
        ]);
        let colour = sampled(&pixels)
            .primary
            .expect("smeared purple names purple");

        assert!(colour.h > 0.68 && colour.h < 0.9, "hue was {}", colour.h);
    }

    #[test]
    fn light_accent_wins_on_a_dark_cover() {
        let pixels = image(&[
            ([170, 110, 220], 350),
            ([120, 20, 20], 350),
            ([8, 8, 8], 300),
        ]);
        let colour = sampled(&pixels)
            .primary
            .expect("a dark cover names its light accent");

        assert!(colour.h > 0.7 && colour.h < 0.88, "hue was {}", colour.h);
    }

    #[test]
    fn names_a_runner_up() {
        let pixels = image(&[
            ([220, 40, 40], 600),
            ([40, 120, 220], 300),
            ([120, 120, 120], 100),
        ]);
        let palette = sampled(&pixels);

        assert!(palette.primary.is_some());
        let second = palette.secondary.expect("two families name two hues");
        let gap = (palette.primary.unwrap().h - second.h)
            .abs()
            .min(1. - (palette.primary.unwrap().h - second.h).abs());
        assert!(gap > 0.1, "hues were {gap}");
    }

    #[test]
    fn one_family_names_no_runner_up() {
        let pixels = image(&[([90, 30, 130], 800), ([10, 10, 12], 200)]);
        let palette = sampled(&pixels);

        assert!(palette.primary.is_some());
        assert!(palette.secondary.is_none());
    }
}
