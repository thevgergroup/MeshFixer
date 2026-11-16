//! Thumbnails for the Parts library grid: each part rendered offscreen in
//! Clay at 128 px (`mf_render::offscreen::render_image`, the same
//! renderer as the view), cached by the part's content hash — in memory
//! as a texture, and on disk as `<settings>/thumbnails/<hash>.png` so a
//! part is rendered once, ever, until it changes.
//!
//! Rendering runs off the UI thread (MF-RT-001): the panel asks for a
//! thumbnail each frame ([`Thumbs::get`]); one that isn't cached yet is
//! queued with its mesh on the background lane (`crate::jobs`), and the panel
//! shows the part's name alone until its picture arrives. Without a GPU
//! a thumbnail simply never arrives (the name is the whole cell), and
//! the failure is kept, never retried every frame.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use eframe::egui;
use mf_geometry::Mesh;

/// The rendered size, in pixels (square).
pub const SIZE: u32 = 128;

/// Bumped when the look changes (size, shading, framing), so a cached
/// picture from before isn't reused.
const VERSION: &str = "thumb-v1-clay-128";

/// What the worker hands back: a key and its picture (or why not).
type Done = (String, Result<egui::ColorImage, String>);

/// The thumbnail cache, owned by the Parts panel's state.
#[derive(Clone, Default)]
pub struct Thumbs {
    ready: HashMap<String, egui::TextureHandle>,
    failed: HashMap<String, String>,
    queued: HashSet<String>,
    done: Arc<Mutex<Vec<Done>>>,
}

impl std::fmt::Debug for Thumbs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Thumbs")
            .field("ready", &self.ready.len())
            .field("failed", &self.failed.len())
            .field("queued", &self.queued.len())
            .finish()
    }
}

/// A part's cache key: its content hash with the look's version, so the
/// same part is the same picture and a changed part a new one.
pub fn key(content_hash: &str) -> String {
    let mut h = blake3::Hasher::new();
    h.update(VERSION.as_bytes());
    h.update(content_hash.as_bytes());
    h.finalize().to_hex().to_string()
}

fn disk_path(dir: &std::path::Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.png"))
}

impl Thumbs {
    /// `key`'s thumbnail, if it's ready. Otherwise it's loaded from the
    /// disk cache in `dir`, or (once) queued for rendering from the mesh
    /// `mesh` makes; it shows on a later frame.
    pub fn get(
        &mut self,
        ctx: &egui::Context,
        jobs: &crate::jobs::Jobs,
        dir: Option<&std::path::Path>,
        key: &str,
        mesh: impl FnOnce() -> Option<Mesh>,
    ) -> Option<egui::TextureHandle> {
        self.collect(ctx);
        if let Some(t) = self.ready.get(key) {
            return Some(t.clone());
        }
        if self.failed.contains_key(key) || self.queued.contains(key) {
            return None;
        }
        if let Some(image) = dir.and_then(|d| read_png(&disk_path(d, key))) {
            let t = ctx.load_texture(format!("part thumb {key}"), image, Default::default());
            self.ready.insert(key.to_owned(), t.clone());
            return Some(t);
        }
        let Some(mesh) = mesh() else {
            self.failed.insert(key.to_owned(), "no mesh".into());
            return None;
        };
        self.queued.insert(key.to_owned());
        let done = self.done.clone();
        let key = key.to_owned();
        let dir = dir.map(std::path::Path::to_path_buf);
        let at = key.clone();
        // On the background lane (ADR-0009): counted by `settle()`, and a
        // panicking render is this thumbnail's failure, kept like any
        // other, never a cell that waits forever. Nothing cancels it (a
        // thumbnail is cached for good once rendered).
        jobs.once(
            &mut None,
            move |_| {
                Ok(render(&mesh).inspect(|image| {
                    if let Some(dir) = &dir {
                        // allow-swallow: a picture that can't be cached on disk is still shown; it's rendered again next session
                        let _ = write_png(dir, &at, image);
                    }
                }))
            },
            move |result| {
                let result = match result {
                    Ok(r) => r,
                    Err(crate::jobs::Failure::Panicked(why)) => Err(why),
                    Err(crate::jobs::Failure::Cancelled) => Err("cancelled".to_owned()),
                };
                done.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((key, result));
            },
        );
        None
    }

    /// Moves finished renders into textures.
    fn collect(&mut self, ctx: &egui::Context) {
        let finished: Vec<Done> =
            std::mem::take(&mut *self.done.lock().unwrap_or_else(|e| e.into_inner()));
        for (key, result) in finished {
            self.queued.remove(&key);
            match result {
                Ok(image) => {
                    let t =
                        ctx.load_texture(format!("part thumb {key}"), image, Default::default());
                    self.ready.insert(key, t);
                }
                Err(why) => {
                    self.failed.insert(key, why);
                }
            }
        }
    }

    /// Why `key` has no picture, if rendering it failed.
    pub fn failure(&self, key: &str) -> Option<&str> {
        self.failed.get(key).map(String::as_str)
    }

    /// How many thumbnails are ready to show.
    pub fn ready(&self) -> usize {
        self.ready.len()
    }

    /// True while any thumbnail is still rendering.
    pub fn busy(&self) -> bool {
        !self.queued.is_empty()
    }
}

/// `mesh` in Clay, framed, `SIZE` square.
fn render(mesh: &Mesh) -> Result<egui::ColorImage, String> {
    let image =
        mf_render::offscreen::render_image(mesh, SIZE, SIZE, false).map_err(|e| e.to_string())?;
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [image.width() as usize, image.height() as usize],
        image.as_raw(),
    ))
}

fn write_png(dir: &std::path::Path, key: &str, image: &egui::ColorImage) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let [w, h] = image.size;
    let bytes: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
    let rgba = image::RgbaImage::from_raw(w as u32, h as u32, bytes)
        .ok_or_else(|| "the picture's size doesn't match its pixels".to_owned())?;
    // Written then renamed, so a reader never sees half a file.
    let tmp = dir.join(format!("{key}.png.part"));
    rgba.save_with_format(&tmp, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, disk_path(dir, key)).map_err(|e| e.to_string())
}

fn read_png(path: &std::path::Path) -> Option<egui::ColorImage> {
    // allow-swallow: a missing or unreadable cached picture is simply rendered again
    let image = image::open(path).ok()?.to_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [image.width() as usize, image.height() as usize],
        image.as_raw(),
    ))
}
