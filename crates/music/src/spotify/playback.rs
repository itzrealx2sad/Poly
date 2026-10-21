// SPDX-License-Identifier: GPL-3.0-or-later
// Portions Copyright (C) Sonora Contributors
// Modifications Copyright (C) 2026 Poly Project Contributors

use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use librespot_core::{Session, SpotifyUri};
use librespot_playback::config::{Bitrate, PlayerConfig};
use librespot_playback::mixer::NoOpVolume;
use librespot_playback::player::{Player, PlayerEvent, PlayerEventChannel};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::audio::{Chain, Volume};
use crate::sink::Cue;
use crate::spectrum::Spectrum;
use crate::spotify::sink::{OutputSink, TransitionSignal, TransitionSync};
use crate::{
    PlaybackConfig, PlaybackEvent, PlaybackEvents, PlaybackFactory, Player as MusicPlayer,
    StreamQuality, TransitionConfig,
};

const TRACK_PREFIX: &str = "spotify:track:";
const STALE_POSITIONS: u8 = 2;

struct Deck {
    player: Arc<Player>,
    transition: Arc<RwLock<TransitionSignal>>,
    cue: Cue,
}

impl Deck {
    fn spawn_event_forwarder(
        deck_idx: usize,
        mut rx: PlayerEventChannel,
        active_deck: Arc<RwLock<usize>>,
        events_tx: UnboundedSender<PlayerEvent>,
    ) {
        std::thread::spawn(move || {
            while let Some(event) = rx.blocking_recv() {
                let is_active = active_deck.read().ok().map_or(false, |d| *d == deck_idx);
                if is_active && events_tx.send(event).is_err() {
                    break;
                }
            }
        });
    }
}

pub struct Events {
    player: UnboundedReceiver<PlayerEvent>,
    output: UnboundedReceiver<()>,
    cue_a: Cue,
    cue_b: Cue,
    active_deck: Arc<RwLock<usize>>,
    written_a: UnboundedReceiver<()>,
    written_b: UnboundedReceiver<()>,
    held: Option<PlaybackEvent>,
    stale: u8,
    active_id: Arc<RwLock<Option<String>>>,
}

#[async_trait]
impl PlaybackEvents for Events {
    async fn next(&mut self) -> Option<PlaybackEvent> {
        loop {
            let active_is_a = self.active_deck.read().ok().map_or(true, |d| *d == 0);
            tokio::select! {
                biased;
                event = self.player.recv() => {
                    let raw_event = event?;
                    let mut new_active_id = None;
                    match &raw_event {
                        PlayerEvent::Playing { track_id, .. } | PlayerEvent::Loading { track_id, .. } => {
                            new_active_id = Some(track_id.to_id());
                        }
                        _ => {}
                    }
                    if let Some(new_id) = new_active_id {
                        if let Ok(mut active) = self.active_id.write() {
                            if active.as_deref() != Some(&new_id) {
                                *active = Some(new_id.clone());
                            }
                        }
                    }
                    let Some(event) = translate(raw_event) else { continue };
                    match event {
                        PlaybackEvent::Playing { .. } | PlaybackEvent::Seeked { .. } => {
                            if active_is_a {
                                self.cue_a.arm();
                            } else {
                                self.cue_b.arm();
                            }
                            self.stale = 0;
                            self.held = Some(event);
                        }
                        PlaybackEvent::Position { .. } if (if active_is_a { self.cue_a.cleared() } else { self.cue_b.cleared() }) => {
                            self.stale += 1;
                            if self.stale > STALE_POSITIONS {
                                log::warn!("playback: the engine kept its old position, letting audio through");
                                if active_is_a {
                                    self.cue_a.arm();
                                } else {
                                    self.cue_b.arm();
                                }
                                self.stale = 0;
                            }
                            return Some(event);
                        }
                        PlaybackEvent::Position { .. } | PlaybackEvent::Length { .. } => {
                            return Some(event);
                        }
                        event => {
                            self.held = None;
                            self.stale = 0;
                            return Some(event);
                        }
                    }
                }
                written = self.written_a.recv() => {
                    written?;
                    if active_is_a {
                        if let Some(event) = self.held.take() {
                            return Some(event);
                        }
                    }
                }
                written = self.written_b.recv() => {
                    written?;
                    if !active_is_a {
                        if let Some(event) = self.held.take() {
                            return Some(event);
                        }
                    }
                }
                changed = self.output.recv() => {
                    changed?;
                    return Some(PlaybackEvent::OutputChanged);
                }
            }
        }
    }
}

pub struct Factory(Session);

impl Factory {
    pub fn new(session: Session) -> Self {
        Self(session)
    }
}

impl PlaybackFactory for Factory {
    fn start(&self, config: PlaybackConfig) -> (Box<dyn MusicPlayer>, Box<dyn PlaybackEvents>) {
        let (engine, events) = Engine::start(self.0.clone(), config);
        (Box::new(engine), Box::new(events))
    }
}

pub struct Engine {
    deck_a: Deck,
    deck_b: Deck,
    active_deck: Arc<RwLock<usize>>,
    active_id: Arc<RwLock<Option<String>>>,
    sync: Arc<TransitionSync>,
    volume: Volume,
    spectrum: Spectrum,
    gapless: bool,
}

impl Engine {
    fn start(session: Session, config: PlaybackConfig) -> (Self, Events) {
        let volume = Volume::new(config.gain);
        let spectrum = Spectrum::new();

        let bitrate = match config.quality {
            StreamQuality::Low => Bitrate::Bitrate96,
            StreamQuality::Medium => Bitrate::Bitrate160,
            StreamQuality::High | StreamQuality::Lossless => Bitrate::Bitrate320,
        };

        let player_config = PlayerConfig {
            bitrate,
            normalisation: config.normalisation,
            gapless: config.gapless,
            position_update_interval: Some(config.position_interval),
            ..Default::default()
        };

        let chain = Chain {
            volume: volume.clone(),
            equalizer: config.equalizer.clone(),
            spectrum: spectrum.clone(),
        };
        let (output_tx, output_rx) = unbounded_channel();
        let (events_tx, events_rx) = unbounded_channel();
        let active_deck = Arc::new(RwLock::new(0));
        let active_id = Arc::new(RwLock::new(None));
        let sync = Arc::new(TransitionSync::new());

        let (cue_a, written_a) = Cue::new();
        let transition_a = Arc::new(RwLock::new(TransitionSignal::default()));
        let sink_cue_a = cue_a.clone();
        let sink_trans_a = transition_a.clone();
        let sink_sync_a = sync.clone();
        let out_tx_a = output_tx.clone();
        let chain_a = chain.clone();
        let player_a = Player::new(
            player_config.clone(),
            session.clone(),
            Box::new(NoOpVolume),
            move || OutputSink::boxed(sink_cue_a, chain_a, out_tx_a, sink_trans_a, sink_sync_a),
        );

        let (cue_b, written_b) = Cue::new();
        let transition_b = Arc::new(RwLock::new(TransitionSignal::default()));
        let sink_cue_b = cue_b.clone();
        let sink_trans_b = transition_b.clone();
        let sink_sync_b = sync.clone();
        let out_tx_b = output_tx.clone();
        let chain_b = chain.clone();
        let player_b = Player::new(player_config, session, Box::new(NoOpVolume), move || {
            OutputSink::boxed(sink_cue_b, chain_b, out_tx_b, sink_trans_b, sink_sync_b)
        });

        let rx_a = player_a.get_player_event_channel();
        let rx_b = player_b.get_player_event_channel();

        Deck::spawn_event_forwarder(0, rx_a, active_deck.clone(), events_tx.clone());
        Deck::spawn_event_forwarder(1, rx_b, active_deck.clone(), events_tx);

        let deck_a = Deck {
            player: player_a,
            transition: transition_a,
            cue: cue_a.clone(),
        };
        let deck_b = Deck {
            player: player_b,
            transition: transition_b,
            cue: cue_b.clone(),
        };

        let events = Events {
            player: events_rx,
            output: output_rx,
            cue_a,
            cue_b,
            active_deck: active_deck.clone(),
            written_a,
            written_b,
            held: None,
            stale: 0,
            active_id: active_id.clone(),
        };
        let engine = Self {
            deck_a,
            deck_b,
            active_deck,
            active_id,
            sync,
            volume,
            spectrum,
            gapless: config.gapless,
        };
        (engine, events)
    }

    fn current_transition_config(&self) -> TransitionConfig {
        self.deck_a
            .transition
            .read()
            .ok()
            .map_or(TransitionConfig::None, |t| t.config)
    }

    fn deck(&self, idx: usize) -> &Deck {
        if idx == 0 {
            &self.deck_a
        } else {
            &self.deck_b
        }
    }

    fn load(&self, track_id: &str, at: Duration, seamless: bool) -> Result<()> {
        let uri = track_uri(track_id)?;
        let cur_config = self.current_transition_config();
        let is_transition = seamless && at.is_zero() && cur_config.is_enabled();
        let has_active = self.active_id.read().ok().is_some_and(|id| id.is_some());

        if is_transition && has_active {
            let current_idx = *self.active_deck.read().unwrap();
            let incoming_idx = 1 - current_idx;
            let outro_secs = self
                .deck(current_idx)
                .transition
                .read()
                .ok()
                .map(|c| c.outro_secs)
                .filter(|&s| s > 0.5)
                .unwrap_or_else(|| cur_config.duration_for(None).as_secs_f32());

            if let Ok(mut ctrl) = self.deck(incoming_idx).transition.write() {
                ctrl.config = cur_config;
                ctrl.intro_trigger = ctrl.intro_trigger.wrapping_add(1);
                ctrl.intro_secs = outro_secs;
                ctrl.outro_trigger = 0;
                ctrl.outro_secs = 0.0;
            }

            if let Ok(mut active) = self.active_deck.write() {
                *active = incoming_idx;
            }
            if let Ok(mut active) = self.active_id.write() {
                *active = Some(track_id.to_owned());
            }

            self.sync.bump_deck(incoming_idx);
            let outgoing_epoch = self.sync.deck_epoch(current_idx);
            self.deck(incoming_idx).player.load(uri, true, 0);

            let outgoing_player = self.deck(current_idx).player.clone();
            let sync = self.sync.clone();
            let wait_dur = Duration::from_secs_f32(outro_secs + 1.2);
            std::thread::spawn(move || {
                std::thread::sleep(wait_dur);
                if sync.deck_epoch(current_idx) == outgoing_epoch {
                    outgoing_player.stop();
                }
            });

            return Ok(());
        }

        let active_idx = *self.active_deck.read().unwrap();
        let other_idx = 1 - active_idx;
        self.sync.bump_deck(active_idx);
        self.sync.bump_deck(other_idx);
        self.deck(other_idx).player.stop();

        if !seamless || !self.gapless {
            self.deck(active_idx).cue.clear();
        }
        if let Ok(mut ctrl) = self.deck(active_idx).transition.write() {
            ctrl.intro_trigger = ctrl.intro_trigger.wrapping_add(1);
            ctrl.intro_secs = 0.0;
            ctrl.outro_trigger = 0;
            ctrl.outro_secs = 0.0;
        }
        if let Ok(mut active) = self.active_id.write() {
            *active = Some(track_id.to_owned());
        }
        self.deck(active_idx)
            .player
            .load(uri, true, at.as_millis() as u32);
        Ok(())
    }

    fn load_paused_at(&self, track_id: &str, at: Duration) -> Result<()> {
        let uri = track_uri(track_id)?;
        let active_idx = *self.active_deck.read().unwrap();
        let other_idx = 1 - active_idx;
        self.sync.bump_deck(active_idx);
        self.sync.bump_deck(other_idx);
        self.deck(other_idx).player.stop();

        self.deck(active_idx).cue.clear();
        if let Ok(mut ctrl) = self.deck(active_idx).transition.write() {
            ctrl.intro_trigger = ctrl.intro_trigger.wrapping_add(1);
            ctrl.intro_secs = 0.0;
            ctrl.outro_trigger = 0;
            ctrl.outro_secs = 0.0;
        }
        if let Ok(mut active) = self.active_id.write() {
            *active = Some(track_id.to_owned());
        }
        self.deck(active_idx)
            .player
            .load(uri, false, at.as_millis() as u32);
        Ok(())
    }

    fn preload(&self, track_id: &str) -> Result<()> {
        let active_idx = *self.active_deck.read().unwrap();
        let is_trans = self.current_transition_config().is_enabled();
        let target_idx = if is_trans { 1 - active_idx } else { active_idx };
        self.deck(target_idx).player.preload(track_uri(track_id)?);
        Ok(())
    }

    fn play(&self) {
        let active = self.active_deck.read().ok().map_or(0, |deck| *deck);
        let deck = self.deck(active);
        if deck.cue.cleared() {
            return;
        }
        deck.player.play();
    }

    fn pause(&self) {
        let active = self.active_deck.read().ok().map_or(0, |deck| *deck);
        self.deck(active).player.pause();
    }

    fn seek(&self, position: Duration) {
        let active_idx = *self.active_deck.read().unwrap();
        let other_idx = 1 - active_idx;
        self.sync.bump_deck(other_idx);
        self.deck(other_idx).player.stop();
        if let Ok(mut ctrl) = self.deck(active_idx).transition.write() {
            ctrl.intro_trigger = ctrl.intro_trigger.wrapping_add(1);
            ctrl.intro_secs = 0.0;
            ctrl.outro_trigger = 0;
            ctrl.outro_secs = 0.0;
        }
        self.deck(active_idx)
            .player
            .seek(position.as_millis() as u32);
        self.deck(active_idx).cue.clear();
    }

    fn set_gain(&self, gain: f32) {
        self.volume.set(gain);
    }

    fn set_transition(&self, transition: TransitionConfig) {
        if let Ok(mut ctrl) = self.deck_a.transition.write() {
            ctrl.config = transition;
        }
        if let Ok(mut ctrl) = self.deck_b.transition.write() {
            ctrl.config = transition;
        }
    }

    fn start_outro(&self, remaining: Duration) -> bool {
        if !self.current_transition_config().is_enabled() {
            return false;
        }
        let active_idx = *self.active_deck.read().unwrap();
        if let Ok(mut ctrl) = self.deck(active_idx).transition.write() {
            ctrl.outro_trigger = ctrl.outro_trigger.wrapping_add(1);
            ctrl.outro_secs = remaining.as_secs_f32().clamp(0.5, 12.0);
        }
        true
    }

    fn spectrum(&self) -> Spectrum {
        self.spectrum.clone()
    }
}

impl MusicPlayer for Engine {
    fn load(&self, track_id: &str, at: Duration, seamless: bool) -> Result<()> {
        self.load(track_id, at, seamless)
    }

    fn load_paused_at(&self, track_id: &str, at: Duration) -> Result<()> {
        self.load_paused_at(track_id, at)
    }

    fn preload(&self, track_id: &str, _segue: bool) -> Result<()> {
        self.preload(track_id)
    }

    fn play(&self) {
        self.play();
    }

    fn pause(&self) {
        self.pause();
    }

    fn seek(&self, position: Duration) {
        self.seek(position);
    }

    fn set_gain(&self, gain: f32) {
        self.set_gain(gain);
    }

    fn set_transition(&self, transition: TransitionConfig) {
        self.set_transition(transition);
    }

    fn start_outro(&self, remaining: Duration) -> bool {
        self.start_outro(remaining)
    }

    fn spectrum(&self) -> Option<Spectrum> {
        Some(self.spectrum())
    }
}

fn track_uri(track_id: &str) -> Result<SpotifyUri> {
    SpotifyUri::from_uri(&format!("{TRACK_PREFIX}{track_id}"))
        .with_context(|| format!("{track_id} is not a track id"))
}

/// librespot's event as ours, or `None` for the ones the state has no use for. A denied audio
/// key is `Refused` and one refused for now is `Throttled`. Any other unavailability names the
/// track.
fn translate(event: PlayerEvent) -> Option<PlaybackEvent> {
    let millis = |position_ms: u32| Duration::from_millis(position_ms as u64);
    let track_id = |uri: SpotifyUri| Some(uri.to_id());

    match event {
        PlayerEvent::Loading {
            track_id: uri,
            position_ms,
            ..
        } => Some(PlaybackEvent::Loading {
            id: track_id(uri),
            at: millis(position_ms),
        }),
        PlayerEvent::Playing {
            track_id: uri,
            position_ms,
            ..
        } => Some(PlaybackEvent::Playing {
            id: track_id(uri),
            at: millis(position_ms),
        }),
        PlayerEvent::Paused {
            track_id: uri,
            position_ms,
            ..
        } => Some(PlaybackEvent::Paused {
            id: track_id(uri),
            at: millis(position_ms),
        }),
        PlayerEvent::PositionChanged {
            track_id: uri,
            position_ms,
            ..
        }
        | PlayerEvent::PositionCorrection {
            track_id: uri,
            position_ms,
            ..
        } => Some(PlaybackEvent::Position {
            id: track_id(uri),
            at: millis(position_ms),
        }),
        PlayerEvent::Seeked {
            track_id: uri,
            position_ms,
            ..
        } => Some(PlaybackEvent::Seeked {
            id: track_id(uri),
            at: millis(position_ms),
        }),
        PlayerEvent::Stopped { track_id: uri, .. }
        | PlayerEvent::EndOfTrack { track_id: uri, .. } => {
            Some(PlaybackEvent::Ended { id: track_id(uri) })
        }
        PlayerEvent::Unavailable { denied: true, .. } => Some(PlaybackEvent::Refused),
        PlayerEvent::Unavailable {
            throttled: true,
            track_id: uri,
            ..
        } => Some(PlaybackEvent::Throttled { id: track_id(uri) }),
        PlayerEvent::Unavailable { track_id: uri, .. } => {
            Some(PlaybackEvent::Unavailable { id: track_id(uri) })
        }
        _ => None,
    }
}
