// SPDX-License-Identifier: MPL-2.0

use crate::{CosmicBg, CosmicBgLayer};

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use cosmic_bg_config::state::State;
use cosmic_bg_config::{Color, Entry, SamplingMethod, ScalingMode, Source};
use cosmic_config::CosmicConfigEntry;
use image::{DynamicImage, ImageDecoder, ImageReader, ImageResult, Limits};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use rand::rng;
use rand::seq::SliceRandom;
use sctk::reexports::calloop::timer::{TimeoutAction, Timer};
use sctk::reexports::calloop::{self, RegistrationToken};
use sctk::reexports::client::QueueHandle;
use tracing::error;
use walkdir::WalkDir;

#[derive(Debug, Clone)]
struct CachedImage {
    image: DynamicImage,
    mtime: SystemTime,
    byte_size: usize,
    last_used: u64,
}

/// A shared, aggregate-budget LRU cache for decoded originals.
#[derive(Debug)]
pub struct ImageCache {
    entries: HashMap<PathBuf, CachedImage>,
    budget_bytes: usize,
    current_bytes: usize,
    counter: u64,
}

impl Default for ImageCache {
    fn default() -> Self {
        // Default 128 MiB aggregate cache budget
        Self::new(128 * 1024 * 1024)
    }
}

impl ImageCache {
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            budget_bytes,
            current_bytes: 0,
            counter: 0,
        }
    }

    pub fn get(&mut self, path: &Path, mtime: SystemTime) -> Option<&DynamicImage> {
        self.counter += 1;
        let counter = self.counter;
        if let Some(entry) = self.entries.get_mut(path)
            && entry.mtime == mtime
        {
            entry.last_used = counter;
            return Some(&entry.image);
        }
        None
    }

    pub fn insert(&mut self, path: PathBuf, mtime: SystemTime, image: DynamicImage) {
        self.counter += 1;
        let byte_size = image.as_bytes().len();

        if let Some(old) = self.entries.remove(&path) {
            self.current_bytes = self.current_bytes.saturating_sub(old.byte_size);
        }

        while self.current_bytes + byte_size > self.budget_bytes && !self.entries.is_empty() {
            let oldest_key = self
                .entries
                .iter()
                .min_by_key(|(_, v)| v.last_used)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest_key {
                if let Some(removed) = self.entries.remove(&k) {
                    self.current_bytes = self.current_bytes.saturating_sub(removed.byte_size);
                }
            } else {
                break;
            }
        }

        self.current_bytes += byte_size;
        self.entries.insert(
            path,
            CachedImage {
                image,
                mtime,
                byte_size,
                last_used: self.counter,
            },
        );
    }

    pub fn invalidate(&mut self, path: &Path) {
        if let Some(removed) = self.entries.remove(path) {
            self.current_bytes = self.current_bytes.saturating_sub(removed.byte_size);
        }
    }
}

/// Cheaply filters obvious non-image candidates by extension and hidden file status.
#[must_use]
pub fn is_image_candidate(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
        return false;
    };
    if let Some(file_name) = path.file_name().and_then(|s| s.to_str())
        && file_name.starts_with('.')
    {
        return false;
    }
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "jfif"
            | "webp"
            | "jxl"
            | "avif"
            | "hdr"
            | "bmp"
            | "gif"
            | "tiff"
            | "tif"
            | "qoi"
    )
}

/// Recursively scans directory or checks single file for valid image candidates.
pub fn scan_image_candidates(source: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let Ok(source) = source.canonicalize() else {
        return candidates;
    };

    if source.is_dir() {
        for entry in WalkDir::new(&source)
            .follow_links(true)
            .into_iter()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if path.is_file() && is_image_candidate(path) {
                candidates.push(path.to_path_buf());
            }
        }
    } else if source.is_file() && is_image_candidate(&source) {
        candidates.push(source);
    }

    candidates
}

#[derive(Debug)]
pub struct Wallpaper {
    pub entry: Entry,
    pub layers: Vec<CosmicBgLayer>,
    pub image_queue: VecDeque<PathBuf>,
    loop_handle: calloop::LoopHandle<'static, CosmicBg>,
    queue_handle: QueueHandle<CosmicBg>,
    pub current_source: Option<Source>,
    last_rendered_image: Option<DynamicImage>,
    timer_token: Option<RegistrationToken>,
    pub _watcher: Option<RecommendedWatcher>,
}

impl Drop for Wallpaper {
    fn drop(&mut self) {
        if let Some(token) = self.timer_token.take() {
            self.loop_handle.remove(token);
        }
        self._watcher = None;
    }
}

impl Wallpaper {
    pub fn new(
        entry: Entry,
        queue_handle: QueueHandle<CosmicBg>,
        loop_handle: calloop::LoopHandle<'static, CosmicBg>,
        source_tx: calloop::channel::SyncSender<(String, notify::Event)>,
    ) -> Self {
        let mut wallpaper = Wallpaper {
            entry,
            layers: Vec::new(),
            current_source: None,
            last_rendered_image: None,
            image_queue: VecDeque::default(),
            timer_token: None,
            _watcher: None,
            loop_handle,
            queue_handle,
        };

        wallpaper.load_images();
        wallpaper.watch_source(source_tx);
        wallpaper
    }

    pub fn save_state(&self) -> Result<(), cosmic_config::Error> {
        let Some(cur_source) = self.current_source.clone() else {
            return Ok(());
        };
        let state_helper = State::state()?;
        let mut state = State::get_entry(&state_helper).unwrap_or_default();
        for l in &self.layers {
            let name = l.output_info.name.clone().unwrap_or_default();
            if let Some((_, source)) = state
                .wallpapers
                .iter_mut()
                .find(|(output, _)| *output == name)
            {
                *source = cur_source.clone();
            } else {
                state.wallpapers.push((name, cur_source.clone()))
            }
        }
        state.write_entry(&state_helper)
    }

    #[allow(clippy::too_many_lines)]
    pub fn draw(&mut self, image_cache: &mut ImageCache) {
        let start = Instant::now();
        let mut cur_resized_img: Option<DynamicImage> = None;

        for layer in self.layers.iter_mut().filter(|layer| layer.needs_redraw) {
            let Some(pool) = layer.pool.as_mut() else {
                continue;
            };

            let Some(fractional_scale) = layer.fractional_scale else {
                continue;
            };

            let Some((logical_width, logical_height)) = layer.size else {
                continue;
            };

            let width = logical_width * fractional_scale / 120;
            let height = logical_height * fractional_scale / 120;

            if cur_resized_img
                .as_ref()
                .is_none_or(|img| img.width() != width || img.height() != height)
            {
                let Some(source) = self.current_source.as_ref() else {
                    tracing::info!("No source for wallpaper");
                    continue;
                };

                cur_resized_img = match source {
                    Source::Path(_) => {
                        let mut decoded_img = None;
                        let max_attempts = 10.min(self.image_queue.len()).max(1);
                        let mut attempts = 0;

                        while attempts < max_attempts {
                            attempts += 1;
                            let path = match self.current_source.as_ref() {
                                Some(Source::Path(p)) => p.clone(),
                                _ => break,
                            };

                            let mtime = fs::metadata(&path)
                                .and_then(|m| m.modified())
                                .unwrap_or(std::time::UNIX_EPOCH);

                            if let Some(img) = image_cache.get(&path, mtime) {
                                decoded_img = Some(img.clone());
                                break;
                            }

                            match ImageReader::open(&path)
                                .ok()
                                .and_then(|f| f.with_guessed_format().ok())
                                .map(decode)
                            {
                                Some(Ok(img)) => {
                                    image_cache.insert(path, mtime, img.clone());
                                    decoded_img = Some(img);
                                    break;
                                }
                                Some(Err(why)) => {
                                    tracing::warn!(
                                        ?why,
                                        "Failed to decode image: {}",
                                        path.display()
                                    );
                                }
                                None => {
                                    tracing::warn!("Failed to open image file: {}", path.display());
                                }
                            }

                            // If decode failed, advance to next candidate in queue
                            if self.image_queue.len() > 1
                                && let Some(next) = self.image_queue.pop_front()
                            {
                                self.current_source = Some(Source::Path(next.clone()));
                                self.image_queue.push_back(next);
                            }
                        }

                        let img = match decoded_img {
                            Some(img) => {
                                self.last_rendered_image = Some(img.clone());
                                img
                            }
                            None => {
                                if let Some(ref last) = self.last_rendered_image {
                                    tracing::warn!(
                                        "Using last successfully rendered image as fallback"
                                    );
                                    last.clone()
                                } else {
                                    continue;
                                }
                            }
                        };

                        match self.entry.scaling_mode {
                            ScalingMode::Fit(color) => Some(crate::scaler::fit(
                                &img,
                                &color,
                                width,
                                height,
                                &self.entry.filter_method,
                            )),

                            ScalingMode::Zoom => Some(crate::scaler::zoom(
                                &img,
                                width,
                                height,
                                &self.entry.filter_method,
                            )),

                            ScalingMode::Stretch => Some(crate::scaler::stretch(
                                &img,
                                width,
                                height,
                                &self.entry.filter_method,
                            )),
                        }
                    }

                    Source::Color(Color::Single([r, g, b])) => {
                        Some(DynamicImage::ImageRgb8(crate::colored::single_1x1([
                            *r, *g, *b,
                        ])))
                    }

                    Source::Color(Color::Gradient(gradient)) => {
                        match crate::colored::gradient(gradient, width, height) {
                            Ok(buffer) => Some(DynamicImage::ImageRgb8(buffer)),
                            Err(why) => {
                                tracing::error!(
                                    ?gradient,
                                    ?why,
                                    "color gradient in config is invalid"
                                );
                                None
                            }
                        }
                    }
                };
            }

            let Some(image) = cur_resized_img.as_ref() else {
                continue;
            };

            let is_solid_single =
                matches!(self.current_source, Some(Source::Color(Color::Single(_))));
            let (buf_width, buf_height) = if is_solid_single {
                (1, 1)
            } else {
                (width, height)
            };

            let required_bytes = buf_width as usize * buf_height as usize * 4;
            if pool.len() < required_bytes
                && let Err(why) = pool.resize(required_bytes)
            {
                tracing::error!(?why, "failed to resize pool in draw");
                continue;
            }

            let buffer_result = crate::draw::canvas(
                pool,
                image,
                buf_width as i32,
                buf_height as i32,
                buf_width as i32 * 4,
            );

            match buffer_result {
                Ok(buffer) => {
                    crate::draw::layer_surface(
                        layer,
                        &self.queue_handle,
                        &buffer,
                        (buf_width as i32, buf_height as i32),
                    );
                    layer.needs_redraw = false;

                    let elapsed = Instant::now().duration_since(start);

                    tracing::debug!(?elapsed, source = ?self.entry.source, "wallpaper draw");
                }

                Err(why) => {
                    tracing::error!(?why, "wallpaper could not be drawn");
                }
            }
        }
    }

    pub fn load_images(&mut self) {
        let mut image_queue = VecDeque::new();

        match self.entry.source {
            Source::Path(ref source) => {
                tracing::debug!(?source, "loading images");
                let candidates = scan_image_candidates(source);
                for p in candidates {
                    image_queue.push_back(p);
                }

                if image_queue.len() > 1 {
                    let image_slice = image_queue.make_contiguous();
                    match self.entry.sampling_method {
                        SamplingMethod::Alphanumeric => {
                            image_slice
                                .sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));
                        }
                        SamplingMethod::Random => image_slice.shuffle(&mut rng()),
                    };

                    // If a wallpaper from this slideshow was previously set, resume with that wallpaper.
                    if let Some(Source::Path(last_path)) = current_image(&self.entry.output)
                        && image_queue.contains(&last_path)
                    {
                        while let Some(path) = image_queue.pop_front() {
                            if path == last_path {
                                image_queue.push_front(path);
                                break;
                            }

                            image_queue.push_back(path);
                        }
                    }
                }

                if let Some(current_image_path) = image_queue.pop_front() {
                    self.current_source = Some(Source::Path(current_image_path.clone()));
                    image_queue.push_back(current_image_path);
                }
            }

            Source::Color(ref c) => {
                self.current_source = Some(Source::Color(c.clone()));
            }
        };

        if let Err(err) = self.save_state() {
            error!("{err}");
        }
        self.image_queue = image_queue;
        self.reconcile_timer();
    }

    pub fn watch_source(&mut self, tx: calloop::channel::SyncSender<(String, notify::Event)>) {
        let Source::Path(ref source) = self.entry.source else {
            self._watcher = None;
            return;
        };

        let output = self.entry.output.clone();
        let mut watcher = match RecommendedWatcher::new(
            move |res| {
                if let Ok(e) = res {
                    if let Err(why) = tx.send((output.clone(), e)) {
                        tracing::error!(?why, "failed to send fs event to channel");
                    }
                } else if let Err(why) = res {
                    tracing::error!(?why, "fs watcher error");
                }
            },
            notify::Config::default(),
        ) {
            Ok(w) => w,
            Err(why) => {
                tracing::error!(?why, "failed to create RecommendedWatcher");
                return;
            }
        };

        tracing::debug!(output = self.entry.output, ?source, "watching source");

        if let Ok(m) = fs::metadata(source) {
            let res = if m.is_dir() {
                watcher.watch(source, RecursiveMode::Recursive)
            } else {
                watcher.watch(source, RecursiveMode::NonRecursive)
            };
            if let Err(why) = res {
                tracing::error!(?why, ?source, "failed to watch source path");
            }
        } else {
            tracing::warn!(?source, "source path does not exist or cannot be read");
        }

        self._watcher = Some(watcher);
    }

    pub fn reconcile_timer(&mut self) {
        let rotation_freq = self.entry.rotation_frequency;
        let should_run = rotation_freq > 0 && self.image_queue.len() > 1;

        if should_run {
            if self.timer_token.is_none() {
                self.register_timer();
            }
        } else if let Some(token) = self.timer_token.take() {
            self.loop_handle.remove(token);
        }
    }

    fn register_timer(&mut self) {
        let rotation_freq = self.entry.rotation_frequency;
        let cosmic_bg_clone = self.entry.output.clone();
        if rotation_freq == 0 || self.image_queue.len() <= 1 {
            return;
        }

        self.timer_token = self
            .loop_handle
            .insert_source(
                Timer::from_duration(Duration::from_secs(rotation_freq)),
                move |_, _, state: &mut CosmicBg| {
                    let span = tracing::debug_span!("Wallpaper::timer");
                    let _handle = span.enter();

                    let Some(item) = state
                        .wallpapers
                        .iter_mut()
                        .find(|w| w.entry.output == cosmic_bg_clone)
                    else {
                        return TimeoutAction::Drop;
                    };

                    if item.image_queue.len() <= 1 {
                        item.timer_token = None;
                        return TimeoutAction::Drop;
                    }

                    if let Some(next) = item.image_queue.pop_front() {
                        let next_source = Source::Path(next.clone());
                        if item.current_source.as_ref() == Some(&next_source) {
                            item.image_queue.push_back(next);
                            return TimeoutAction::ToDuration(Duration::from_secs(
                                item.entry.rotation_frequency,
                            ));
                        }

                        item.current_source = Some(next_source);
                        if let Err(err) = item.save_state() {
                            error!("{err}");
                        }

                        item.image_queue.push_back(next);
                        item.clear_image();
                        item.draw(&mut state.image_cache);

                        return TimeoutAction::ToDuration(Duration::from_secs(
                            item.entry.rotation_frequency,
                        ));
                    }

                    item.timer_token = None;
                    TimeoutAction::Drop
                },
            )
            .ok();
    }

    pub fn mark_needs_redraw(&mut self) {
        for l in &mut self.layers {
            l.needs_redraw = true;
        }
    }

    pub fn clear_image(&mut self) {
        self.last_rendered_image = None;
        self.mark_needs_redraw();
    }
}

fn decode(mut reader: ImageReader<BufReader<fs::File>>) -> ImageResult<DynamicImage> {
    let mut limits = Limits::default();
    limits.max_alloc = Some(1024 * 1024 * 1024);
    reader.limits(limits.clone());

    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation()?;

    limits.reserve(decoder.total_bytes())?;
    decoder.set_limits(limits)?;

    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);

    Ok(image)
}

fn current_image(output: &str) -> Option<Source> {
    let state = State::state().ok()?;
    let mut wallpapers = State::get_entry(&state)
        .unwrap_or_default()
        .wallpapers
        .into_iter();

    let wallpaper = if output == "all" {
        wallpapers.next()
    } else {
        wallpapers.into_iter().find(|(name, _path)| name == output)
    };

    wallpaper.map(|(_name, path)| path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};
    use std::fs::File;
    use tempfile::tempdir;

    #[test]
    fn test_scan_image_candidates_filters_non_images_and_hidden() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let subdir = root.join("subdir");
        fs::create_dir(&subdir).unwrap();

        File::create(root.join("img1.png")).unwrap();
        File::create(subdir.join("img2.jpg")).unwrap();
        File::create(root.join("README.md")).unwrap();
        File::create(root.join(".hidden.png")).unwrap();
        File::create(subdir.join("notes.txt")).unwrap();

        let candidates = scan_image_candidates(root);

        assert_eq!(candidates.len(), 2, "Should find only the 2 valid images");
        assert!(candidates.iter().any(|p| p.ends_with("img1.png")));
        assert!(candidates.iter().any(|p| p.ends_with("img2.jpg")));
        assert!(!candidates.iter().any(|p| p.ends_with("README.md")));
        assert!(!candidates.iter().any(|p| p.ends_with(".hidden.png")));
    }

    #[test]
    fn test_is_image_candidate_extensions() {
        assert!(is_image_candidate(Path::new("test.png")));
        assert!(is_image_candidate(Path::new("test.JPG")));
        assert!(is_image_candidate(Path::new("test.webp")));
        assert!(is_image_candidate(Path::new("test.avif")));
        assert!(is_image_candidate(Path::new("test.jxl")));

        assert!(!is_image_candidate(Path::new("test.txt")));
        assert!(!is_image_candidate(Path::new("test.md")));
        assert!(!is_image_candidate(Path::new("README")));
        assert!(!is_image_candidate(Path::new(".hidden.png")));
    }

    #[test]
    fn test_image_cache_lru_and_budget() {
        // Budget for roughly 2 small images (each 10x10 RGB8 = 300 bytes)
        let mut cache = ImageCache::new(650);

        let img1 = DynamicImage::ImageRgb8(RgbImage::from_pixel(10, 10, Rgb([1, 1, 1])));
        let img2 = DynamicImage::ImageRgb8(RgbImage::from_pixel(10, 10, Rgb([2, 2, 2])));
        let img3 = DynamicImage::ImageRgb8(RgbImage::from_pixel(10, 10, Rgb([3, 3, 3])));

        let p1 = PathBuf::from("/tmp/img1.png");
        let p2 = PathBuf::from("/tmp/img2.png");
        let p3 = PathBuf::from("/tmp/img3.png");
        let now = SystemTime::now();

        cache.insert(p1.clone(), now, img1);
        assert!(cache.get(&p1, now).is_some());

        cache.insert(p2.clone(), now, img2);
        assert!(cache.get(&p2, now).is_some());

        // Access p1 to make p2 the LRU
        assert!(cache.get(&p1, now).is_some());

        // Inserting p3 should evict p2 because p1 was accessed more recently
        cache.insert(p3.clone(), now, img3);
        assert!(cache.get(&p3, now).is_some());
        assert!(cache.get(&p1, now).is_some());
        assert!(cache.get(&p2, now).is_none(), "p2 should have been evicted");
    }
}
