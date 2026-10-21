// SPDX-License-Identifier: GPL-3.0-or-later
// Portions Copyright (C) Sonora Contributors
// Modifications Copyright (C) 2026 Poly Project Contributors

use crate::metrics::snapped;
use crate::palette::{CoverPalette, of_image};
use crate::skeleton::Skeleton;
use crate::theme::ActiveTheme as _;
use futures::AsyncReadExt as _;
use futures::channel::oneshot;
use gpui::prelude::*;
use gpui::{
    App, Asset, AssetLogger, Context, Div, ElementId, Entity, Global, Hsla, ImageCache,
    ImageCacheError, ImageSource, Interactivity, ObjectFit, Pixels, RenderImage, Resource,
    SharedString, SharedUri, StyleRefinement, Styled, Task, Window, div, img, px, svg,
};
use image::{
    AnimationDecoder, DynamicImage, Frame, ImageDecoder, ImageFormat, RgbaImage,
    codecs::{gif::GifDecoder, webp::WebPDecoder},
    imageops,
};
use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

const FILE_PREFIX: &str = "file://";

const FALLBACK_ICON: &str = "icons/music.svg";
pub(crate) const ROUNDED: Pixels = px(4.);
/// What the cache trims back to at the least. The budget grows to one screenful of the
/// largest window a cover was drawn in, so a HiDPI grid never trims what it shows. The
/// cache is allowed half as much again past its budget while a scroll pulls covers in,
/// since a trim asks every window to redraw and is worth doing in one batch.
const CACHE_BYTES: usize = 64 * 1024 * 1024;
const CACHE_ITEMS: usize = 512;
/// The largest edge a cover is decoded at. It sits above a fullscreen cover on a 5K screen at
/// 2x, so it only ever stops a local original of several thousand pixels from being kept whole.
const MAX_SAMPLE_EDGE: u32 = 4096;
/// The step decode edges are rounded up to. A power of two would decode up to four times
/// the pixels drawn.
const EDGE_STEP: u32 = 64;
/// The edge a cover is decoded at when only its palette is wanted.
const PALETTE_EDGE: u32 = 128;
const GRACE: Duration = Duration::from_secs(5);
const KEEP_ITEMS: usize = 96;
const IDLE: Duration = Duration::from_secs(120);
const ORPHAN: Duration = Duration::from_secs(20);
const SWEEP: Duration = Duration::from_secs(30);
/// How many softened covers are kept. Only the cover waiting on its large art is drawn
/// soft, so two cover the current track and a skip back.
const SOFT_ITEMS: usize = 2;
const SOFT_SIGMA: f32 = 1.6;
const SMALL_BYTES: usize = 64 * 1024;
const BIG_BYTES: usize = 256 * 1024;
const MAX_PENDING: usize = 16;
/// How many covers decode at once. A decode holds the whole source image, so this bounds
/// the transient memory, and running decodes on their own threads keeps those buffers in a
/// couple of malloc arenas rather than one per worker thread.
const DECODERS: usize = 4;
/// How long a condemned cover is held before it is dropped. One redraw of every window
/// is all it takes for anything still on screen to ask for its cover again, and that
/// redraw is already on its way when the batch is condemned.
const REPRIEVE: Duration = Duration::from_millis(250);
/// How many cover palettes are kept. Each one is two colours, so the map costs
/// nothing beside the frames, and holding them past an eviction is what keeps a
/// button its colour while its cover is decoded again.
const TINT_ITEMS: usize = 4096;

type ArtworkKey = (Resource, u32);

type Job = Box<dyn FnOnce() + Send>;

#[derive(Clone, Hash)]
struct ArtworkSource {
    resource: Resource,
    edge: u32,
}

#[derive(Clone)]
enum ArtworkAssetLoader {}

type ArtworkResourceLoader = AssetLogger<ArtworkAssetLoader>;

/// Reads the encoded bytes of a cover. Nothing holds on to them once the decode has
/// landed, because the HTTP disk cache makes a second read cheap.
fn fetch(
    resource: Resource,
    cx: &App,
) -> impl std::future::Future<Output = Result<Vec<u8>, ImageCacheError>> + Send + 'static {
    let client = cx.http_client();
    let asset_source = cx.asset_source().clone();

    async move {
        match resource {
            Resource::Path(path) => Ok(std::fs::read(path.as_ref())?),
            Resource::Uri(uri) => {
                let mut response = client.get(uri.as_ref(), ().into(), true).await?;
                let mut body = Vec::new();
                response.body_mut().read_to_end(&mut body).await?;
                if !response.status().is_success() {
                    let mut body = String::from_utf8_lossy(&body).into_owned();
                    let first_line = body.lines().next().unwrap_or("").trim_end();
                    body.truncate(first_line.len());
                    return Err(ImageCacheError::BadStatus {
                        uri,
                        status: response.status(),
                        body,
                    });
                }
                Ok(body)
            }
            Resource::Embedded(path) => {
                let Some(data) = asset_source.load(&path)? else {
                    return Err(ImageCacheError::Asset(
                        format!("Embedded resource not found: {path}").into(),
                    ));
                };
                Ok(data.into_owned())
            }
        }
    }
}

/// A decoded cover with the palette sampled from it. The loader samples it on its own
/// thread, so a cover landing mid-scroll costs the frame nothing beyond storing it.
#[derive(Clone)]
struct Decoded {
    image: Arc<RenderImage>,
    palette: CoverPalette,
}

impl Asset for ArtworkAssetLoader {
    type Source = ArtworkSource;
    type Output = Result<Decoded, ImageCacheError>;

    fn load(
        source: Self::Source,
        cx: &mut App,
    ) -> impl std::future::Future<Output = Self::Output> + Send + 'static {
        let svg_renderer = cx.svg_renderer();
        let bytes = fetch(source.resource, cx);

        async move {
            let bytes = bytes.await?;

            let Ok(format) = image::guess_format(&bytes) else {
                let image = svg_renderer.render_single_frame(&bytes, 1.0)?;
                let palette = of_image(&image);
                return Ok(Decoded { image, palette });
            };
            on_decoder(move || {
                let image = Arc::new(RenderImage::new(raster_frames(
                    &bytes,
                    format,
                    source.edge,
                )?));
                let palette = of_image(&image);
                Ok(Decoded { image, palette })
            })
            .await
            .unwrap_or_else(|| Err(ImageCacheError::Asset("artwork decoder stopped".into())))
        }
    }
}

/// Runs `decode` on one of the `DECODERS` artwork threads and resolves with its result, or
/// with none when the job died with its thread. A job whose caller stopped waiting is skipped.
fn on_decoder<T: Send + 'static>(
    decode: impl FnOnce() -> T + Send + 'static,
) -> impl std::future::Future<Output = Option<T>> + Send + 'static {
    static QUEUE: OnceLock<mpsc::Sender<Job>> = OnceLock::new();

    let (sender, receiver) = oneshot::channel();
    let job: Job = Box::new(move || {
        if !sender.is_canceled() {
            sender.send(decode()).ok();
        }
    });
    if let Err(mpsc::SendError(job)) = QUEUE.get_or_init(decoders).send(job) {
        job();
    }

    async move { receiver.await.ok() }
}

/// Starts the artwork decode threads and hands back the queue that feeds them. When none
/// could start, the queue refuses every job and `on_decoder` runs it in place.
fn decoders() -> mpsc::Sender<Job> {
    let (sender, receiver) = mpsc::channel::<Job>();
    let receiver = Arc::new(Mutex::new(receiver));
    for index in 0..DECODERS {
        let receiver = receiver.clone();
        let started = std::thread::Builder::new()
            .name(format!("artwork-{index}"))
            .spawn(move || {
                loop {
                    let next = receiver.lock().ok().and_then(|queue| queue.recv().ok());
                    let Some(job) = next else {
                        return;
                    };
                    job();
                }
            });
        if let Err(error) = started {
            log::warn!("artwork: cannot start a decoder: {error}");
        }
    }

    sender
}

fn raster_frames(
    bytes: &[u8],
    format: ImageFormat,
    edge: u32,
) -> Result<Vec<Frame>, ImageCacheError> {
    match format {
        ImageFormat::Gif => animated_frames(GifDecoder::new(Cursor::new(bytes))?, edge),
        ImageFormat::WebP => {
            let mut decoder = WebPDecoder::new(Cursor::new(bytes))?;
            if decoder.has_animation() {
                let _ = decoder.set_background_color(image::Rgba([0, 0, 0, 0]));
                animated_frames(decoder, edge)
            } else {
                static_frame(decoder, edge)
            }
        }
        _ => {
            let decoder =
                image::ImageReader::with_format(Cursor::new(bytes), format).into_decoder()?;
            static_frame(decoder, edge)
        }
    }
}

fn static_frame(mut decoder: impl ImageDecoder, edge: u32) -> Result<Vec<Frame>, ImageCacheError> {
    let orientation = decoder.orientation()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    Ok(vec![Frame::new(artwork_frame(image, edge))])
}

fn animated_frames<'a>(
    decoder: impl AnimationDecoder<'a>,
    edge: u32,
) -> Result<Vec<Frame>, ImageCacheError> {
    let mut frames = Vec::new();
    for frame in decoder.into_frames() {
        match frame {
            Ok(frame) => {
                let delay = frame.delay();
                frames.push(Frame::from_parts(
                    artwork_frame(DynamicImage::ImageRgba8(frame.into_buffer()), edge),
                    0,
                    0,
                    delay,
                ));
            }
            Err(error) => log::debug!("Skipping artwork animation frame: {error}"),
        }
    }
    if frames.is_empty() {
        return Err(ImageCacheError::Asset(
            "Animated artwork contained no decodable frames".into(),
        ));
    }
    Ok(frames)
}

/// Crops a decoded cover to its centre square, scales it down to `edge` and swaps it to
/// BGRA. The scaling happens in the decoder's own pixel format, so an RGB JPEG only grows
/// a fourth channel at the size it is drawn. An image no bigger than `edge`, or any image
/// when `edge` is zero, keeps its size.
fn artwork_frame(image: DynamicImage, edge: u32) -> RgbaImage {
    let (width, height) = (image.width(), image.height());
    let side = width.min(height);
    let image = match edge == 0 || side <= edge {
        true => image,
        false => {
            let square = match width == height {
                true => image,
                false => image.crop_imm((width - side) / 2, (height - side) / 2, side, side),
            };
            square.resize_exact(edge, edge, imageops::FilterType::Lanczos3)
        }
    };
    let mut image = image.into_rgba8();
    bgra(&mut image);
    image
}

fn bgra(image: &mut RgbaImage) {
    for pixel in image.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
}

struct Cached {
    value: Result<Arc<RenderImage>, ImageCacheError>,
    bytes: usize,
    used: Instant,
}

struct ArtworkCache {
    items: HashMap<ArtworkKey, Cached>,
    /// Covers taken out of `items` and kept alive until the windows have redrawn once. See
    /// `condemn`.
    condemned: HashMap<ArtworkKey, Cached>,
    condemned_soft: Vec<Arc<RenderImage>>,
    condemned_at: Option<Instant>,
    pending: HashMap<ArtworkKey, Instant>,
    soft: HashMap<(Resource, u32), Arc<RenderImage>>,
    /// The palette of every cover decoded this run, kept apart from the frames
    /// so an eviction never costs a button its colour.
    tints: HashMap<Resource, CoverPalette>,
    palette_tasks: HashMap<SharedString, Task<CoverPalette>>,
    bytes: usize,
    screen: usize,
    _sweep: Task<()>,
}

struct Installed(Entity<ArtworkCache>);

impl Global for Installed {}

impl ArtworkCache {
    fn entity(cx: &mut App) -> Entity<Self> {
        if cx.try_global::<Installed>().is_none() {
            let cache = cx.new(|cx| Self {
                items: HashMap::new(),
                condemned: HashMap::new(),
                condemned_soft: Vec::new(),
                condemned_at: None,
                pending: HashMap::new(),
                soft: HashMap::new(),
                tints: HashMap::new(),
                palette_tasks: HashMap::new(),
                bytes: 0,
                screen: 0,
                _sweep: sweeper(cx),
            });
            cx.set_global(Installed(cache));
        }
        cx.global::<Installed>().0.clone()
    }

    fn insert(
        &mut self,
        resource: ArtworkKey,
        decoded: Result<Decoded, ImageCacheError>,
        cx: &mut App,
    ) {
        if let Ok(decoded) = &decoded {
            let canonical = canonical_resource(&resource.0);
            if !self.tints.contains_key(&canonical) {
                self.trim_tints();
                self.tints.insert(canonical, decoded.palette);
            }
        }
        let value = decoded.map(|decoded| decoded.image);
        let bytes = value.as_ref().map_or(0, |image| image_bytes(image));
        self.bytes = self.bytes.saturating_add(bytes);
        self.items.insert(
            resource,
            Cached {
                value,
                bytes,
                used: Instant::now(),
            },
        );

        self.trim(cx);
    }

    /// Drops the palettes of covers no longer held, once the map has grown past
    /// its cap. Scrolling through more art than that pays one pass.
    fn trim_tints(&mut self) {
        if self.tints.len() < TINT_ITEMS {
            return;
        }
        let live = &self.items;
        self.tints
            .retain(|resource, _| live.keys().any(|key| &key.0 == resource));
    }

    fn oldest(&self) -> Option<(ArtworkKey, Instant)> {
        self.items
            .iter()
            .min_by_key(|(_, cached)| cached.used)
            .map(|(resource, cached)| (resource.clone(), cached.used))
    }

    /// Takes a cover out of the cache without dropping it. Dropping one frees its tile in
    /// the GPU atlas, and a view whose layout was cached redraws from the primitives it
    /// recorded last frame, so a cover still shown there would be painted with whatever art
    /// took its tile over. A condemned cover keeps its frames until `flush`, and `load_at`
    /// takes it back the moment anything asks for it again.
    fn condemn(&mut self, resource: &ArtworkKey) {
        let Some(cached) = self.items.remove(resource) else {
            return;
        };
        self.bytes = self.bytes.saturating_sub(cached.bytes);
        self.condemned.insert(resource.clone(), cached);
    }

    /// What the cache trims back to: `CACHE_BYTES`, or one screenful of the largest window
    /// when that is more.
    fn budget(&self) -> usize {
        CACHE_BYTES.max(self.screen)
    }

    /// Condemns the least recently drawn covers until the cache is back inside its budget,
    /// then asks every window to redraw. The redraw is what makes the condemned batch safe
    /// to drop: it rebuilds every cached view, so nothing is replayed from last frame's
    /// primitives and everything still on screen asks for its cover again.
    fn trim(&mut self, cx: &mut App) {
        let budget = self.budget();
        if self.bytes <= budget + budget / 2 && self.items.len() <= CACHE_ITEMS {
            return;
        }
        let before = self.condemned.len();
        while self.items.len() > 1 && (self.bytes > budget || self.items.len() > CACHE_ITEMS) {
            let Some((resource, _)) = self.oldest() else {
                break;
            };
            self.condemn(&resource);
        }
        if self.condemned.len() > before {
            self.condemned_at = Some(Instant::now());
            cx.refresh_windows();
        }
    }

    /// Drops every cover still condemned once the redraw that `trim` asked for has been and
    /// gone. Whatever is left here was on no screen through a full rebuild of every view.
    fn flush(&mut self, cx: &mut App) {
        if self.condemned_at.is_none_or(|at| at.elapsed() < REPRIEVE) {
            return;
        }
        self.condemned_at = None;
        for (resource, cached) in std::mem::take(&mut self.condemned) {
            cx.remove_asset::<ArtworkResourceLoader>(&ArtworkSource {
                resource: resource.0.clone(),
                edge: resource.1,
            });
            if let Ok(image) = cached.value {
                cx.drop_image(image, None);
            }
        }
        for image in std::mem::take(&mut self.condemned_soft) {
            cx.drop_image(image, None);
        }
    }

    fn prepared(&self, resource: &Resource, edge: u32, soft: bool) -> Option<Arc<RenderImage>> {
        match soft {
            true => self.soft.get(&(resource.clone(), edge)).cloned(),
            false => None,
        }
    }

    fn prepare(
        &mut self,
        resource: &Resource,
        edge: u32,
        soft: bool,
        image: Arc<RenderImage>,
        cx: &mut App,
    ) -> Arc<RenderImage> {
        if !soft {
            return image;
        }

        let key = (resource.clone(), edge);
        if let Some(found) = self.soft.get(&key) {
            return found.clone();
        }
        if self.soft.len() >= SOFT_ITEMS {
            self.condemned_soft
                .extend(self.soft.drain().map(|(_, image)| image));
            self.condemned_at = Some(Instant::now());
            cx.refresh_windows();
        }
        let Some(softened) = blurred(&image) else {
            return image;
        };
        self.soft.insert(key, softened.clone());
        softened
    }

    fn sweep(&mut self, cx: &mut App) {
        let held = self.items.len();
        let abandoned: Vec<ArtworkKey> = self
            .pending
            .iter()
            .filter(|(_, started)| started.elapsed() > ORPHAN)
            .map(|(resource, _)| resource.clone())
            .collect();

        for resource in &abandoned {
            self.pending.remove(resource);
            cx.remove_asset::<ArtworkResourceLoader>(&ArtworkSource {
                resource: resource.0.clone(),
                edge: resource.1,
            });
        }

        let mut ages: Vec<(ArtworkKey, Instant, usize)> = self
            .items
            .iter()
            .map(|(resource, cached)| (resource.clone(), cached.used, cached.bytes))
            .collect();
        ages.sort_unstable_by_key(|(_, used, _)| *used);

        let idle = ages
            .iter()
            .filter(|(_, used, _)| used.elapsed() > IDLE)
            .count();
        let protected = ages.len().saturating_sub(KEEP_ITEMS);
        let budget = self.budget();
        let mut bytes = self.bytes;
        let mut stale = Vec::new();

        for (index, (resource, used, size)) in ages.iter().enumerate() {
            if index >= protected || used.elapsed() <= GRACE {
                break;
            }
            if bytes <= budget && used.elapsed() <= IDLE {
                break;
            }
            stale.push(resource.clone());
            bytes = bytes.saturating_sub(*size);
        }

        for resource in &stale {
            self.condemn(resource);
        }
        if !stale.is_empty() {
            self.condemned_at = Some(Instant::now());
            cx.refresh_windows();
        }

        self.palette_tasks.retain(|_, task| !task.is_finished());

        let tiny = self.count(..SMALL_BYTES);
        let small = self.count(SMALL_BYTES..BIG_BYTES);
        let big = self.count(BIG_BYTES..);

        log::debug!(
            "artwork: {} held / {} KiB, dropped {}, idle {idle}, abandoned {}, waiting {}, sizes {tiny}/{small}/{big}",
            self.items.len(),
            self.bytes / 1024,
            held - self.items.len(),
            abandoned.len(),
            self.pending.len()
        );
    }

    fn count(&self, range: impl std::ops::RangeBounds<usize>) -> usize {
        self.items
            .values()
            .filter(|cached| range.contains(&cached.bytes))
            .count()
    }
}

/// Releases condemned covers as their reprieve runs out and sweeps the cache on its own
/// slower clock. The two share one timer because a tick that finds nothing to release costs
/// a look at an empty map.
fn sweeper(cx: &mut Context<ArtworkCache>) -> Task<()> {
    cx.spawn(async move |this, cx| {
        let mut swept = Instant::now();
        loop {
            cx.background_executor().timer(REPRIEVE).await;
            let due = swept.elapsed() >= SWEEP;
            let alive = this.update(cx, |this, cx| {
                this.flush(cx);
                if due {
                    this.sweep(cx);
                }
            });
            if alive.is_err() {
                return;
            }
            if due {
                swept = Instant::now();
            }
        }
    })
}

impl ImageCache for ArtworkCache {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        self.load_at(resource, 0, window, cx)
    }
}

impl ArtworkCache {
    fn reap_pending(&mut self, window: &mut Window, cx: &mut App) {
        let pending: Vec<ArtworkKey> = self.pending.keys().cloned().collect();
        for key in pending {
            let source = ArtworkSource {
                resource: key.0.clone(),
                edge: key.1,
            };
            let Some(value) = window.use_asset::<ArtworkResourceLoader>(&source, cx) else {
                continue;
            };
            self.pending.remove(&key);
            self.insert(key, value, cx);
        }
    }

    fn load_at(
        &mut self,
        resource: &Resource,
        edge: u32,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let key = (resource.clone(), edge);
        if let Some(cached) = self.items.get_mut(&key) {
            cached.used = Instant::now();
            return Some(cached.value.clone());
        }
        // Still on screen after all, so it goes back in the cache rather than being dropped.
        if let Some(mut cached) = self.condemned.remove(&key) {
            cached.used = Instant::now();
            let value = cached.value.clone();
            self.bytes = self.bytes.saturating_add(cached.bytes);
            self.items.insert(key, cached);
            return Some(value);
        }
        let viewport = window.viewport_size();
        let scale = window.scale_factor();
        let screen =
            (viewport.width.as_f32() * viewport.height.as_f32() * scale * scale) as usize * 4;
        self.screen = self.screen.max(screen);

        if !self.pending.contains_key(&key) && self.pending.len() >= MAX_PENDING {
            self.reap_pending(window, cx);
            if self.pending.len() >= MAX_PENDING {
                return None;
            }
        }

        let source = ArtworkSource {
            resource: resource.clone(),
            edge,
        };
        let Some(value) = window.use_asset::<ArtworkResourceLoader>(&source, cx) else {
            self.pending.insert(key, Instant::now());
            return None;
        };

        let image = value.clone().map(|decoded| decoded.image);
        self.pending.remove(&key);
        self.insert(key, value, cx);
        Some(image)
    }

    /// The largest decode the cache holds of the same cover at no less than half of `edge`, to
    /// draw while the one at `edge` loads. A resize that moves a cover to the next edge step
    /// keeps it on screen instead of flashing the skeleton.
    fn stand_in(&mut self, resource: &Resource, edge: u32) -> Option<Arc<RenderImage>> {
        let canonical = canonical_resource(resource);
        let (_, cached) = self
            .items
            .iter_mut()
            .filter(|((held, size), cached)| {
                (held == resource || canonical_resource(held) == canonical)
                    && *size > 0
                    && cached.value.is_ok()
            })
            .max_by_key(|((_, size), _)| *size)?;
        cached.used = Instant::now();
        cached.value.clone().ok()
    }
}

fn blurred(image: &RenderImage) -> Option<Arc<RenderImage>> {
    let frames: Vec<Frame> = (0..image.frame_count())
        .filter_map(|index| {
            let size = image.size(index);
            let width = size.width.0.max(0) as u32;
            let height = size.height.0.max(0) as u32;
            let bytes = image.as_bytes(index)?.to_vec();
            let whole = RgbaImage::from_raw(width, height, bytes)?;

            Some(Frame::from_parts(
                imageops::fast_blur(&whole, SOFT_SIGMA),
                0,
                0,
                image.delay(index),
            ))
        })
        .collect();
    if frames.len() != image.frame_count() {
        log::warn!("artwork: cannot soften an image");
        return None;
    }

    Some(Arc::new(RenderImage::new(frames)))
}

/// The edge a cover drawn at `size` is decoded at: its physical size rounded up to the next
/// `EDGE_STEP`, so nearby sizes share one decode, and never past `MAX_SAMPLE_EDGE`.
fn sample_edge(size: Pixels, window: &Window) -> u32 {
    let scale = window.scale_factor().max(1.0);
    let physical = ((size / px(1.)) * scale).ceil().max(1.) as u32;
    physical.next_multiple_of(EDGE_STEP).min(MAX_SAMPLE_EDGE)
}

fn optimize_artwork_url(url: &str, edge: u32) -> String {
    if url.contains("i.scdn.co/image/ab67") {
        if edge > 0 && edge <= 64 {
            url.replace("0000b273", "00004851").replace("00001e02", "00004851")
        } else if edge > 0 && edge <= 300 {
            url.replace("0000b273", "00001e02").replace("00004851", "00001e02")
        } else {
            url.replace("00004851", "0000b273").replace("00001e02", "0000b273")
        }
    } else if url.contains("mzstatic.com") {
        let dim = match edge {
            0 => 600,
            e if e <= 100 => 100,
            e if e <= 300 => 300,
            e if e <= 600 => 600,
            _ => 1000,
        };
        let target = format!("{dim}x{dim}");
        if url.contains("{w}x{h}") {
            url.replace("{w}x{h}", &target)
        } else if url.contains("/100x100") && dim != 100 {
            url.replace("/100x100", &format!("/{target}"))
        } else if url.contains("/600x600") && dim != 600 {
            url.replace("/600x600", &format!("/{target}"))
        } else {
            url.to_owned()
        }
    } else {
        url.to_owned()
    }
}

fn canonical_resource(resource: &Resource) -> Resource {
    match resource {
        Resource::Uri(uri) => {
            let s = uri.as_ref();
            if s.contains("i.scdn.co/image/ab67") {
                Resource::Uri(SharedUri::from(
                    s.replace("00004851", "0000b273").replace("00001e02", "0000b273"),
                ))
            } else if s.contains("mzstatic.com") {
                Resource::Uri(SharedUri::from(
                    s.replace("/100x100", "/600x600")
                        .replace("/300x300", "/600x600")
                        .replace("/1000x1000", "/600x600"),
                ))
            } else {
                resource.clone()
            }
        }
        _ => resource.clone(),
    }
}

pub(crate) fn resource(url: impl Into<SharedString>) -> Resource {
    resource_at(url, 0)
}

pub(crate) fn resource_at(url: impl Into<SharedString>, edge: u32) -> Resource {
    let url = url.into();
    let optimized = optimize_artwork_url(url.as_ref(), edge);
    match optimized.strip_prefix(FILE_PREFIX) {
        Some(path) => Resource::Path(Arc::from(Path::new(path))),
        None => Resource::Uri(SharedUri::from(optimized)),
    }
}

pub fn cover_palette(url: &str, cx: &App) -> Option<CoverPalette> {
    let installed = cx.try_global::<Installed>()?;
    let cache = installed.0.read(cx);
    let resource = resource(url.to_owned());
    if let Some(pal) = cache.tints.get(&resource).copied() {
        return Some(pal);
    }
    let canonical = canonical_resource(&resource);
    cache.tints.get(&canonical).copied()
}

pub(crate) fn sample_palette(url: SharedString, cx: &mut App) -> Task<CoverPalette> {
    if let Some(palette) = cover_palette(&url, cx) {
        return Task::ready(palette);
    }
    let cache = ArtworkCache::entity(cx);
    if let Some(task) = cache.read(cx).palette_tasks.get(&url) {
        return task.clone();
    }
    let res = resource_at(url.clone(), PALETTE_EDGE);
    let load = ArtworkAssetLoader::load(
        ArtworkSource {
            resource: res,
            edge: PALETTE_EDGE,
        },
        cx,
    );
    let url_key = url.clone();
    let task = cx.background_spawn(async move {
        load.await
            .map(|decoded| decoded.palette)
            .unwrap_or_default()
    });
    cache.update(cx, |cache, _| {
        cache.palette_tasks.insert(url_key, task.clone());
    });
    task
}

pub fn artwork_usage(cx: &App) -> Option<(usize, usize)> {
    let installed = cx.try_global::<Installed>()?;
    let cache = installed.0.read(cx);
    let soft_bytes: usize = cache.soft.values().map(|image| image_bytes(image)).sum();
    Some((
        cache.items.len() + cache.soft.len(),
        cache.bytes + soft_bytes,
    ))
}

fn image_bytes(image: &RenderImage) -> usize {
    (0..image.frame_count())
        .filter_map(|frame| image.as_bytes(frame))
        .fold(0, |bytes, frame| bytes.saturating_add(frame.len()))
}

#[derive(IntoElement)]
pub struct Avatar {
    art: Artwork,
}

impl Avatar {
    #[track_caller]
    pub fn new(url: Option<impl Into<SharedString>>) -> Self {
        Self {
            art: Artwork::new(url).circle().flex_none(),
        }
    }

    pub fn size(mut self, size: Pixels) -> Self {
        self.art = self.art.size(size);
        self
    }
}

impl Styled for Avatar {
    fn style(&mut self) -> &mut StyleRefinement {
        self.art.style()
    }
}

impl RenderOnce for Avatar {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        self.art
    }
}

#[derive(IntoElement)]
pub struct Artwork {
    url: Option<SharedString>,
    size: Pixels,
    circle: bool,
    radius: Option<Pixels>,
    fallback: SharedString,
    accent: bool,
    soft: bool,
    interactivity: Interactivity,
}

impl Artwork {
    #[track_caller]
    pub fn new(url: Option<impl Into<SharedString>>) -> Self {
        Self {
            url: url.map(Into::into),
            size: px(28.),
            circle: false,
            radius: None,
            soft: false,
            fallback: FALLBACK_ICON.into(),
            accent: false,
            interactivity: Interactivity::new(),
        }
    }

    pub fn size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }

    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.interactivity.element_id = Some(id.into());
        self
    }

    pub fn circle(mut self) -> Self {
        self.circle = true;
        self
    }

    pub fn corner_radius(mut self, radius: Pixels) -> Self {
        self.radius = Some(radius);
        self
    }

    pub fn fallback(mut self, icon: impl Into<SharedString>) -> Self {
        self.fallback = icon.into();
        self
    }

    pub fn soft(mut self, soft: bool) -> Self {
        self.soft = soft;
        self
    }

    pub fn accent(mut self) -> Self {
        self.accent = true;
        self
    }
}

impl Styled for Artwork {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.interactivity.base_style
    }
}

impl InteractiveElement for Artwork {
    fn interactivity(&mut self) -> &mut Interactivity {
        &mut self.interactivity
    }
}

impl RenderOnce for Artwork {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            url,
            size,
            circle,
            radius,
            fallback,
            accent,
            soft,
            interactivity,
        } = self;
        let theme = *cx.theme();
        let muted = theme.muted_foreground;
        let glyph = match accent {
            true => theme.tint.unwrap_or(theme.primary),
            false => muted.opacity(0.5),
        };
        let size = snapped(size, window);
        let rounded = match (circle, radius) {
            (true, _) => size / 2.,
            (false, Some(radius)) => radius,
            (false, None) => cx.theme().radius.min(ROUNDED),
        };
        let placeholder = {
            let fallback = fallback.clone();
            move || blank(size, rounded, muted, glyph, fallback.clone()).into_any_element()
        };

        match url {
            Some(url) => {
                let cache = ArtworkCache::entity(cx);
                let edge = sample_edge(size, window);
                let resource = resource_at(url, edge);
                let source = ImageSource::Custom(Arc::new({
                    let cache = cache.clone();
                    move |window, cx| {
                        if let Some(prepared) =
                            cache.update(cx, |cache, _| cache.prepared(&resource, edge, soft))
                        {
                            return Some(Ok(prepared));
                        }
                        let Some(loaded) = cache
                            .update(cx, |cache, cx| cache.load_at(&resource, edge, window, cx))
                        else {
                            return match soft {
                                true => None,
                                false => cache
                                    .update(cx, |cache, _| cache.stand_in(&resource, edge))
                                    .map(Ok),
                            };
                        };
                        let loaded = loaded.map(|image| {
                            cache.update(cx, |cache, cx| {
                                cache.prepare(&resource, edge, soft, image, cx)
                            })
                        });
                        Some(loaded)
                    }
                }));
                refined(
                    img(source)
                        .image_cache(&cache)
                        .size(size)
                        .object_fit(ObjectFit::Cover)
                        .rounded(rounded)
                        .with_loading(move || {
                            Skeleton::new()
                                .size(size)
                                .rounded(rounded)
                                .into_any_element()
                        })
                        .with_fallback(placeholder),
                    interactivity,
                )
                .into_any_element()
            }
            None => refined(blank(size, rounded, muted, glyph, fallback), interactivity)
                .into_any_element(),
        }
    }
}

fn refined<T: Styled + InteractiveElement>(mut element: T, mut caller: Interactivity) -> T {
    let mut style = std::mem::take(element.style());
    style.refine(&caller.base_style);
    *caller.base_style = style;
    *element.interactivity() = caller;
    element
}

fn blank(size: Pixels, rounded: Pixels, muted: Hsla, glyph: Hsla, fallback: SharedString) -> Div {
    div()
        .size(size)
        .rounded(rounded)
        .bg(muted.opacity(0.12))
        .flex()
        .items_center()
        .justify_center()
        .child(
            svg()
                .path(icons::path(fallback))
                .size(size * 0.46)
                .text_color(glyph),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Delay, Rgba, codecs::gif::GifEncoder};

    #[test]
    fn artwork_loader_targets_the_requested_edge() {
        let image = RgbaImage::from_pixel(240, 120, Rgba([20, 40, 60, 255]));
        let frame = artwork_frame(DynamicImage::ImageRgba8(image), 64);

        assert_eq!(frame.width(), 64);
        assert_eq!(frame.height(), 64);
    }

    #[test]
    fn artwork_loader_preserves_animated_frames() {
        let mut bytes = Vec::new();
        GifEncoder::new(&mut bytes)
            .encode_frames([
                Frame::from_parts(
                    RgbaImage::from_pixel(120, 120, Rgba([20, 40, 60, 255])),
                    0,
                    0,
                    Delay::from_numer_denom_ms(80, 1),
                ),
                Frame::from_parts(
                    RgbaImage::from_pixel(120, 120, Rgba([80, 100, 120, 255])),
                    0,
                    0,
                    Delay::from_numer_denom_ms(120, 1),
                ),
            ])
            .unwrap();

        let frames = raster_frames(&bytes, ImageFormat::Gif, 64).unwrap();

        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].buffer().dimensions(), (64, 64));
        assert_eq!(frames[0].delay(), Delay::from_numer_denom_ms(80, 1));
        assert_eq!(frames[1].delay(), Delay::from_numer_denom_ms(120, 1));
    }
}
