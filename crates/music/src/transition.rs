// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Poly Project Contributors

use std::f32::consts::{FRAC_PI_2, PI, SQRT_2};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

const LOW_CROSSOVER_HZ: f32 = 130.0;
const HIGH_CROSSOVER_HZ: f32 = 3000.0;
const SILENCE_THRESHOLD: f32 = 0.0022;
const MAX_SILENCE_TRIM_MS: u64 = 1600;
const ONSET_CUSHION_MS: u64 = 12;
const EARLY_FADE_RMS_THRESHOLD: f32 = 0.022;
const QUIET_TAIL_RMS_THRESHOLD: f32 = 0.012;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitionConfig {
    None,
    CrossfadeManual(u32),
    CrossfadeAuto,
    AutoMix,
}

impl TransitionConfig {
    pub fn is_active(self) -> bool {
        !matches!(self, Self::None)
    }

    pub fn is_enabled(self) -> bool {
        self.is_active()
    }

    pub fn is_automix(self) -> bool {
        matches!(self, Self::AutoMix)
    }

    pub fn duration_for(self, track_duration: Option<Duration>) -> Duration {
        let secs = track_duration.map_or(180.0, |d| d.as_secs_f32());
        if secs > 0.0 && secs < 6.0 {
            return Duration::ZERO;
        }
        let target = match self {
            Self::None => 0.0,
            Self::CrossfadeManual(manual) => {
                let requested = manual.clamp(1, 12) as f32;
                if secs > 0.0 {
                    requested.min((secs * 0.38).max(1.0))
                } else {
                    requested
                }
            }
            Self::CrossfadeAuto => Self::auto_crossfade_secs(secs),
            Self::AutoMix => Self::automix_secs(secs),
        };
        if target <= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f32(target)
        }
    }

    pub fn preload_window(self, track_duration: Option<Duration>) -> Duration {
        let base = self.duration_for(track_duration);
        if base.is_zero() {
            Duration::from_secs(10)
        } else {
            (base + self.early_fade_lookahead(track_duration) + Duration::from_secs(5))
                .max(Duration::from_secs(12))
        }
    }

    pub fn early_fade_lookahead(self, track_duration: Option<Duration>) -> Duration {
        let secs = track_duration.map_or(180.0, |d| d.as_secs_f32());
        if secs < 60.0 {
            return Duration::ZERO;
        }
        match self {
            Self::CrossfadeAuto => Duration::from_millis(2500),
            Self::AutoMix => Duration::from_millis(3500),
            Self::None | Self::CrossfadeManual(_) => Duration::ZERO,
        }
    }

    fn auto_crossfade_secs(track_secs: f32) -> f32 {
        if track_secs < 35.0 {
            1.8
        } else if track_secs < 120.0 {
            let t = CurveMath::remap(track_secs, 35.0, 120.0);
            CurveMath::lerp(3.0, 5.0, CurveMath::smoothstep(t))
        } else if track_secs < 240.0 {
            let t = CurveMath::remap(track_secs, 120.0, 240.0);
            CurveMath::lerp(5.0, 6.4, CurveMath::smoothstep(t))
        } else {
            let t = CurveMath::remap(track_secs, 240.0, 360.0);
            CurveMath::lerp(6.4, 7.6, CurveMath::smoothstep(t))
        }
    }

    fn automix_secs(track_secs: f32) -> f32 {
        if track_secs < 40.0 {
            2.4
        } else if track_secs < 120.0 {
            let t = CurveMath::remap(track_secs, 40.0, 120.0);
            CurveMath::lerp(4.2, 6.8, CurveMath::smoothstep(t))
        } else if track_secs < 240.0 {
            let t = CurveMath::remap(track_secs, 120.0, 240.0);
            CurveMath::lerp(6.8, 8.2, CurveMath::smoothstep(t))
        } else {
            let t = CurveMath::remap(track_secs, 240.0, 360.0);
            CurveMath::lerp(8.2, 9.2, CurveMath::smoothstep(t))
        }
    }
}

pub struct CurveMath;

impl CurveMath {
    #[inline]
    pub fn smoothstep(x: f32) -> f32 {
        let t = x.clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    #[inline]
    pub fn smootherstep(x: f32) -> f32 {
        let t = x.clamp(0.0, 1.0);
        t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
    }

    #[inline]
    pub fn warped_progress(p: f32) -> f32 {
        let t = p.clamp(0.0, 1.0);
        0.28 * t + 0.72 * Self::smootherstep(t)
    }

    #[inline]
    pub fn equal_power_out(p: f32) -> f32 {
        (Self::warped_progress(p) * FRAC_PI_2).cos()
    }

    #[inline]
    pub fn equal_power_in(p: f32) -> f32 {
        (Self::warped_progress(p) * FRAC_PI_2).sin()
    }

    #[inline]
    pub fn equal_power_pair(p: f32) -> (f32, f32) {
        let angle = Self::warped_progress(p) * FRAC_PI_2;
        (angle.cos(), angle.sin())
    }

    #[inline]
    pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
        a + (b - a) * t.clamp(0.0, 1.0)
    }

    #[inline]
    pub fn remap(x: f32, start: f32, end: f32) -> f32 {
        let span = end - start;
        if span.abs() < 1e-6 {
            return 0.0;
        }
        ((x - start) / span).clamp(0.0, 1.0)
    }

    #[inline]
    pub fn soft_clip(sample: f32) -> f32 {
        let abs = sample.abs();
        if abs <= 0.90 {
            return sample;
        }
        let excess = (abs - 0.90) * 10.0;
        let compressed = 0.90 + 0.10 * (excess / (1.0 + excess));
        compressed.copysign(sample)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BandGains {
    pub low: f32,
    pub mid: f32,
    pub high: f32,
    pub center_scale: f32,
}

impl BandGains {
    pub const UNITY: Self = Self {
        low: 1.0,
        mid: 1.0,
        high: 1.0,
        center_scale: 1.0,
    };

    pub const SILENT: Self = Self {
        low: 0.0,
        mid: 0.0,
        high: 0.0,
        center_scale: 1.0,
    };

    #[inline]
    pub fn lerp(self, other: Self, t: f32) -> Self {
        Self {
            low: CurveMath::lerp(self.low, other.low, t),
            mid: CurveMath::lerp(self.mid, other.mid, t),
            high: CurveMath::lerp(self.high, other.high, t),
            center_scale: CurveMath::lerp(self.center_scale, other.center_scale, t),
        }
    }

    #[inline]
    pub fn outgoing(is_automix: bool, p: f32, swap_center: f32) -> Self {
        let p = p.clamp(0.0, 1.0);
        if !is_automix {
            let g_out = CurveMath::equal_power_out(p);
            let mid_dip = 1.0 - 0.15 * (p * PI).sin().powi(2);
            let bass_taper = 1.0 - 0.14 * CurveMath::smootherstep(CurveMath::remap(p, 0.42, 0.88));
            return Self {
                low: g_out * mid_dip * bass_taper,
                mid: g_out,
                high: g_out,
                center_scale: 1.0,
            };
        }
        let swap_start = (swap_center - 0.16).clamp(0.18, 0.45);
        let swap_end = (swap_center + 0.16).clamp(0.52, 0.82);
        let low = if p < swap_start {
            let t = CurveMath::remap(p, 0.0, swap_start);
            CurveMath::lerp(1.0, 0.92, CurveMath::smoothstep(t))
        } else if p < swap_end {
            let t = CurveMath::remap(p, swap_start, swap_end);
            CurveMath::lerp(0.92, 0.06, CurveMath::smootherstep(t))
        } else {
            let t = CurveMath::remap(p, swap_end, 1.0);
            CurveMath::lerp(0.06, 0.0, CurveMath::smootherstep(t))
        };
        let mid = if p < 0.32 {
            let t = CurveMath::remap(p, 0.0, 0.32);
            CurveMath::lerp(1.0, 0.85, CurveMath::smoothstep(t))
        } else if p < 0.72 {
            let t = CurveMath::remap(p, 0.32, 0.72);
            CurveMath::lerp(0.85, 0.30, CurveMath::smootherstep(t))
        } else {
            let t = CurveMath::remap(p, 0.72, 1.0);
            0.30 * (1.0 - CurveMath::smootherstep(t))
        };
        let high = if p < 0.32 {
            let t = CurveMath::remap(p, 0.0, 0.32);
            CurveMath::lerp(1.0, 0.76, CurveMath::smoothstep(t))
        } else if p < 0.70 {
            let t = CurveMath::remap(p, 0.32, 0.70);
            CurveMath::lerp(0.76, 0.20, CurveMath::smootherstep(t))
        } else {
            let t = CurveMath::remap(p, 0.70, 1.0);
            0.20 * (1.0 - CurveMath::smootherstep(t))
        };
        let center_pocket = (p * PI).sin().powi(2);
        let center_scale = 1.0 - 0.18 * center_pocket;
        Self {
            low,
            mid,
            high,
            center_scale,
        }
    }

    #[inline]
    pub fn incoming(is_automix: bool, p: f32, swap_center: f32) -> Self {
        let p = p.clamp(0.0, 1.0);
        if !is_automix {
            let g_in = CurveMath::equal_power_in(p);
            let mid_dip = 1.0 - 0.15 * (p * PI).sin().powi(2);
            let bass_bloom = 0.86 + 0.14 * CurveMath::smootherstep(CurveMath::remap(p, 0.12, 0.58));
            return Self {
                low: g_in * mid_dip * bass_bloom,
                mid: g_in,
                high: g_in,
                center_scale: 1.0,
            };
        }
        let swap_start = (swap_center - 0.16).clamp(0.18, 0.45);
        let swap_end = (swap_center + 0.16).clamp(0.52, 0.82);
        let low = if p < swap_start {
            let t = CurveMath::remap(p, 0.0, swap_start);
            CurveMath::lerp(0.0, 0.22, CurveMath::smootherstep(t))
        } else if p < swap_end {
            let t = CurveMath::remap(p, swap_start, swap_end);
            CurveMath::lerp(0.22, 0.94, CurveMath::smootherstep(t))
        } else {
            let t = CurveMath::remap(p, swap_end, 1.0);
            CurveMath::lerp(0.94, 1.0, CurveMath::smootherstep(t))
        };
        let mid = if p < 0.32 {
            let t = CurveMath::remap(p, 0.0, 0.32);
            0.56 * CurveMath::equal_power_in(t)
        } else if p < 0.72 {
            let t = CurveMath::remap(p, 0.32, 0.72);
            CurveMath::lerp(0.56, 0.92, CurveMath::smootherstep(t))
        } else {
            let t = CurveMath::remap(p, 0.72, 1.0);
            CurveMath::lerp(0.92, 1.0, CurveMath::smootherstep(t))
        };
        let high = if p < 0.32 {
            let t = CurveMath::remap(p, 0.0, 0.32);
            0.62 * CurveMath::equal_power_in(t)
        } else if p < 0.70 {
            let t = CurveMath::remap(p, 0.32, 0.70);
            CurveMath::lerp(0.62, 0.95, CurveMath::smootherstep(t))
        } else {
            let t = CurveMath::remap(p, 0.70, 1.0);
            CurveMath::lerp(0.95, 1.0, CurveMath::smootherstep(t))
        };
        Self {
            low,
            mid,
            high,
            center_scale: 1.0,
        }
    }
}

#[derive(Clone, Copy)]
pub struct SvfCoeffs {
    a1: f32,
    a2: f32,
    a3: f32,
    k: f32,
}

impl SvfCoeffs {
    pub fn crossover(freq_hz: f32, sample_rate: u32) -> Self {
        let rate = sample_rate.max(8000) as f32;
        let normalized = (freq_hz / rate).clamp(0.0005, 0.45);
        let g = (PI * normalized).tan();
        let k = SQRT_2;
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        Self { a1, a2, a3, k }
    }
}

#[derive(Clone, Copy, Default)]
pub struct SvfState {
    ic1eq: f32,
    ic2eq: f32,
}

impl SvfState {
    #[inline]
    pub fn split(&mut self, input: f32, coeffs: &SvfCoeffs) -> (f32, f32) {
        let v3 = input - self.ic2eq;
        let v1 = coeffs.a1 * self.ic1eq + coeffs.a2 * v3;
        let v2 = self.ic2eq + coeffs.a2 * self.ic1eq + coeffs.a3 * v3;
        self.ic1eq = 2.0 * v1 - self.ic1eq;
        self.ic2eq = 2.0 * v2 - self.ic2eq;
        let low = v2 + coeffs.k * v1;
        let high = input - low;
        (low, high)
    }

    pub fn reset(&mut self) {
        self.ic1eq = 0.0;
        self.ic2eq = 0.0;
    }
}

#[derive(Clone, Copy, Default)]
pub struct ThreeBandChannel {
    low_split: SvfState,
    high_split: SvfState,
}

impl ThreeBandChannel {
    #[inline]
    pub fn split(
        &mut self,
        input: f32,
        low_coeffs: &SvfCoeffs,
        high_coeffs: &SvfCoeffs,
    ) -> (f32, f32, f32) {
        let (low, upper) = self.low_split.split(input, low_coeffs);
        let (mid, high) = self.high_split.split(upper, high_coeffs);
        (low, mid, high)
    }

    #[inline]
    pub fn process(
        &mut self,
        input: f32,
        low_coeffs: &SvfCoeffs,
        high_coeffs: &SvfCoeffs,
        gains: BandGains,
    ) -> f32 {
        let (low, mid, high) = self.split(input, low_coeffs, high_coeffs);
        low * gains.low + mid * gains.mid + high * gains.high
    }

    pub fn reset(&mut self) {
        self.low_split.reset();
        self.high_split.reset();
    }
}

#[derive(Clone, Copy)]
pub struct ThreeBandDeck {
    channels: [ThreeBandChannel; 2],
    low_coeffs: SvfCoeffs,
    high_coeffs: SvfCoeffs,
    rate: u32,
}

impl ThreeBandDeck {
    pub fn new(sample_rate: u32) -> Self {
        let rate = sample_rate.max(8000);
        Self {
            channels: [ThreeBandChannel::default(); 2],
            low_coeffs: SvfCoeffs::crossover(LOW_CROSSOVER_HZ, rate),
            high_coeffs: SvfCoeffs::crossover(HIGH_CROSSOVER_HZ, rate),
            rate,
        }
    }

    pub fn set_rate(&mut self, sample_rate: u32) {
        let rate = sample_rate.max(8000);
        if self.rate != rate {
            self.rate = rate;
            self.low_coeffs = SvfCoeffs::crossover(LOW_CROSSOVER_HZ, rate);
            self.high_coeffs = SvfCoeffs::crossover(HIGH_CROSSOVER_HZ, rate);
            self.reset();
        }
    }

    pub fn reset(&mut self) {
        self.channels[0].reset();
        self.channels[1].reset();
    }

    #[inline]
    pub fn process_stereo(&mut self, l: f32, r: f32, gains: BandGains) -> (f32, f32) {
        let out_l = self.channels[0].process(l, &self.low_coeffs, &self.high_coeffs, gains);
        let out_r = self.channels[1].process(r, &self.low_coeffs, &self.high_coeffs, gains);
        if (gains.center_scale - 1.0).abs() < 1e-4 {
            return (out_l, out_r);
        }
        let mid = 0.5 * (out_l + out_r) * gains.center_scale;
        let side = 0.5 * (out_l - out_r);
        (mid + side, mid - side)
    }

    #[inline]
    pub fn process_channel(&mut self, ch: usize, sample: f32, gains: BandGains) -> f32 {
        if ch < 2 {
            self.channels[ch].process(sample, &self.low_coeffs, &self.high_coeffs, gains)
        } else {
            sample * gains.mid
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct EnvelopeFollower {
    fast_rms: f32,
    slow_rms: f32,
    peak_rms: f32,
    observed_chunks: u32,
}

impl EnvelopeFollower {
    pub fn new(_sample_rate: u32) -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn observe_block(&mut self, samples: &[f32], _channels: usize) {
        self.observe(samples);
    }

    pub fn observe(&mut self, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        let mut sum_sq = 0.0f32;
        for &s in samples {
            sum_sq += s * s;
        }
        let rms = (sum_sq / samples.len() as f32).sqrt();
        if self.observed_chunks == 0 {
            self.fast_rms = rms;
            self.slow_rms = rms;
            self.peak_rms = rms;
        } else {
            let fast_alpha = if rms > self.fast_rms { 0.35 } else { 0.18 };
            self.fast_rms += fast_alpha * (rms - self.fast_rms);
            self.slow_rms += 0.06 * (rms - self.slow_rms);
            if rms > self.peak_rms {
                self.peak_rms = rms;
            }
        }
        self.observed_chunks = self.observed_chunks.saturating_add(1);
    }

    pub fn rms(&self) -> f32 {
        self.fast_rms
    }

    pub fn is_fading_out(&self) -> bool {
        self.observed_chunks >= 12
            && self.peak_rms > 0.06
            && self.fast_rms < EARLY_FADE_RMS_THRESHOLD
            && self.fast_rms < self.slow_rms * 0.72
    }

    pub fn silence_assist(&self, p: f32) -> f32 {
        if self.observed_chunks < 4 || p >= 0.92 {
            return 0.0;
        }
        if self.fast_rms >= QUIET_TAIL_RMS_THRESHOLD {
            return 0.0;
        }
        let quietness = 1.0 - (self.fast_rms / QUIET_TAIL_RMS_THRESHOLD).clamp(0.0, 1.0);
        0.38 * quietness * CurveMath::smoothstep(CurveMath::remap(p, 0.08, 0.65))
    }
}

#[derive(Clone, Copy)]
pub struct BeatAccentDetector {
    low_filter: SvfState,
    low_coeffs: SvfCoeffs,
    fast_env: f32,
    slow_env: f32,
    locked_center: Option<f32>,
}

impl BeatAccentDetector {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            low_filter: SvfState::default(),
            low_coeffs: SvfCoeffs::crossover(115.0, sample_rate),
            fast_env: 0.0,
            slow_env: 0.0,
            locked_center: None,
        }
    }

    pub fn observe_block(&mut self, samples: &[f32], channels: usize, p: f32) -> f32 {
        if let Some(center) = self.locked_center {
            return center;
        }
        if samples.is_empty() || channels == 0 {
            return 0.50;
        }
        let frames = samples.len() / channels;
        let step = (frames / 32).max(1);
        let mut max_flux = 0.0f32;
        let mut f = 0;
        while f < frames {
            let s = samples[f * channels].abs();
            let (low, _) = self.low_filter.split(s, &self.low_coeffs);
            let energy = low * low;
            self.fast_env += 0.25 * (energy - self.fast_env);
            self.slow_env += 0.03 * (energy - self.slow_env);
            let flux = (self.fast_env - self.slow_env * 1.45).max(0.0);
            if flux > max_flux {
                max_flux = flux;
            }
            f += step;
        }
        if (0.36..=0.56).contains(&p) && max_flux > 0.0025 {
            let center = (p + 0.04).clamp(0.42, 0.56);
            self.locked_center = Some(center);
            return center;
        }
        0.50
    }
}

pub struct SilenceTrimmer;

impl SilenceTrimmer {
    pub fn trim<S: Iterator<Item = f32>>(
        source: &mut S,
        sample_rate: u32,
        channels: u16,
        _gain: f32,
    ) -> (Vec<f32>, u64, Duration) {
        let (skipped_samples, ring) = Self::trim_leading(source, sample_rate, channels);
        let ch = u64::from(channels).max(1);
        let rate = f64::from(sample_rate).max(1.0);
        let frames = skipped_samples / ch;
        let dur = Duration::from_secs_f64(frames as f64 / rate);
        (ring, skipped_samples, dur)
    }

    pub fn trim_leading<S: Iterator<Item = f32>>(
        source: &mut S,
        sample_rate: u32,
        channels: u16,
    ) -> (u64, Vec<f32>) {
        let ch = usize::from(channels).max(1);
        let rate = u64::from(sample_rate.max(8000));
        let max_frames = (rate * MAX_SILENCE_TRIM_MS / 1000) as usize;
        let cushion_frames = ((rate * ONSET_CUSHION_MS / 1000) as usize).max(1);
        let cushion_samples = cushion_frames * ch;
        let mut ring = Vec::with_capacity(cushion_samples + ch);
        let mut skipped_samples = 0u64;
        for _ in 0..max_frames {
            let mut frame_peak = 0.0f32;
            let frame_start = ring.len();
            for _ in 0..ch {
                let Some(s) = source.next() else {
                    return (0, ring);
                };
                let abs = s.abs();
                if abs > frame_peak {
                    frame_peak = abs;
                }
                ring.push(s);
            }
            if frame_peak >= SILENCE_THRESHOLD {
                return (skipped_samples, ring);
            }
            if frame_start >= cushion_samples {
                ring.drain(0..ch);
                skipped_samples += ch as u64;
            }
        }
        (skipped_samples, ring)
    }
}

pub struct StreamResampler {
    source_rate: u32,
    target_rate: u32,
    source_channels: usize,
    target_channels: usize,
    step: f64,
    phase: f64,
    history: [[f32; 4]; 2],
    primed: bool,
    in_buf: Vec<f32>,
    in_pos: usize,
}

impl StreamResampler {
    pub fn new(
        source_rate: u32,
        source_channels: u16,
        target_rate: u32,
        target_channels: u16,
    ) -> Option<Self> {
        let s_rate = source_rate.max(1);
        let t_rate = target_rate.max(1);
        let s_ch = usize::from(source_channels).max(1);
        let t_ch = usize::from(target_channels).max(1);
        if s_rate == t_rate && s_ch == t_ch {
            return None;
        }
        Some(Self {
            source_rate: s_rate,
            target_rate: t_rate,
            source_channels: s_ch,
            target_channels: t_ch,
            step: s_rate as f64 / t_rate as f64,
            phase: 0.0,
            history: [[0.0; 4]; 2],
            primed: false,
            in_buf: Vec::new(),
            in_pos: 0,
        })
    }

    pub fn reset(&mut self) {
        self.phase = 0.0;
        self.history = [[0.0; 4]; 2];
        self.primed = false;
        self.in_buf.clear();
        self.in_pos = 0;
    }

    fn pull_source_frame<S: Iterator<Item = f32>>(
        source_channels: usize,
        prefix: &mut Vec<f32>,
        prefix_pos: &mut usize,
        source: &mut S,
        gain: f32,
    ) -> Option<[f32; 2]> {
        let mut first = 0.0f32;
        let mut second = 0.0f32;
        for c in 0..source_channels {
            let raw = if *prefix_pos < prefix.len() {
                let val = prefix[*prefix_pos];
                *prefix_pos += 1;
                if *prefix_pos >= prefix.len() {
                    prefix.clear();
                    *prefix_pos = 0;
                }
                val
            } else {
                source.next()?
            };
            let scaled = raw * gain;
            if c == 0 {
                first = scaled;
                second = scaled;
            } else if c == 1 {
                second = scaled;
            }
        }
        Some([first, second])
    }

    pub fn take<S: Iterator<Item = f32>>(
        &mut self,
        prefix: &mut Vec<f32>,
        prefix_pos: &mut usize,
        source: &mut S,
        gain: f32,
        target_frames: usize,
        out: &mut Vec<f32>,
    ) -> u64 {
        let mut consumed_source_frames = 0u64;
        if self.source_rate == self.target_rate {
            for _ in 0..target_frames {
                let Some(pair) = Self::pull_source_frame(
                    self.source_channels,
                    prefix,
                    prefix_pos,
                    source,
                    gain,
                ) else {
                    break;
                };
                consumed_source_frames += 1;
                self.push_output_frame(pair, out);
            }
            return consumed_source_frames;
        }
        if !self.primed {
            let Some(first) = Self::pull_source_frame(
                self.source_channels,
                prefix,
                prefix_pos,
                source,
                gain,
            ) else {
                return 0;
            };
            consumed_source_frames += 1;
            let second = Self::pull_source_frame(
                self.source_channels,
                prefix,
                prefix_pos,
                source,
                gain,
            )
            .unwrap_or(first);
            if second != first {
                consumed_source_frames += 1;
            }
            for ch in 0..2 {
                self.history[ch] = [first[ch], first[ch], second[ch], second[ch]];
            }
            self.primed = true;
        }
        for _ in 0..target_frames {
            while self.phase >= 1.0 {
                let Some(next_pair) = Self::pull_source_frame(
                    self.source_channels,
                    prefix,
                    prefix_pos,
                    source,
                    gain,
                ) else {
                    return consumed_source_frames;
                };
                consumed_source_frames += 1;
                for ch in 0..2 {
                    let h = &mut self.history[ch];
                    h[0] = h[1];
                    h[1] = h[2];
                    h[2] = h[3];
                    h[3] = next_pair[ch];
                }
                self.phase -= 1.0;
            }
            let t = self.phase as f32;
            let mut interpolated = [0.0f32; 2];
            for ch in 0..2 {
                let [y0, y1, y2, y3] = self.history[ch];
                let c0 = y1;
                let c1 = 0.5 * (y2 - y0);
                let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
                let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
                interpolated[ch] = ((c3 * t + c2) * t + c1) * t + c0;
            }
            self.push_output_frame(interpolated, out);
            self.phase += self.step;
        }
        consumed_source_frames
    }

    #[inline]
    fn push_output_frame(&self, pair: [f32; 2], out: &mut Vec<f32>) {
        if self.target_channels == 1 {
            out.push(0.5 * (pair[0] + pair[1]));
        } else {
            out.push(pair[0]);
            out.push(pair[1]);
            for _ in 2..self.target_channels {
                out.push(0.0);
            }
        }
    }

    fn next_input_frame(
        &mut self,
        pull: &mut impl FnMut(usize, &mut Vec<f32>),
    ) -> Option<[f32; 2]> {
        let ch = self.source_channels;
        if self.in_pos + ch > self.in_buf.len() {
            if self.in_pos < self.in_buf.len() {
                self.in_buf.drain(0..self.in_pos);
            } else {
                self.in_buf.clear();
            }
            self.in_pos = 0;
            let needed = ch * 256;
            pull(needed, &mut self.in_buf);
            if self.in_pos + ch > self.in_buf.len() {
                return None;
            }
        }
        let first = self.in_buf[self.in_pos];
        let second = if ch > 1 {
            self.in_buf[self.in_pos + 1]
        } else {
            first
        };
        self.in_pos += ch;
        Some([first, second])
    }

    pub fn produce(
        &mut self,
        target_frames: usize,
        out: &mut Vec<f32>,
        mut pull: impl FnMut(usize, &mut Vec<f32>),
    ) {
        if self.source_rate == self.target_rate {
            for _ in 0..target_frames {
                let Some(pair) = self.next_input_frame(&mut pull) else {
                    break;
                };
                self.push_output_frame(pair, out);
            }
            return;
        }
        if !self.primed {
            let Some(first) = self.next_input_frame(&mut pull) else {
                return;
            };
            let second = self.next_input_frame(&mut pull).unwrap_or(first);
            for ch in 0..2 {
                self.history[ch] = [first[ch], first[ch], second[ch], second[ch]];
            }
            self.primed = true;
        }
        for _ in 0..target_frames {
            while self.phase >= 1.0 {
                let Some(next_pair) = self.next_input_frame(&mut pull) else {
                    return;
                };
                for ch in 0..2 {
                    let h = &mut self.history[ch];
                    h[0] = h[1];
                    h[1] = h[2];
                    h[2] = h[3];
                    h[3] = next_pair[ch];
                }
                self.phase -= 1.0;
            }
            let t = self.phase as f32;
            let mut interpolated = [0.0f32; 2];
            for ch in 0..2 {
                let [y0, y1, y2, y3] = self.history[ch];
                let c0 = y1;
                let c1 = 0.5 * (y2 - y0);
                let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
                let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
                interpolated[ch] = ((c3 * t + c2) * t + c1) * t + c0;
            }
            self.push_output_frame(interpolated, out);
            self.phase += self.step;
        }
    }
}

pub struct DualStreamMixer {
    channels: usize,
    total_frames: usize,
    done_frames: usize,
    is_automix: bool,
    out_deck: ThreeBandDeck,
    in_deck: ThreeBandDeck,
    out_env: EnvelopeFollower,
    beat_detector: BeatAccentDetector,
}

impl DualStreamMixer {
    pub fn new(sample_rate: u32, channels: u16, total_frames: usize, is_automix: bool) -> Self {
        let rate = sample_rate.max(8000);
        let ch = usize::from(channels).max(1);
        Self {
            channels: ch,
            total_frames: total_frames.max(1),
            done_frames: 0,
            is_automix,
            out_deck: ThreeBandDeck::new(rate),
            in_deck: ThreeBandDeck::new(rate),
            out_env: EnvelopeFollower::default(),
            beat_detector: BeatAccentDetector::new(rate),
        }
    }

    pub fn is_finished(&self) -> bool {
        self.done_frames >= self.total_frames
    }

    pub fn mix_block(&mut self, out_samples: &mut [f32], in_samples: &[f32]) -> bool {
        let ch = self.channels;
        let frames = out_samples.len() / ch;
        if frames == 0 || in_samples.is_empty() {
            return true;
        }
        let in_frames = in_samples.len() / ch;
        let mix_frames = frames.min(in_frames);
        self.out_env.observe(out_samples);
        let inv_total = 1.0 / self.total_frames.max(1) as f32;
        let p0 = (self.done_frames as f32 * inv_total).clamp(0.0, 1.0);
        let p1 = ((self.done_frames + mix_frames) as f32 * inv_total).clamp(0.0, 1.0);
        let swap_center = if self.is_automix {
            self.beat_detector.observe_block(in_samples, ch, 0.5 * (p0 + p1))
        } else {
            0.50
        };
        let assist0 = self.out_env.silence_assist(p0);
        let assist1 = self.out_env.silence_assist(p1);
        let p0_in = (p0 + assist0 * (1.0 - p0)).clamp(0.0, 1.0);
        let p1_in = (p1 + assist1 * (1.0 - p1)).clamp(0.0, 1.0);
        let out_g0 = BandGains::outgoing(self.is_automix, p0, swap_center);
        let out_g1 = BandGains::outgoing(self.is_automix, p1, swap_center);
        let in_g0 = BandGains::incoming(self.is_automix, p0_in, swap_center);
        let in_g1 = BandGains::incoming(self.is_automix, p1_in, swap_center);
        let inv_frames = 1.0 / mix_frames.max(1) as f32;
        if ch == 2 {
            for f in 0..mix_frames {
                let t = f as f32 * inv_frames;
                let og = out_g0.lerp(out_g1, t);
                let ig = in_g0.lerp(in_g1, t);
                let idx = f * 2;
                let (ol, or) = self.out_deck.process_stereo(out_samples[idx], out_samples[idx + 1], og);
                let (il, ir) = self.in_deck.process_stereo(in_samples[idx], in_samples[idx + 1], ig);
                out_samples[idx] = CurveMath::soft_clip(ol + il);
                out_samples[idx + 1] = CurveMath::soft_clip(or + ir);
            }
        } else {
            for f in 0..mix_frames {
                let t = f as f32 * inv_frames;
                let og = out_g0.lerp(out_g1, t);
                let ig = in_g0.lerp(in_g1, t);
                for c in 0..ch {
                    let idx = f * ch + c;
                    let o = self.out_deck.process_channel(c, out_samples[idx], og);
                    let i = self.in_deck.process_channel(c, in_samples[idx], ig);
                    out_samples[idx] = CurveMath::soft_clip(o + i);
                }
            }
        }
        if mix_frames < frames {
            for f in mix_frames..frames {
                for c in 0..ch {
                    let idx = f * ch + c;
                    out_samples[idx] = self.out_deck.process_channel(c, out_samples[idx], out_g1);
                }
            }
            return true;
        }
        self.done_frames += mix_frames;
        self.done_frames >= self.total_frames
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TransitionSignal {
    pub config: TransitionConfig,
    pub intro_trigger: usize,
    pub intro_secs: f32,
    pub outro_trigger: usize,
    pub outro_secs: f32,
}

impl Default for TransitionSignal {
    fn default() -> Self {
        Self {
            config: TransitionConfig::None,
            intro_trigger: 0,
            intro_secs: 0.0,
            outro_trigger: 0,
            outro_secs: 0.0,
        }
    }
}

#[derive(Default)]
pub struct TransitionSync {
    incoming_ready: AtomicBool,
    deck_epochs: [AtomicU64; 2],
}

impl TransitionSync {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bump_deck(&self, deck_idx: usize) -> u64 {
        self.deck_epochs[deck_idx.min(1)]
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
    }

    pub fn deck_epoch(&self, deck_idx: usize) -> u64 {
        self.deck_epochs[deck_idx.min(1)].load(Ordering::Acquire)
    }

    pub fn prepare_overlap(&self) {
        self.incoming_ready.store(false, Ordering::Release);
    }

    pub fn mark_incoming_ready(&self) {
        self.incoming_ready.store(true, Ordering::Release);
    }

    pub fn is_incoming_ready(&self) -> bool {
        self.incoming_ready.load(Ordering::Acquire)
    }
}

pub struct SingleDeckProcessor {
    deck: ThreeBandDeck,
    beat_detector: BeatAccentDetector,
    seen_intro_trigger: usize,
    seen_outro_trigger: usize,
    intro_frames: usize,
    total_intro: usize,
    outro_frames: usize,
    total_outro: usize,
    outro_wait_frames: usize,
    sample_rate: u32,
    channels: usize,
    sync: Arc<TransitionSync>,
}

impl SingleDeckProcessor {
    pub fn new(sample_rate: u32, channels: usize, sync: Arc<TransitionSync>) -> Self {
        let rate = sample_rate.max(8000);
        Self {
            deck: ThreeBandDeck::new(rate),
            beat_detector: BeatAccentDetector::new(rate),
            seen_intro_trigger: 0,
            seen_outro_trigger: 0,
            intro_frames: 0,
            total_intro: 0,
            outro_frames: 0,
            total_outro: 0,
            outro_wait_frames: 0,
            sample_rate: rate,
            channels: channels.max(1),
            sync,
        }
    }

    pub fn on_start(&mut self, signal: &TransitionSignal) {
        self.seen_intro_trigger = signal.intro_trigger;
        if signal.intro_trigger > 0 && signal.intro_secs > 0.0 {
            let secs = signal.intro_secs.clamp(0.5, 12.0);
            self.total_intro = (secs * self.sample_rate as f32) as usize;
        } else {
            self.total_intro = 0;
        }
        self.intro_frames = 0;
        self.total_outro = 0;
        self.outro_frames = 0;
        self.outro_wait_frames = 0;
        self.deck.reset();
        self.beat_detector = BeatAccentDetector::new(self.sample_rate);
    }

    pub fn process(&mut self, samples: &mut [f32], ctrl: &TransitionSignal) {
        let sync = Arc::clone(&self.sync);
        let channels = self.channels;
        self.process_packet(samples, channels, Some(*ctrl), &sync);
    }

    pub fn process_packet(
        &mut self,
        samples: &mut [f32],
        channels: usize,
        signal: Option<TransitionSignal>,
        sync: &TransitionSync,
    ) {
        let Some(ctrl) = signal else {
            return;
        };
        if ctrl.intro_trigger != self.seen_intro_trigger {
            self.seen_intro_trigger = ctrl.intro_trigger;
            self.total_outro = 0;
            self.outro_frames = 0;
            self.outro_wait_frames = 0;
            if ctrl.intro_trigger > 0 && ctrl.intro_secs > 0.0 && ctrl.config.is_active() {
                let secs = ctrl.intro_secs.clamp(0.5, 12.0);
                self.total_intro = (secs * self.sample_rate as f32) as usize;
            } else {
                self.total_intro = 0;
            }
            self.intro_frames = 0;
            self.deck.reset();
            self.beat_detector = BeatAccentDetector::new(self.sample_rate);
        }
        if ctrl.outro_trigger != self.seen_outro_trigger {
            self.seen_outro_trigger = ctrl.outro_trigger;
            if ctrl.outro_trigger > 0 && ctrl.outro_secs > 0.0 && ctrl.config.is_active() {
                let secs = ctrl.outro_secs.clamp(0.5, 12.0);
                self.total_outro = (secs * self.sample_rate as f32) as usize;
                self.outro_frames = 0;
                self.outro_wait_frames = 0;
                self.deck.reset();
            } else {
                self.total_outro = 0;
                self.outro_frames = 0;
            }
        }
        let is_automix = ctrl.config.is_automix();
        if self.total_outro > 0 {
            let max_wait = (self.sample_rate as usize) / 2;
            if !sync.is_incoming_ready() && self.outro_wait_frames < max_wait {
                self.outro_wait_frames += samples.len() / channels.max(1);
                return;
            }
            if self.outro_frames < self.total_outro {
                self.apply_outro(samples, channels, is_automix);
            } else {
                samples.fill(0.0);
            }
        } else if self.total_intro > 0 && self.intro_frames < self.total_intro {
            if self.intro_frames == 0 {
                sync.mark_incoming_ready();
            }
            self.apply_intro(samples, channels, is_automix);
        }
    }

    fn apply_intro(&mut self, samples: &mut [f32], channels: usize, is_automix: bool) {
        let ch = channels.max(1);
        let frames = samples.len() / ch;
        if frames == 0 {
            return;
        }
        let inv_total = 1.0 / self.total_intro.max(1) as f32;
        let p0 = (self.intro_frames as f32 * inv_total).clamp(0.0, 1.0);
        let p1 = ((self.intro_frames + frames) as f32 * inv_total).clamp(0.0, 1.0);
        let swap_center = if is_automix {
            self.beat_detector.observe_block(samples, ch, 0.5 * (p0 + p1))
        } else {
            0.50
        };
        let g0 = BandGains::incoming(is_automix, p0, swap_center);
        let g1 = BandGains::incoming(is_automix, p1, swap_center);
        let inv_frames = 1.0 / frames.max(1) as f32;
        if ch == 2 {
            for f in 0..frames {
                let gains = g0.lerp(g1, f as f32 * inv_frames);
                let idx = f * 2;
                let (l, r) = self.deck.process_stereo(samples[idx], samples[idx + 1], gains);
                samples[idx] = CurveMath::soft_clip(l);
                samples[idx + 1] = CurveMath::soft_clip(r);
            }
        } else {
            for f in 0..frames {
                let gains = g0.lerp(g1, f as f32 * inv_frames);
                for c in 0..ch {
                    let idx = f * ch + c;
                    let val = self.deck.process_channel(c, samples[idx], gains);
                    samples[idx] = CurveMath::soft_clip(val);
                }
            }
        }
        self.intro_frames += frames;
    }

    fn apply_outro(&mut self, samples: &mut [f32], channels: usize, is_automix: bool) {
        let ch = channels.max(1);
        let frames = samples.len() / ch;
        if frames == 0 {
            return;
        }
        let inv_total = 1.0 / self.total_outro.max(1) as f32;
        let p0 = (self.outro_frames as f32 * inv_total).clamp(0.0, 1.0);
        let p1 = ((self.outro_frames + frames) as f32 * inv_total).clamp(0.0, 1.0);
        let g0 = BandGains::outgoing(is_automix, p0, 0.50);
        let g1 = BandGains::outgoing(is_automix, p1, 0.50);
        let inv_frames = 1.0 / frames.max(1) as f32;
        if ch == 2 {
            for f in 0..frames {
                let gains = g0.lerp(g1, f as f32 * inv_frames);
                let idx = f * 2;
                let (l, r) = self.deck.process_stereo(samples[idx], samples[idx + 1], gains);
                samples[idx] = CurveMath::soft_clip(l);
                samples[idx + 1] = CurveMath::soft_clip(r);
            }
        } else {
            for f in 0..frames {
                let gains = g0.lerp(g1, f as f32 * inv_frames);
                for c in 0..ch {
                    let idx = f * ch + c;
                    let val = self.deck.process_channel(c, samples[idx], gains);
                    samples[idx] = CurveMath::soft_clip(val);
                }
            }
        }
        self.outro_frames += frames;
    }
}
