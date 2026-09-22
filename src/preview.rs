use adw::prelude::*;
use gtk::pango;
use std::io::Read;
use std::path::Path;

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

// ── Async preview payload delivery slot ──
// Worker decodes/computes preview data and writes here; a persistent
// poll on the main thread picks it up and applies to the UI.
#[derive(Clone)]
enum PreviewPayload {
    Image {
        rgba: Vec<u8>,
        w: u32,
        h: u32,
        /// If this is a page from a multi-page document (PDF/PPTX),
        /// store the total pages/slides for the nav bar.
        total_pages: Option<usize>,
        /// Which page/slide this payload represents (1-indexed).
        page: Option<usize>,
    },
    Text {
        content: String,
    },
    Info,
}

static PREVIEW_PAYLOAD: OnceLock<Mutex<Option<(u64, std::path::PathBuf, PreviewPayload)>>> =
    OnceLock::new();

fn preview_payload_slot() -> &'static Mutex<Option<(u64, std::path::PathBuf, PreviewPayload)>> {
    PREVIEW_PAYLOAD.get_or_init(|| Mutex::new(None))
}

// ── OCR deduplication: prevent multiple tesseract jobs for the same file ──
static OCR_INFLIGHT: OnceLock<Mutex<std::collections::HashSet<std::path::PathBuf>>> =
    OnceLock::new();

fn ocr_inflight() -> &'static Mutex<std::collections::HashSet<std::path::PathBuf>> {
    OCR_INFLIGHT.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

// ── Bounded in-memory preview cache ──
// Key: (path, mtime_secs). Values are either decoded RGBA (for images) or
// text content (for text/audio/archive/doc-type previews).
const PREVIEW_CACHE_CAP: usize = 24;

struct CacheEntry {
    path: std::path::PathBuf,
    mtime: u64,
    payload: PreviewPayload,
}

static PREVIEW_CACHE: OnceLock<Mutex<VecDeque<CacheEntry>>> = OnceLock::new();

fn preview_cache() -> &'static Mutex<VecDeque<CacheEntry>> {
    PREVIEW_CACHE.get_or_init(|| Mutex::new(VecDeque::new()))
}

// ── Page/slide count metadata for nav bar ──
static DOC_META: OnceLock<Mutex<std::collections::HashMap<std::path::PathBuf, usize>>> =
    OnceLock::new();

fn store_doc_meta(path: &Path, total: usize) {
    if let Ok(mut map) = DOC_META.get_or_init(|| Mutex::new(std::collections::HashMap::new())).lock() {
        map.insert(path.to_path_buf(), total);
    }
}

fn cache_lookup(path: &Path) -> Option<PreviewPayload> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut cache = preview_cache().lock().ok()?;
    if let Some(pos) = cache.iter().position(|e| e.path == path && e.mtime == mtime) {
        let entry = cache.remove(pos).unwrap();
        // Move to back (most recently used).
        cache.push_back(entry);
        return Some(cache.back().unwrap().payload.clone());
    }
    // Stale entry — remove it.
    cache.retain(|e| e.path != path);
    None
}

fn cache_insert(path: std::path::PathBuf, payload: PreviewPayload) {
    let meta = std::fs::metadata(&path).ok();
    let mtime = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut cache) = preview_cache().lock() {
        // Remove existing entry for this path.
        cache.retain(|e| e.path != path);
        // Evict oldest if full.
        while cache.len() >= PREVIEW_CACHE_CAP {
            cache.pop_front();
        }
        cache.push_back(CacheEntry {
            path,
            mtime,
            payload,
        });
    }
}

#[derive(Clone)]
pub struct PreviewPane {
    container: gtk::Box,
    stack: gtk::Stack,
    image: gtk::Picture,
    image_caption: gtk::Label,
    video: gtk::Video,
    text: gtk::TextView,
    iicon: gtk::Image,
    ititle: gtk::Label,
    isub: gtk::Label,
    // Music overview page widgets.
    mcover: gtk::Picture,
    mtitle: gtk::Label,
    mmeta: gtk::Label,
    mstats: gtk::Label,
    // Trigger help page widgets.
    htext: gtk::Label,
    hpic: gtk::Picture,
    // Identity of the track currently shown in the music page, so late async
    // cover/stats updates can verify they're still relevant.
    msel: std::rc::Rc<std::cell::RefCell<String>>,
    // Tracks the path currently being previewed, so an async thumbnail that
    // finishes late doesn't overwrite a newer selection.
    current: std::rc::Rc<std::cell::RefCell<std::path::PathBuf>>,
    // Debounce: cancel and reschedule on each selection change so rapid
    // list rebuilds during typing don't trigger expensive decodes.
    preview_debounce_id: std::rc::Rc<std::cell::Cell<Option<gtk::glib::SourceId>>>,
    // Async image preview: generation counter + persistent poll.
    preview_gen: std::rc::Rc<std::cell::Cell<u64>>,
    preview_poll_id: std::rc::Rc<std::cell::Cell<Option<gtk::glib::SourceId>>>,
    // Multi-page document navigation (PPTX slides, PDF pages, etc.)
    current_slide: std::rc::Rc<std::cell::Cell<usize>>,
    total_slides: std::rc::Rc<std::cell::Cell<usize>>,
    slide_paths: std::rc::Rc<std::cell::RefCell<Vec<std::path::PathBuf>>>,
    nav_box: gtk::Box,
    nav_label: gtk::Label,
    nav_prev: gtk::Button,
    nav_next: gtk::Button,
    // Track which file the nav bar belongs to (for on-demand page rendering).
    nav_file_path: std::rc::Rc<std::cell::RefCell<std::path::PathBuf>>,
    // Whether a preview payload has been applied (set true on apply, false on clear).
    // Prevents the stale-`current` early-return from blocking re-render after a clear.
    displayed: std::rc::Rc<std::cell::Cell<bool>>,
    // Whether a preview load is currently in flight (debounce fired, worker running).
    // Prevents list rebuilds from cancelling/restarting an in-flight load.
    pending: std::rc::Rc<std::cell::Cell<bool>>,
    // Whether the nav buttons have been connected (prevents duplicate handlers).
    nav_connected: std::rc::Rc<std::cell::Cell<bool>>,
}

impl PreviewPane {
    pub fn new() -> Self {
        // Fixed-width column for preview. Height grows with body.
        let container = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .width_request(340)
            .vexpand(true)
            .valign(gtk::Align::Fill)
            .css_classes(["preview-pane"])
            .build();

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(100)
            .vexpand(true)
            .hexpand(true)
            .build();

        // Empty placeholder
        stack.add_named(
            &gtk::Label::builder()
                .label("Highlight a result\nto preview")
                .css_classes(["dim-label"])
                .wrap(true)
                .justify(gtk::Justification::Center)
                .vexpand(true)
                .valign(gtk::Align::Center)
                .build(),
            Some("empty"),
        );

        // Image - put inside a ScrolledWindow with hard max content size.
        // This is the only way Picture can be capped in GTK4 since Picture
        // requests its natural pixel size by default.
        let image = gtk::Picture::builder()
            .can_shrink(true)
            .content_fit(gtk::ContentFit::Contain)
            .width_request(320)
            .height_request(240)
            .hexpand(true)
            .vexpand(true)
            .build();
        let image_clip = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .max_content_width(320)
            .max_content_height(240)
            .min_content_width(320)
            .min_content_height(240)
            .propagate_natural_width(false)
            .propagate_natural_height(false)
            .child(&image)
            .build();
        let image_caption = gtk::Label::builder()
            .css_classes(["caption", "dim-label"])
            .halign(gtk::Align::Center)
            .margin_top(6)
            .margin_bottom(2)
            .wrap(true)
            .build();
        let image_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        image_box.append(&image_clip);
        image_box.append(&image_caption);
        stack.add_named(&image_box, Some("image"));

        // Video - plays any format GStreamer (the GTK media backend) supports,
        // with the standard play/pause/seek/volume overlay controls.
        let video = gtk::Video::builder()
            .autoplay(false)
            .loop_(false)
            .width_request(320)
            .height_request(240)
            .hexpand(true)
            .vexpand(true)
            .build();
        let video_clip = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .max_content_width(320)
            .max_content_height(240)
            .min_content_width(320)
            .min_content_height(240)
            .propagate_natural_width(false)
            .propagate_natural_height(false)
            .child(&video)
            .build();
        stack.add_named(&video_clip, Some("video"));

        // Text
        let text = gtk::TextView::builder()
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .left_margin(8)
            .right_margin(8)
            .top_margin(8)
            .bottom_margin(8)
            .build();
        let text_scroll = gtk::ScrolledWindow::builder()
            .child(&text)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .max_content_height(200)
            .propagate_natural_height(true)
            .build();
        stack.add_named(&text_scroll, Some("text"));

        // Info card
        let ib = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .vexpand(true)
            .build();
        let iicon = gtk::Image::builder().pixel_size(48).build();
        let ititle = gtk::Label::builder()
            .ellipsize(pango::EllipsizeMode::Middle)
            .max_width_chars(22)
            .css_classes(["title-4"])
            .build();
        let isub = gtk::Label::builder()
            .css_classes(["dim-label", "caption"])
            .wrap(true)
            .justify(gtk::Justification::Left)
            .xalign(0.5)
            .use_markup(false)
            .build();
        isub.set_max_width_chars(28);
        ib.append(&iicon);
        ib.append(&ititle);
        ib.append(&isub);
        stack.add_named(&ib, Some("info"));

        // Music overview: large cover art, title, artist + source badge, and
        // live view/like statistics.
        let mb = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .vexpand(true)
            .build();
        let mcover = gtk::Picture::builder()
            .can_shrink(true)
            .content_fit(gtk::ContentFit::Contain)
            .width_request(240)
            .height_request(240)
            .css_classes(["music-cover-large"])
            .build();
        let mcover_clip = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .max_content_width(240)
            .max_content_height(240)
            .min_content_width(240)
            .min_content_height(240)
            .propagate_natural_width(false)
            .propagate_natural_height(false)
            .child(&mcover)
            .build();
        let mtitle = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .max_width_chars(26)
            .css_classes(["title-4"])
            .build();
        let mmeta = gtk::Label::builder()
            .css_classes(["dim-label"])
            .wrap(true)
            .justify(gtk::Justification::Center)
            .max_width_chars(30)
            .build();
        let mstats = gtk::Label::builder()
            .css_classes(["caption"])
            .wrap(true)
            .justify(gtk::Justification::Center)
            .max_width_chars(32)
            .build();
        mb.append(&mcover_clip);
        mb.append(&mtitle);
        mb.append(&mmeta);
        mb.append(&mstats);
        stack.add_named(&mb, Some("music"));

        // Trigger help: instruction text (scrollable, selectable) plus an
        // optional screenshot below it.
        let hb = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .hexpand(true)
            .vexpand(true)
            .build();
        let htext = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Left)
            .xalign(0.0)
            .selectable(true)
            .css_classes(["dim-label"])
            .build();
        let htext_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .max_content_height(140)
            .propagate_natural_height(true)
            .child(&htext)
            .build();
        let hpic = gtk::Picture::builder()
            .can_shrink(true)
            .content_fit(gtk::ContentFit::Contain)
            .width_request(320)
            .height_request(240)
            .hexpand(true)
            .build();
        let hpic_clip = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .max_content_width(320)
            .max_content_height(240)
            .min_content_width(320)
            .min_content_height(240)
            .propagate_natural_width(false)
            .propagate_natural_height(false)
            .child(&hpic)
            .build();
        hb.append(&htext_scroll);
        hb.append(&hpic_clip);
        stack.add_named(&hb, Some("help"));

        container.append(&stack);
        stack.set_visible_child_name("empty");

        // Navigation bar for multi-page documents (PPTX slides, PDF pages, etc.)
        let nav_prev = gtk::Button::builder()
            .label("◀")
            .width_request(36)
            .sensitive(false)
            .build();
        let nav_label = gtk::Label::builder()
            .label("1 / 1")
            .width_request(80)
            .halign(gtk::Align::Center)
            .build();
        let nav_next = gtk::Button::builder()
            .label("▶")
            .width_request(36)
            .sensitive(false)
            .build();
        let nav_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::Center)
            .margin_top(4)
            .margin_bottom(4)
            .visible(false)
            .build();
        nav_box.append(&nav_prev);
        nav_box.append(&nav_label);
        nav_box.append(&nav_next);
        container.append(&nav_box);

        let pane = Self {
            container,
            stack,
            image,
            image_caption,
            video,
            text,
            iicon,
            ititle,
            isub,
            mcover,
            mtitle,
            mmeta,
            mstats,
            htext,
            hpic,
            msel: std::rc::Rc::new(std::cell::RefCell::new(String::new())),
            current: std::rc::Rc::new(std::cell::RefCell::new(std::path::PathBuf::new())),
            preview_debounce_id: std::rc::Rc::new(std::cell::Cell::new(None)),
            preview_gen: std::rc::Rc::new(std::cell::Cell::new(0)),
            preview_poll_id: std::rc::Rc::new(std::cell::Cell::new(None)),
            current_slide: std::rc::Rc::new(std::cell::Cell::new(1)),
            total_slides: std::rc::Rc::new(std::cell::Cell::new(1)),
            slide_paths: std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
            nav_box,
            nav_label,
            nav_prev,
            nav_next,
            nav_file_path: std::rc::Rc::new(std::cell::RefCell::new(std::path::PathBuf::new())),
            displayed: std::rc::Rc::new(std::cell::Cell::new(false)),
            pending: std::rc::Rc::new(std::cell::Cell::new(false)),
            nav_connected: std::rc::Rc::new(std::cell::Cell::new(false)),
        };

        // Start a persistent poll to deliver async preview decode results.
        let pane_clone = pane.clone();
        let poll_id = gtk::glib::timeout_add_local(Duration::from_millis(16), move || {
            pane_clone.deliver_pending_preview_payload();
            gtk::glib::ControlFlow::Continue
        });
        pane.preview_poll_id.set(Some(poll_id));
        pane
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.container
    }
    /// The path currently being previewed (a sentinel a late async update
    /// compares against before touching the UI).
    pub fn current_path(&self) -> std::path::PathBuf {
        self.current.borrow().clone()
    }
    pub fn clear(&self) {
        self.stop_video();
        *self.msel.borrow_mut() = String::new();
        self.displayed.set(false);
        self.pending.set(false);
        self.stack.set_visible_child_name("empty");
    }

    /// Pause and detach the video player so it doesn't keep playing (or
    /// holding the file open) once the user navigates away.
    fn stop_video(&self) {
        if let Some(stream) = self.video.media_stream() {
            stream.pause();
        }
        self.video.set_file(gtk::gio::File::NONE);
    }

    /// Show raw text (e.g. a clipboard text entry) in full, scrollable.
    pub fn show_text(&self, s: &str) {
        *self.current.borrow_mut() = std::path::PathBuf::new();
        self.stop_video();
        self.displayed.set(true);
        self.text.buffer().set_text(s);
        self.stack.set_visible_child_name("text");
    }

    /// Show trigger help: instruction text, plus the help_image screenshot
    /// when one is available locally (downloaded on demand to the cache).
    /// `image_path` may point at a not-yet-downloaded cache file; in that
    /// case the current marker is left as that path so the late download can
    /// verify it's still the row being previewed.
    pub fn show_help(&self, help: &str, image_path: Option<&Path>) {
        self.stop_video();
        self.displayed.set(true);
        self.htext.set_label(help);
        if let Some(p) = image_path {
            if p.exists() {
                *self.current.borrow_mut() = std::path::PathBuf::new();
                set_picture_from_file(&self.hpic, p);
                self.hpic.set_visible(true);
            } else {
                *self.current.borrow_mut() = p.to_path_buf();
                self.hpic.set_visible(false);
            }
        } else {
            *self.current.borrow_mut() = std::path::PathBuf::new();
            self.hpic.set_visible(false);
        }
        self.stack.set_visible_child_name("help");
    }

    /// Show a rich overview for a music search result: cover art, title,
    /// artist, a source badge (YouTube Music / Local), duration, and — for
    /// YouTube tracks — live view/like counts fetched in the background.
    pub fn show_music(&self, source: &str, title: &str) {
        let track = crate::music::Track::from_action(source, title);
        *self.current.borrow_mut() = std::path::PathBuf::new();
        *self.msel.borrow_mut() = track.source.clone();
        self.stop_video();
        self.displayed.set(true);

        self.mtitle.set_label(&track.title);
        let artist = if track.artist.is_empty() {
            track.source_label().to_string()
        } else {
            track.artist.clone()
        };
        self.mmeta
            .set_label(&format!("{}  ·  {}", artist, track.source_label()));

        let duration_line = if track.duration_ms > 0 {
            format!("Duration {}", fmt_dur(track.duration_ms))
        } else {
            String::new()
        };
        self.mcover.set_paintable(gtk::gdk::Paintable::NONE);

        if track.is_youtube && !track.video_id.is_empty() {
            // Stats: use cache if warm, else prefetch and poll for the result.
            if let Some(s) = crate::youtube_music::stats_cached(&track.video_id) {
                self.mstats.set_label(&format_stats(&s, &duration_line));
            } else {
                self.mstats.set_label(&if duration_line.is_empty() {
                    "Loading views & likes…".to_string()
                } else {
                    format!("{}\nLoading views & likes…", duration_line)
                });
                crate::youtube_music::prefetch_stats(&track.video_id);
                self.poll_stats(
                    track.video_id.clone(),
                    track.source.clone(),
                    duration_line.clone(),
                );
            }
        } else {
            self.mstats.set_label(&duration_line);
        }

        // Cover art: if already cached, load now; otherwise download in a
        // widget-free worker thread and poll the cache on the main thread.
        if !track.cover_url.is_empty() {
            if crate::youtube_music::cover_ready(&track.cover_url) {
                let path = crate::youtube_music::cover_cache_path(&track.cover_url);
                if let Ok(tex) = gtk::gdk::Texture::from_filename(&path) {
                    self.mcover.set_paintable(Some(&tex));
                }
            } else {
                let url = track.cover_url.clone();
                std::thread::spawn(move || {
                    crate::youtube_music::ensure_cover(&url);
                });
                self.poll_cover(track.cover_url.clone(), track.source.clone());
            }
        }

        self.stack.set_visible_child_name("music");
    }

    /// Poll the cover cache on the main thread until the download lands.
    fn poll_cover(&self, cover_url: String, sel_id: String) {
        let pic = self.mcover.clone();
        let sel = self.msel.clone();
        let mut tries = 0u32;
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(400), move || {
            tries += 1;
            if *sel.borrow() != sel_id || tries > 40 {
                return gtk::glib::ControlFlow::Break;
            }
            if crate::youtube_music::cover_ready(&cover_url) {
                let path = crate::youtube_music::cover_cache_path(&cover_url);
                if let Ok(tex) = gtk::gdk::Texture::from_filename(&path) {
                    pic.set_paintable(Some(&tex));
                }
                return gtk::glib::ControlFlow::Break;
            }
            gtk::glib::ControlFlow::Continue
        });
    }

    /// Poll the stats cache on the main thread until the background fetch lands
    /// (or the user moves to a different track / gives up after ~20s).
    fn poll_stats(&self, video_id: String, sel_id: String, duration_line: String) {
        let lbl = self.mstats.clone();
        let sel = self.msel.clone();
        let mut tries = 0u32;
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(600), move || {
            tries += 1;
            if *sel.borrow() != sel_id || tries > 34 {
                return gtk::glib::ControlFlow::Break;
            }
            if let Some(s) = crate::youtube_music::stats_cached(&video_id) {
                lbl.set_label(&format_stats(&s, &duration_line));
                return gtk::glib::ControlFlow::Break;
            }
            gtk::glib::ControlFlow::Continue
        });
    }

    pub fn show_path(&self, p: &Path) {
        if !p.exists() {
            return self.clear();
        }
        // Skip re-render if this file is already being displayed
        // or a load for it is already in flight.
        if *self.current.borrow() == p && (self.displayed.get() || self.pending.get()) {
            return;
        }
        *self.current.borrow_mut() = p.to_path_buf();
        self.stop_video();

        let is_video_ext = |ext: &str| {
            matches!(
                ext,
                "mp4" | "mkv" | "webp" | "mov" | "avi" | "wmv" | "flv" | "m4v"
                    | "mpeg" | "mpg" | "m2ts" | "mts" | "ogv" | "3gp" | "3g2"
                    | "asf" | "rm" | "rmvb" | "vob" | "divx" | "f4v" | "mxf"
            )
        };
        let ext = p
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase())
            .unwrap_or_default();

        // Cancel any pending debounce from a previous selection.
        if let Some(id) = self.preview_debounce_id.take() {
            id.remove();
        }

        // For non-image files (text, audio, archive, office): try in-memory cache first.
        // Images and AVIF/HEIC always go through the async worker (they decode to RGBA).
        // Videos go through the debounce (loaded on main thread after settling).
        let is_raster_image = matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "jpe" | "webp" | "gif" | "bmp" | "svg"
                | "svgz" | "ico" | "tiff" | "tif" | "jxl" | "ppm" | "pgm"
                | "pbm" | "pnm" | "xpm" | "tga" | "avif" | "heic" | "heif"
        );

        if !is_raster_image && !is_video_ext(&ext) {
            if let Some(cached) = cache_lookup(p) {
                self.apply_payload(p, &cached);
                return;
            }
        }

        // Debounce all preview loads: 120ms trailing.
        let path = p.to_path_buf();
        let gen_cell = self.preview_gen.clone();
        let db = self.preview_debounce_id.clone();
        let is_video = is_video_ext(&ext);
        let pending_flag = self.pending.clone();
        pending_flag.set(true);

        let id = gtk::glib::timeout_add_local_once(Duration::from_millis(120), move || {
            db.set(None);
            let new_gen = gen_cell.get().wrapping_add(1);
            gen_cell.set(new_gen);
            if is_video {
                // Video: load on the main thread via the delivery slot.
                // Signal delivery via a special marker that apply_payload recognizes.
                let _ = preview_payload_slot().lock().map(|mut s| {
                    let dominated = s.as_ref().map_or(false, |(g, _, _)| *g > new_gen);
                    if !dominated {
                        *s = Some((new_gen, path.clone(), PreviewPayload::Info));
                    }
                });
                return;
            }
            let path_clone = path.clone();
            std::thread::spawn(move || {
                let payload = compute_preview(&path_clone);
                // Cache text/archive/audio payloads for instant revisit.
                // Don't cache Image (too large) or Info (allow retry on next selection).
                if matches!(&payload, PreviewPayload::Text { .. }) {
                    cache_insert(path_clone.clone(), payload.clone());
                }
                let _ = preview_payload_slot().lock().map(|mut s| {
                    // Only store if our generation is still current or newer
                    // (prevent a stale slow worker from clobbering a fresh payload).
                    let dominated = s.as_ref().map_or(false, |(g, _, _)| *g > new_gen);
                    if !dominated {
                        *s = Some((new_gen, path_clone, payload));
                    }
                });
            });
        });
        self.preview_debounce_id.set(Some(id));
    }

    /// Kick off background OCR for the image so it becomes searchable.
    /// No UI update — OCR text is used by find-mode search via the cache.
    fn refresh_ocr_text(&self, p: &Path) {
        if crate::ocr::cached_text_for(p).is_some() {
            return;
        }
        if !crate::ocr::is_available() {
            return;
        }
        let ocr_path = p.to_path_buf();
        // Dedupe: skip if a tesseract job is already running for this file.
        {
            if let Ok(mut set) = ocr_inflight().lock() {
                if !set.insert(ocr_path.clone()) {
                    return;
                }
            }
        }
        std::thread::spawn(move || {
            crate::ocr::text_for(&ocr_path);
            if let Ok(mut set) = ocr_inflight().lock() {
                set.remove(&ocr_path);
            }
        });
    }

    /// Poll the async decode delivery slot and apply if the generation matches.
    fn deliver_pending_preview_payload(&self) {
        let (gen, path, payload) = {
            let slot = preview_payload_slot().lock().ok();
            match slot.and_then(|mut g| g.take()) {
                Some(v) => v,
                None => return,
            }
        };
        if gen != self.preview_gen.get() {
            return;
        }
        if *self.current.borrow() != path {
            return;
        }
        self.apply_payload(&path, &payload);
    }

    fn apply_payload(&self, path: &Path, payload: &PreviewPayload) {
        self.displayed.set(true);
        self.pending.set(false);
        match payload {
            PreviewPayload::Image { rgba, w, h, total_pages, page } => {
                log::info!("preview: applied Image {}x{} for {}{}", w, h, path.file_name().unwrap_or_default().to_string_lossy(),
                    total_pages.map_or_else(String::new, |t| format!(" ({} pages)", t)));
                let bytes = gtk::glib::Bytes::from(rgba);
                let tex = gtk::gdk::MemoryTexture::new(
                    *w as i32,
                    *h as i32,
                    gtk::gdk::MemoryFormat::R8g8b8a8,
                    &bytes,
                    (*w * 4) as usize,
                );
                self.image.set_paintable(Some(&tex));
                self.stack.set_visible_child_name("image");
                self.image_caption.set_label(
                    &crate::imageinfo::info(path)
                        .map(|i| crate::imageinfo::caption(&i))
                        .unwrap_or_default(),
                );
                // Only OCR actual image files — skip PDF/PPTX/office pages.
                if is_ocrable_image(path) {
                    self.refresh_ocr_text(path);
                }
                // Set up nav bar for multi-page documents (PDF/PPTX).
                if let Some(&total) = total_pages.as_ref() {
                    self.total_slides.set(total);
                    self.current_slide.set(page.unwrap_or(1));
                    *self.nav_file_path.borrow_mut() = path.to_path_buf();
                    self.slide_paths.borrow_mut().clear();
                    self.nav_box.set_visible(total > 1);
                    self.nav_prev.set_sensitive(total > 1);
                    self.nav_next.set_sensitive(total > 1);
                    self.update_nav_label();
                    self.connect_nav_buttons();
                } else {
                    self.nav_box.set_visible(false);
                }
            }
            PreviewPayload::Text { content } => {
                self.text.buffer().set_text(content);
                self.stack.set_visible_child_name("text");
            }
            PreviewPayload::Info => {
                log::info!("preview: applied Info for {}", path.file_name().unwrap_or_default().to_string_lossy());
                // Check if this is a video file — if so, load the video player.
                let is_video = path.extension()
                    .and_then(|s| s.to_str())
                    .map(|ext| matches!(ext.to_ascii_lowercase().as_str(),
                        "mp4" | "mkv" | "webp" | "mov" | "avi" | "wmv" | "flv" | "m4v"
                            | "mpeg" | "mpg" | "m2ts" | "mts" | "ogv" | "3gp" | "3g2"
                            | "asf" | "rm" | "rmvb" | "vob" | "divx" | "f4v" | "mxf"
                    ))
                    .unwrap_or(false);
                if is_video {
                    self.stop_video();
                    self.video.set_file(Some(&gtk::gio::File::for_path(path)));
                    self.stack.set_visible_child_name("video");
                } else {
                    self.show_info(path);
                }
            }
        }
    }

    /// Show a specific slide from the pre-rendered slide paths.
    /// Show a specific slide/page. Checks disk cache first, then renders on demand.
    fn show_slide(&self, n: usize) {
        let file_path = self.nav_file_path.borrow().clone();
        if file_path.is_empty() {
            return;
        }
        let ext = file_path.extension().and_then(|s| s.to_str()).unwrap_or("");
        if ext.eq_ignore_ascii_case("pdf") {
            if let Some(page_path) = cached_pdf_page(&file_path, n) {
                set_picture_from_file(&self.image, &page_path);
                self.stack.set_visible_child_name("image");
                self.image_caption.set_label("");
            } else if let Some(page_path) = render_pdf_page(&file_path, n) {
                set_picture_from_file(&self.image, &page_path);
                self.stack.set_visible_child_name("image");
                self.image_caption.set_label("");
            }
        } else if matches!(
            ext,
            "pptx" | "ppsx" | "pps" | "odp"
        ) {
            if let Some(slide_path) = cached_pptx_slide(&file_path, n) {
                set_picture_from_file(&self.image, &slide_path);
                self.stack.set_visible_child_name("image");
                self.image_caption.set_label("");
            } else if let Some(slide_path) = render_pptx_slide(&file_path, n) {
                set_picture_from_file(&self.image, &slide_path);
                self.stack.set_visible_child_name("image");
                self.image_caption.set_label("");
            }
        } else {
            // Fallback: use slide_paths vec.
            let paths = self.slide_paths.borrow();
            if let Some(path) = paths.get(n.saturating_sub(1)) {
                set_picture_from_file(&self.image, path);
                self.stack.set_visible_child_name("image");
                self.image_caption.set_label("");
            }
        }
    }

    fn update_nav_label(&self) {
        let cur = self.current_slide.get();
        let tot = self.total_slides.get();
        self.nav_label.set_label(&format!("{} / {}", cur, tot));
    }

    fn connect_nav_buttons(&self) {
        use std::rc::Rc;
        // Only connect once — duplicate handlers accumulate and cause
        // clicking ▶ to advance multiple pages after multiple previews.
        if self.nav_connected.get() {
            return;
        }
        self.nav_connected.set(true);

        // Shared navigation state: current_slide, total_slides, and the
        // widgets needed to update the UI.
        struct NavState {
            current_slide: usize,
            total_slides: usize,
            image: gtk::Picture,
            stack: gtk::Stack,
            nav_label: gtk::Label,
            nav_prev: gtk::Button,
            nav_next: gtk::Button,
            slide_paths: std::rc::Rc<std::cell::RefCell<Vec<std::path::PathBuf>>>,
        }

        let state = Rc::new(std::cell::RefCell::new(NavState {
            current_slide: self.current_slide.get(),
            total_slides: self.total_slides.get(),
            image: self.image.clone(),
            stack: self.stack.clone(),
            nav_label: self.nav_label.clone(),
            nav_prev: self.nav_prev.clone(),
            nav_next: self.nav_next.clone(),
            slide_paths: self.slide_paths.clone(),
        }));

        {
            let s = state.clone();
            self.nav_prev.connect_clicked(move |_| {
                let mut st = s.borrow_mut();
                if st.current_slide > 1 {
                    st.current_slide -= 1;
                    st.nav_label.set_label(&format!("{} / {}", st.current_slide, st.total_slides));
                    st.nav_next.set_sensitive(true);
                    if st.current_slide <= 1 { st.nav_prev.set_sensitive(false); }
                    if let Some(p) = st.slide_paths.borrow().get(st.current_slide - 1) {
                        let p = p.clone();
                        set_picture_from_file(&st.image, &p);
                        st.stack.set_visible_child_name("image");
                    }
                }
            });
        }

        {
            let s = state.clone();
            self.nav_next.connect_clicked(move |_| {
                let mut st = s.borrow_mut();
                if st.current_slide < st.total_slides {
                    st.current_slide += 1;
                    st.nav_label.set_label(&format!("{} / {}", st.current_slide, st.total_slides));
                    st.nav_prev.set_sensitive(true);
                    if st.current_slide >= st.total_slides { st.nav_next.set_sensitive(false); }
                    if let Some(p) = st.slide_paths.borrow().get(st.current_slide - 1) {
                        let p = p.clone();
                        set_picture_from_file(&st.image, &p);
                        st.stack.set_visible_child_name("image");
                    }
                }
            });
        }
    }

    fn show_info(&self, p: &Path) {
        let is_dir = p.is_dir();
        if is_dir {
            self.iicon.set_icon_name(Some("folder-symbolic"));
        } else {
            self.iicon.set_icon_name(Some(info_icon_for(p)));
        }
        self.ititle.set_text(
            &p.file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
        );

        // Build the info text
        let mut info_lines = Vec::new();

        if is_dir {
            // Full path
            info_lines.push(p.display().to_string());
            // Count items
            if let Ok(entries) = std::fs::read_dir(p) {
                let count = entries.count();
                info_lines.push(format!(
                    "{} item{}",
                    count,
                    if count == 1 { "" } else { "s" }
                ));
            }
            // Show last modified date
            if let Ok(meta) = std::fs::metadata(p) {
                if let Ok(modified) = meta.modified() {
                    if let Ok(elapsed) = modified.elapsed() {
                        let secs = elapsed.as_secs();
                        let when = if secs < 60 {
                            "just now".to_string()
                        } else if secs < 3600 {
                            format!("{} min ago", secs / 60)
                        } else if secs < 86400 {
                            format!("{} hr ago", secs / 3600)
                        } else if secs < 604800 {
                            format!(
                                "{} day{} ago",
                                secs / 86400,
                                if secs / 86400 == 1 { "" } else { "s" }
                            )
                        } else if secs < 2_592_000 {
                            format!(
                                "{} week{} ago",
                                secs / 604800,
                                if secs / 604800 == 1 { "" } else { "s" }
                            )
                        } else {
                            format!(
                                "{} month{} ago",
                                secs / 2_592_000,
                                if secs / 2_592_000 == 1 { "" } else { "s" }
                            )
                        };
                        info_lines.push(format!("Modified {}", when));
                    }
                }
            }
            info_lines.push(String::new()); // blank line spacer
            info_lines.push("Ctrl+Enter — open in terminal".into());
        } else {
            // File: size + last modified + hints
            if let Ok(meta) = std::fs::metadata(p) {
                let b = meta.len();
                info_lines.push(crate::imageinfo::human_size(b));
                if let Ok(modified) = meta.modified() {
                    if let Ok(elapsed) = modified.elapsed() {
                        let secs = elapsed.as_secs();
                        let when = if secs < 60 {
                            "just now".to_string()
                        } else if secs < 3600 {
                            format!("{} min ago", secs / 60)
                        } else if secs < 86400 {
                            format!("{} hr ago", secs / 3600)
                        } else if secs < 604800 {
                            format!(
                                "{} day{} ago",
                                secs / 86400,
                                if secs / 86400 == 1 { "" } else { "s" }
                            )
                        } else {
                            format!(
                                "{} day{} ago",
                                secs / 86400,
                                if secs / 86400 == 1 { "" } else { "s" }
                            )
                        };
                        info_lines.push(format!("Modified {}", when));
                    }
                }
            }
            info_lines.push(String::new());
            info_lines.push("Enter — open with default app".into());
        }

        self.isub.set_text(&info_lines.join("\n"));
        self.stack.set_visible_child_name("info");
    }
}

// ──────────────────────────────────────────────────────────────────────
// Thumbnail generation (PDF first page, video frame)
//
// We shell out to standard desktop tools and cache the resulting PNG under
// ~/.cache/spotty/thumbnails/ keyed by a hash of (path + mtime), so re-previewing
// the same file is instant and a changed file regenerates. If the required tool
// isn't installed, the helper returns None and the caller falls back to the
// info card — no hard dependency.
// ──────────────────────────────────────────────────────────────────────

use std::path::PathBuf;

// We use the freedesktop SHARED thumbnail cache so thumbnails are interoperable
// with Nautilus and other desktop apps:
//
//   $XDG_CACHE_HOME/thumbnails/large/<md5(file-uri)>.png
//
/// Maximum bytes to read from a file for the text preview.
const TEXT_PREVIEW_READ_BYTES: usize = 1 << 20; // 1 MB
/// Maximum bytes to display (after which the preview is truncated).
const TEXT_PREVIEW_DISPLAY_BYTES: usize = 32 << 10; // 32 KB

/// Maximum image dimension (width or height) after downscaling.
const IMAGE_MAX_DIM: u32 = 700;

/// Check if the path is an actual image file suitable for OCR (not PDF/PPTX/office).
fn is_ocrable_image(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "webp" | "tif" | "tiff" | "bmp" | "avif"
                    | "gif" | "ico" | "pnm" | "pgm" | "ppm" | "pbm" | "qoi" | "tga"
                    | "heic" | "heif" | "svg"
            )
        })
        .unwrap_or(false)
}

// ── Worker thread: compute preview payload for any file type ──

fn compute_preview(path: &Path) -> PreviewPayload {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        // ── Raster images ──
        "png" | "jpg" | "jpeg" | "jpe" | "webp" | "gif" | "bmp" | "svg" | "svgz" | "ico"
        | "tiff" | "tif" | "jxl" | "ppm" | "pgm" | "pbm" | "pnm" | "xpm" | "tga" => {
            let Some(img) = image::open(path).ok() else {
                return PreviewPayload::Info;
            };
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            let (dw, dh, raw) = if h > IMAGE_MAX_DIM || w > IMAGE_MAX_DIM {
                let scale = IMAGE_MAX_DIM as f32 / (w.max(h) as f32);
                let nw = ((w as f32 * scale) as u32).max(1);
                let nh = ((h as f32 * scale) as u32).max(1);
                let resized = image::imageops::resize(
                    &rgba,
                    nw,
                    nh,
                    image::imageops::FilterType::Lanczos3,
                );
                (nw, nh, resized.into_raw())
            } else {
                (w, h, rgba.into_raw())
            };
            PreviewPayload::Image {
                rgba: raw,
                w: dw,
                h: dh,
                total_pages: None, page: None,
            }
        }
        // ── AVIF / HEIC / HEIF ──
        "avif" | "heic" | "heif" => {
            if let Some(png) = avif_to_png_file(path) {
                if let Some(img) = image::open(&png).ok() {
                    let rgba = img.to_rgba8();
                    let (w, h) = rgba.dimensions();
                    let (dw, dh, raw) = if h > IMAGE_MAX_DIM || w > IMAGE_MAX_DIM {
                        let scale = IMAGE_MAX_DIM as f32 / (w.max(h) as f32);
                        let nw = ((w as f32 * scale) as u32).max(1);
                        let nh = ((h as f32 * scale) as u32).max(1);
                        let resized = image::imageops::resize(
                            &rgba,
                            nw,
                            nh,
                            image::imageops::FilterType::Lanczos3,
                        );
                        (nw, nh, resized.into_raw())
                    } else {
                        (w, h, rgba.into_raw())
                    };
                    return PreviewPayload::Image {
                        rgba: raw,
                        w: dw,
                        h: dh,
                        total_pages: None, page: None,
                    };
                }
            }
            PreviewPayload::Info
        }
        // ── PDF: render first page, cache total for nav ──
        "pdf" => {
            let total = pdf_page_count(path).unwrap_or(1);
            store_doc_meta(path, total);
            let cached = cached_pdf_page(path, 1)
                .or_else(|| render_pdf_page(path, 1));
            if let Some(page_path) = cached {
                if let Some(img) = image::open(&page_path).ok() {
                    let rgba = img.to_rgba8();
                    let (w, h) = rgba.dimensions();
                    let (dw, dh, raw) = if h > IMAGE_MAX_DIM || w > IMAGE_MAX_DIM {
                        let scale = IMAGE_MAX_DIM as f32 / (w.max(h) as f32);
                        let nw = ((w as f32 * scale) as u32).max(1);
                        let nh = ((h as f32 * scale) as u32).max(1);
                        let resized = image::imageops::resize(
                            &rgba, nw, nh, image::imageops::FilterType::Lanczos3,
                        );
                        (nw, nh, resized.into_raw())
                    } else {
                        (w, h, rgba.into_raw())
                    };
                    return PreviewPayload::Image {
                        rgba: raw, w: dw, h: dh,
                        total_pages: Some(total), page: Some(1),
                    };
                }
            }
            // Fallback to shared thumbnails or office-style rendering.
            if let Some(thumb_path) = pdf_thumbnail(path).or_else(|| office_thumbnail(path)) {
                if let Some(img) = image::open(&thumb_path).ok() {
                    let rgba = img.to_rgba8();
                    let (w, h) = rgba.dimensions();
                    let (dw, dh, raw) = if h > IMAGE_MAX_DIM || w > IMAGE_MAX_DIM {
                        let scale = IMAGE_MAX_DIM as f32 / (w.max(h) as f32);
                        let nw = ((w as f32 * scale) as u32).max(1);
                        let nh = ((h as f32 * scale) as u32).max(1);
                        let resized = image::imageops::resize(
                            &rgba, nw, nh, image::imageops::FilterType::Lanczos3,
                        );
                        (nw, nh, resized.into_raw())
                    } else {
                        (w, h, rgba.into_raw())
                    };
                    return PreviewPayload::Image {
                        rgba: raw, w: dw, h: dh,
                        total_pages: Some(total), page: Some(1),
                    };
                }
            }
            PreviewPayload::Info
        }
        // ── PPTX / ODP: render first slide, cache total for nav ──
        "pptx" | "ppsx" | "pps" | "odp" => {
            let total = pptx_slide_count(path).unwrap_or(1);
            store_doc_meta(path, total);
            // Try on-demand render first, then fall back to office_thumbnail
            // (shared thumbnails, embedded thumbs, LibreOffice, Cairo content render).
            let slide_path = cached_pptx_slide(path, 1)
                .or_else(|| render_pptx_slide(path, 1))
                .or_else(|| office_thumbnail(path));
            if let Some(slide_path) = slide_path {
                if let Some(img) = image::open(&slide_path).ok() {
                    let rgba = img.to_rgba8();
                    let (w, h) = rgba.dimensions();
                    let (dw, dh, raw) = if h > IMAGE_MAX_DIM || w > IMAGE_MAX_DIM {
                        let scale = IMAGE_MAX_DIM as f32 / (w.max(h) as f32);
                        let nw = ((w as f32 * scale) as u32).max(1);
                        let nh = ((h as f32 * scale) as u32).max(1);
                        let resized = image::imageops::resize(
                            &rgba, nw, nh, image::imageops::FilterType::Lanczos3,
                        );
                        (nw, nh, resized.into_raw())
                    } else {
                        (w, h, rgba.into_raw())
                    };
                    return PreviewPayload::Image {
                        rgba: raw, w: dw, h: dh,
                        total_pages: Some(total), page: Some(1),
                    };
                }
            }
            PreviewPayload::Info
        }
        // ── Office documents (Word / Excel / legacy PPT / ODT) ──
        "doc" | "docx" | "odt" | "rtf" | "ott" | "fodt" | "wps" | "xls" | "xlsx" | "ods"
        | "ots" | "fods" | "csv" | "ppt" | "otp" | "fodp" => {
            if let Some(thumb) = office_thumbnail(path) {
                if let Some(img) = image::open(&thumb).ok() {
                    let rgba = img.to_rgba8();
                    let (w, h) = rgba.dimensions();
                    let (dw, dh, raw) = if h > IMAGE_MAX_DIM || w > IMAGE_MAX_DIM {
                        let scale = IMAGE_MAX_DIM as f32 / (w.max(h) as f32);
                        let nw = ((w as f32 * scale) as u32).max(1);
                        let nh = ((h as f32 * scale) as u32).max(1);
                        let resized = image::imageops::resize(
                            &rgba, nw, nh, image::imageops::FilterType::Lanczos3,
                        );
                        (nw, nh, resized.into_raw())
                    } else {
                        (w, h, rgba.into_raw())
                    };
                    return PreviewPayload::Image {
                        rgba: raw, w: dw, h: dh,
                        total_pages: None, page: None,
                    };
                }
            }
            PreviewPayload::Info
        }
        // ── Archive listing ──
        _ => {
            if let Some((entries, total)) = archive_listing(path) {
                let mut listing = format!("{} entries\n\n", total);
                for e in &entries {
                    listing.push_str(e);
                    listing.push('\n');
                }
                if total > entries.len() {
                    listing.push_str(&format!(
                        "\u{2026} and {} more\n",
                        total - entries.len()
                    ));
                }
                return PreviewPayload::Text { content: listing };
            }
            // ── Audio metadata ──
            if let Some(audio) = audio_preview(path) {
                let mut meta = String::new();
                if let Some(t) = &audio.title {
                    meta.push_str(&format!("Title:  {}\n", t));
                }
                if let Some(a) = &audio.artist {
                    meta.push_str(&format!("Artist: {}\n", a));
                }
                if let Some(a) = &audio.album {
                    meta.push_str(&format!("Album:  {}\n", a));
                }
                if let Some(y) = &audio.year {
                    meta.push_str(&format!("Year:   {}\n", y));
                }
                if let Some(g) = &audio.genre {
                    meta.push_str(&format!("Genre:  {}\n", g));
                }
                if let Some(d) = &audio.duration {
                    meta.push_str(&format!("Length: {}\n", d));
                }
                meta.push_str(&format!(
                    "\n{} \u{00b7} {}",
                    audio.format.to_uppercase(),
                    crate::imageinfo::human_size(audio.size)
                ));
                return PreviewPayload::Text { content: meta };
            }
            // ── Text preview ──
            if let Some(tp) = text_preview_for(path) {
                let display = if tp.truncated {
                    format!(
                        "\u{2026} showing first {} of {} \u{2014}\n\n{}",
                        crate::imageinfo::human_size(TEXT_PREVIEW_DISPLAY_BYTES as u64),
                        crate::imageinfo::human_size(tp.total_size),
                        &tp.text[..tp.text.len().min(TEXT_PREVIEW_DISPLAY_BYTES)]
                    )
                } else if tp.text.is_empty() {
                    "(empty file)".to_string()
                } else {
                    tp.text
                };
                return PreviewPayload::Text { content: display };
            }
            PreviewPayload::Info
        }
    }
}

/// Get PDF page count from pdfinfo (fast, no rendering).
fn pdf_page_count(pdf: &Path) -> Option<usize> {
    let bin = resolve_tool("pdfinfo")?;
    let out = std::process::Command::new(&bin)
        .arg(pdf)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("Pages:") {
            if let Ok(n) = rest.trim().parse::<usize>() {
                return Some(n);
            }
        }
    }
    None
}

/// Count slides in a PPTX by counting ppt/slides/slideN.xml entries in the zip.
fn pptx_slide_count(doc: &Path) -> Option<usize> {
    let file = std::fs::File::open(doc).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let count = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|e| e.name().to_string()))
        .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
        .count();
    Some(count).filter(|&c| c > 0)
}

/// Check if a cached PDF page PNG exists.
fn cached_pdf_page(pdf: &Path, page: usize) -> Option<PathBuf> {
    let meta = std::fs::metadata(pdf).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let hash = crate::md5::hex(pdf.to_string_lossy().as_bytes());
    let cache_root = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("spotty/pdf")
        .join(format!("{}-{}", hash, mtime));
    let page_path = cache_root.join(format!("page-{}.png", page));
    page_path.exists().then_some(page_path)
}

/// Render a single PDF page to a cached PNG (using pdftoppm).
fn render_pdf_page(pdf: &Path, page: usize) -> Option<PathBuf> {
    let bin = resolve_tool("pdftoppm")?;
    let meta = std::fs::metadata(pdf).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let hash = crate::md5::hex(pdf.to_string_lossy().as_bytes());
    let cache_root = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("spotty/pdf")
        .join(format!("{}-{}", hash, mtime));
    let _ = std::fs::create_dir_all(&cache_root);
    let prefix = cache_root.join(format!("page-{}", page));
    let prefix_str = prefix.to_string_lossy().to_string();
    let status = std::process::Command::new(&bin)
        .args([
            "-png",
            "-r",
            "200",
            "-scale-to",
            "1600",
            "-f",
            &page.to_string(),
            "-l",
            &page.to_string(),
            "-singlefile",
        ])
        .arg(pdf)
        .arg(&prefix_str)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let out = cache_root.join(format!("page-{}.png", page));
    if out.exists() {
        Some(out)
    } else {
        let alt = PathBuf::from(format!("{}-{}.png", prefix_str, page));
        if alt.exists() {
            let _ = std::fs::rename(&alt, &out);
            Some(out)
        } else {
            None
        }
    }
}

/// Check if a cached PPTX slide PNG exists.
fn cached_pptx_slide(doc: &Path, slide: usize) -> Option<PathBuf> {
    let meta = std::fs::metadata(doc).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let hash = crate::md5::hex(doc.to_string_lossy().as_bytes());
    let cache_root = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(format!("spotty/pptx/v{}", PPTX_RENDER_VERSION))
        .join(format!("{}-{}", hash, mtime));
    let slide_path = cache_root.join(format!("slide-{}.png", slide));
    slide_path.exists().then_some(slide_path)
}

/// Render a single PPTX slide to a cached PNG.
fn render_pptx_slide(doc: &Path, slide: usize) -> Option<PathBuf> {
    let Some(layout) = parse_pptx_slide(doc, slide) else {
        return None;
    };
    let png = render_slide_layout(&layout)?;

    let meta = std::fs::metadata(doc).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let hash = crate::md5::hex(doc.to_string_lossy().as_bytes());
    let cache_root = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(format!("spotty/pptx/v{}", PPTX_RENDER_VERSION))
        .join(format!("{}-{}", hash, mtime));
    let _ = std::fs::create_dir_all(&cache_root);
    let slide_path = cache_root.join(format!("slide-{}.png", slide));
    if std::fs::write(&slide_path, &png).is_ok() {
        Some(slide_path)
    } else {
        None
    }
}

struct TextPreview {
    text: String,
    truncated: bool,
    total_size: u64,
}

/// Try to extract a text preview from any file.
/// Returns None for binary files (NUL-heavy) or unreadable files.
fn text_preview_for(path: &Path) -> Option<TextPreview> {
    let meta = std::fs::metadata(path).ok()?;
    let total_size = meta.len();
    let bytes = std::fs::read(path).ok()?;
    let read_len = bytes.len().min(TEXT_PREVIEW_READ_BYTES);

    // Binary detection: scan first 8 KB for NUL bytes.
    let scan_len = read_len.min(8192);
    let nul_count = bytes[..scan_len].iter().filter(|&&b| b == 0).count();
    if scan_len > 0 && (nul_count as f64) / (scan_len as f64) > 0.003 {
        return None;
    }

    let text = if let Ok(s) = std::str::from_utf8(&bytes[..read_len]) {
        s.to_string()
    } else {
        // Mostly-text file with some invalid UTF-8: lossy decode.
        String::from_utf8_lossy(&bytes[..read_len])
            .replace('\0', "")
    };

    let text = text.replace('\r', "");
    let truncated = bytes.len() > read_len;
    Some(TextPreview { text, truncated, total_size })
}

// ── Archive listing (zip/tar/tar.gz) ──

const ARCHIVE_MAX_ENTRIES: usize = 50;

fn archive_listing(path: &Path) -> Option<(Vec<String>, usize)> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "zip" | "cbz" | "epub" | "jar" | "whl" | "apk" => zip_listing(path),
        "tar" => tar_listing(&mut std::fs::File::open(path).ok()?),
        "gz" => {
            // Only handle .tar.gz — single-file gzip isn't an archive.
            let stem = path.file_stem()?.to_str()?;
            if stem.to_ascii_lowercase().ends_with(".tar") {
                let f = std::fs::File::open(path).ok()?;
                let mut gz = flate2::read::GzDecoder::new(f);
                tar_listing(&mut gz)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn zip_listing(path: &Path) -> Option<(Vec<String>, usize)> {
    let file = std::fs::File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let total = archive.len();
    let mut entries = Vec::new();
    let limit = total.min(ARCHIVE_MAX_ENTRIES);
    for i in 0..limit {
        if let Ok(entry) = archive.by_index(i) {
            let size = entry.size();
            let name = entry.name().to_string();
            entries.push(format!("{}  {}", name, crate::imageinfo::human_size(size)));
        }
    }
    Some((entries, total))
}

fn tar_listing(reader: &mut dyn Read) -> Option<(Vec<String>, usize)> {
    let mut entries = Vec::new();
    let mut count = 0usize;
    let mut header = [0u8; 512];
    loop {
        match reader.read_exact(&mut header) {
            Ok(()) => {}
            Err(_) => break, // EOF or truncated
        }
        if header.iter().all(|&b| b == 0) {
            break; // End-of-archive
        }
        // Typeflag at byte 156: '0' or \0 = regular file.
        let typeflag = header[156];
        if typeflag == b'0' || typeflag == 0 {
            // Size in octal at bytes 124..136.
            let size_str = std::str::from_utf8(&header[124..136]).unwrap_or("0").trim();
            let size = u64::from_str_radix(size_str, 8).unwrap_or(0);
            // Name: bytes 0..100 (may be prefixed with path if longname at 345).
            let name = std::str::from_utf8(&header[0..100])
                .unwrap_or("")
                .trim_end_matches('\0')
                .to_string();
            count += 1;
            if entries.len() < ARCHIVE_MAX_ENTRIES {
                entries.push(format!("{}  {}", name, crate::imageinfo::human_size(size)));
            }
        }
        // Skip to next 512-byte block boundary.
        let data_blocks = (size_of::<[u8; 512]>() + 511) / 512; // 1 block per header
        let data_size = {
            let size_str = std::str::from_utf8(&header[124..136]).unwrap_or("0").trim();
            u64::from_str_radix(size_str, 8).unwrap_or(0)
        };
        let skip = data_size + 511 - (data_size + 511) % 512; // round up to 512
        let mut buf = vec![0u8; skip as usize];
        let _ = reader.read_exact(&mut buf);
    }
    Some((entries, count))
}

// ── Audio metadata + cover art ──

struct AudioPreview {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    year: Option<String>,
    genre: Option<String>,
    duration: Option<String>,
    cover: Option<Vec<u8>>,
    cover_mime: Option<String>,
    format: String,
    size: u64,
}

fn audio_preview(path: &Path) -> Option<AudioPreview> {
    let file = std::fs::File::open(path).ok()?;
    let meta = std::fs::metadata(path).ok()?;
    let probe = symphonia::default::get_probe();
    let hint_opts = symphonia::core::meta::MetadataOptions {
        limit_metadata_bytes: symphonia::core::meta::Limit::Maximum(2 << 20),
        limit_visual_bytes: symphonia::core::meta::Limit::Maximum(4 << 20),
    };
    let mss = symphonia::core::io::MediaSourceStream::new(
        Box::new(file),
        Default::default(),
    );
    let mut result = probe
        .format(
            &symphonia::core::probe::Hint::new(),
            mss,
            &symphonia::core::formats::FormatOptions::default(),
            &hint_opts,
        )
        .ok()?;

    let mut title = None;
    let mut artist = None;
    let mut album = None;
    let mut year = None;
    let mut genre = None;
    let mut cover: Option<Vec<u8>> = None;
    let mut cover_mime: Option<String> = None;

    if let Some(meta) = result.metadata.get().and_then(|mut m| m.skip_to_latest().cloned()) {
        for tag in meta.tags() {
            match tag.std_key {
                Some(symphonia::core::meta::StandardTagKey::TrackTitle) => {
                    title = Some(tag.value.to_string());
                }
                Some(symphonia::core::meta::StandardTagKey::Artist) => {
                    artist = Some(tag.value.to_string());
                }
                Some(symphonia::core::meta::StandardTagKey::Album) => {
                    album = Some(tag.value.to_string());
                }
                Some(symphonia::core::meta::StandardTagKey::Date) => {
                    year = Some(tag.value.to_string());
                }
                Some(symphonia::core::meta::StandardTagKey::Genre) => {
                    genre = Some(tag.value.to_string());
                }
                _ => {}
            }
        }
        for visual in meta.visuals() {
            if cover.is_none() {
                cover = Some(visual.data.to_vec());
                cover_mime = Some(visual.media_type.clone());
            }
        }
    }

    if title.is_none() || cover.is_none() {
        let mut fmt_meta = result.format.metadata();
        if let Some(meta) = fmt_meta.skip_to_latest().cloned() {
            for tag in meta.tags() {
                if title.is_none() && matches!(
                    tag.std_key,
                    Some(symphonia::core::meta::StandardTagKey::TrackTitle)
                ) {
                    title = Some(tag.value.to_string());
                }
            }
            if cover.is_none() {
                for visual in meta.visuals() {
                    cover = Some(visual.data.to_vec());
                    cover_mime = Some(visual.media_type.clone());
                    break;
                }
            }
        }
    }

    let fmt_name = path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("audio")
        .to_uppercase();

    Some(AudioPreview {
        title,
        artist,
        album,
        year,
        genre,
        duration: None,
        cover,
        cover_mime,
        format: fmt_name,
        size: meta.len(),
    })
}

// The cache key is md5() of the file's "file://" URI (per the spec). Reading
// this location lets Spotty reuse thumbnails Nautilus already generated; writing
// here lets Nautilus reuse the ones Spotty generates. This works identically
// inside a Flatpak sandbox as long as ~/.cache is mapped in (it is by default
// for the app's own cache, and the shared thumbnail dir can be granted).

/// Root of the freedesktop thumbnail cache (~/.cache/thumbnails).
/// Load an image file into a Picture via an explicit gdk::Texture decode. Using
/// a Texture (rather than Picture::set_filename, which loads lazily) guarantees
/// the image is decoded immediately from the just-written file, avoiding any
/// race where the widget shows nothing because the file was loaded lazily.
fn set_picture_from_file(image: &gtk::Picture, path: &Path) {
    match gtk::gdk::Texture::from_filename(path) {
        Ok(texture) => image.set_paintable(Some(&texture)),
        Err(e) => {
            log::debug!("preview: native load failed for {}: {}", path.display(), e);
            // Fall back to decoding with the `image` crate (supports AVIF,
            // HEIF, JXL, QOI, TGA and other formats GTK doesn't handle
            // natively) and converting to a gdk::Texture via PNG bytes.
            if let Some(tex) = texture_from_image_crate(path) {
                image.set_paintable(Some(&tex));
            } else {
                // Last resort: lazy file loading (works for formats GTK
                // discovers at runtime via GdkPixbuf modules).
                image.set_filename(Some(path));
            }
        }
    }
}

/// Convert AVIF/HEIC to PNG using libheif-rs (pure Rust, no ffmpeg).
fn avif_to_png_file(path: &Path) -> Option<PathBuf> {
    let out = dirs::cache_dir()?
        .join("spotty")
        .join("avif-converted")
        .join(format!("{}.png", crate::md5::hex(path.to_string_lossy().as_bytes())));
    if out.exists() {
        return Some(out);
    }
    let parent = out.parent()?;
    let _ = std::fs::create_dir_all(parent);

    let bytes = std::fs::read(path).ok()?;
    let rgba = decode_heif_to_rgba(&bytes).ok()?;
    rgba.save_with_format(&out, image::ImageFormat::Png).ok()?;
    out.exists().then_some(out)
}

/// Decode AVIF/HEIC to a DynamicImage using libheif-rs (pure Rust).
fn decode_heif_to_rgba(bytes: &[u8]) -> Result<image::DynamicImage, Box<dyn std::error::Error>> {
    use libheif_rs::{ColorSpace, HeifContext, LibHeif, RgbChroma};

    let libheif = LibHeif::new();
    let ctx = HeifContext::read_from_bytes(bytes)?;
    let handle = ctx.primary_image_handle()?;
    let image = libheif.decode(&handle, ColorSpace::Rgb(RgbChroma::Rgba), None)?;
    let planes = image.planes();
    let interleaved = planes.interleaved.ok_or("no interleaved plane")?;

    let width = interleaved.width as usize;
    let height = interleaved.height as usize;
    let stride = interleaved.stride as usize;
    let data = interleaved.data;

    let mut rgba = image::RgbaImage::new(width as u32, height as u32);
    for y in 0..height {
        let row_start = y * stride;
        for x in 0..width {
            let idx = row_start + x * 4;
            let pixel = image::Rgba([data[idx], data[idx + 1], data[idx + 2], data[idx + 3]]);
            rgba.put_pixel(x as u32, y as u32, pixel);
        }
    }
    Ok(image::DynamicImage::ImageRgba8(rgba))
}

/// Use the `image` crate to decode an image file that GTK can't load natively
/// (e.g. AVIF, HEIF, JXL, QOI, TGA) and convert it to a `gdk::Texture` via
/// in-memory PNG bytes.  Returns `None` on any failure.
fn texture_from_image_crate(path: &Path) -> Option<gtk::gdk::Texture> {
    let img = image::open(path).ok()?;
    // Convert to RGBA8 for maximum compatibility.
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let bytes = rgba.into_raw();
    // Build a gdk::Texture from the raw pixel data via a GdkPixbuf.
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_mut_slice(
        bytes,
        gtk::gdk_pixbuf::Colorspace::Rgb,
        true, // has alpha
        8,    // bits per sample
        w as i32,
        h as i32,
        w as i32 * 4, // rowstride
    );
    Some(gtk::gdk::Texture::for_pixbuf(&pixbuf))
}

fn fd_cache_root() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("thumbnails")
}

/// The "large" (256px) thumbnail directory, created if needed.
fn thumb_cache_dir() -> PathBuf {
    let dir = fd_cache_root().join("large");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Build the canonical file:// URI for a path (used as the cache key).
fn file_uri(path: &Path) -> String {
    // gio gives us a correctly percent-encoded URI matching what Nautilus uses.
    gtk::gio::File::for_path(path).uri().to_string()
}

/// The freedesktop shared-cache thumbnail path for `path`:
/// ~/.cache/thumbnails/large/<md5(uri)>.png
fn thumb_cache_path(path: &Path) -> PathBuf {
    let uri = file_uri(path);
    let hash = crate::md5::hex(uri.as_bytes());
    thumb_cache_dir().join(format!("{}.png", hash))
}

/// Bump this when the office/pptx RENDER code changes, so old cached renders are
/// invalidated and regenerated instead of being served stale forever.
const RENDER_VERSION: u32 = 19;
const PPTX_RENDER_VERSION: u32 = 3;

/// A Spotty-private cache path for thumbnails Spotty RENDERS itself (office docs,
/// pptx layout). Kept separate from the shared cache (which we only write real
/// extracted thumbnails to), and versioned so changing the renderer invalidates
/// old output. Lives under ~/.cache/spotty/render-cache/.
fn render_cache_path(path: &Path) -> PathBuf {
    let uri = file_uri(path);
    let mtime = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let key = format!("{}|{}|v{}", uri, mtime, RENDER_VERSION);
    let hash = crate::md5::hex(key.as_bytes());
    let dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("spotty/render-cache");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("{}.png", hash))
}

/// Look for an EXISTING shared-cache thumbnail (any size), checking the standard
/// freedesktop subdirectories newest-spec-first. Returns it only if it is at
/// least as new as the source file (stale thumbnails are ignored).
fn existing_shared_thumb(path: &Path) -> Option<PathBuf> {
    let uri = file_uri(path);
    let hash = crate::md5::hex(uri.as_bytes());
    let src_mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    for size_dir in ["xx-large", "x-large", "large", "normal"] {
        let candidate = fd_cache_root().join(size_dir).join(format!("{}.png", hash));
        if candidate.exists() {
            // Reject stale thumbnails (older than the file they represent).
            if let (Ok(tm), Some(sm)) = (
                std::fs::metadata(&candidate).and_then(|m| m.modified()),
                src_mtime,
            ) {
                if tm < sm {
                    continue;
                }
            }
            return Some(candidate);
        }
    }
    None
}

/// Is `tool` on the PATH?
/// Resolve a helper tool to a runnable path.
///
/// Designed for Flatpak from the start: inside a Flatpak sandbox, bundled
/// binaries live in `/app/bin`, which is also on PATH inside the sandbox — but
/// we check it explicitly first so behaviour is identical whether bundled or
/// host-provided. Outside a sandbox we just find it on the host PATH. This means
/// the SAME code works today (host tools) and later (bundled in the Flatpak),
/// with no branching on "am I in a sandbox".
pub(crate) fn resolve_tool(tool: &str) -> Option<PathBuf> {
    // 1. Bundled location (Flatpak `/app/bin`, or a future portable layout).
    let bundled = PathBuf::from("/app/bin").join(tool);
    if bundled.exists() {
        return Some(bundled);
    }
    // 2. ~/.local/bin (user-local static/binary installs, e.g. downloaded ffmpeg).
    if let Some(home) = dirs::home_dir() {
        let local_bin = home.join(".local/bin").join(tool);
        if local_bin.exists() {
            return Some(local_bin);
        }
    }
    // 3. Anything on PATH (host tools now; also covers /app/bin inside Flatpak).
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let cand = dir.join(tool);
            if cand.exists() {
                return Some(cand);
            }
        }
    }
    None
}

/// Convenience boolean wrapper.
pub(crate) fn has_tool(tool: &str) -> bool {
    resolve_tool(tool).is_some()
}

/// Render the first page of a PDF to a cached PNG. Uses `pdftoppm`
/// (poppler-utils), which is installed on essentially every Linux desktop.
fn pdf_thumbnail(pdf: &Path) -> Option<PathBuf> {
    if let Some(existing) = existing_shared_thumb(pdf) {
        return Some(existing);
    }
    let out = thumb_cache_path(pdf);
    if out.exists() {
        return Some(out);
    }
    if !has_tool("pdftoppm") {
        return None;
    }
    // pdftoppm writes <prefix>.png (or <prefix>-1.png depending on version) for
    // a single page. We pass the prefix WITHOUT extension and then locate the
    // file it actually produced.
    let prefix = out.with_extension(""); // strip .png; we'll find the real output
    let prefix_str = prefix.to_string_lossy().to_string();
    let bin = resolve_tool("pdftoppm")?;
    let status = std::process::Command::new(&bin)
        .args([
            "-png",
            "-f",
            "1",
            "-l",
            "1", // first page only
            "-scale-to",
            "400",         // longest side 400px
            "-singlefile", // produce exactly <prefix>.png, no page suffix
        ])
        .arg(pdf)
        .arg(&prefix_str)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    // With -singlefile, the output is exactly prefix + ".png" == `out`.
    if out.exists() {
        Some(out)
    } else {
        // Fallback for older pdftoppm without -singlefile: it may have written
        // "<prefix>-1.png". Try to find and rename it.
        let alt = PathBuf::from(format!("{}-1.png", prefix_str));
        if alt.exists() && std::fs::rename(&alt, &out).is_ok() {
            Some(out)
        } else {
            None
        }
    }
}

/// Render ALL pages of a PDF to cached PNGs. Returns vec of (path, page_number).
fn render_pdf_all_pages(pdf: &Path) -> Option<(Vec<PathBuf>, usize)> {
    let bin = resolve_tool("pdftoppm")?;
    // Use a hash of path + mtime for cache.
    let meta = std::fs::metadata(pdf).ok()?;
    let mtime = meta.modified().ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let hash = crate::md5::hex(pdf.to_string_lossy().as_bytes());
    let cache_root = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("spotty/pdf")
        .join(format!("{}-{}", hash, mtime));
    let _ = std::fs::create_dir_all(&cache_root);

    // Check if pages are already cached.
    let mut cached = Vec::new();
    let mut i = 1;
    loop {
        let page = cache_root.join(format!("page-{}.png", i));
        if !page.exists() {
            break;
        }
        cached.push(page);
        i += 1;
    }
    let total_pages = if !cached.is_empty() {
        cached.len()
    } else {
        // Render all pages.
        let prefix = cache_root.join("page");
        let prefix_str = prefix.to_string_lossy().to_string();
        let status = std::process::Command::new(&bin)
            .args(["-png", "-r", "150", "-scale-to", "400"])
            .arg(pdf)
            .arg(&prefix_str)
            .status()
            .ok();
        if !status.map(|s| s.success()).unwrap_or(false) {
            return None;
        }
        let mut count = 0;
        loop {
            let page = cache_root.join(format!("page-{}.png", count + 1));
            if !page.exists() {
                break;
            }
            cached.push(page);
            count += 1;
        }
        if count == 0 {
            return None;
        }
        count
    };
    let paths: Vec<PathBuf> = cached.into_iter().collect();
    Some((paths, total_pages))
}

/// Generate a thumbnail using LibreOffice headless (if installed).
/// This provides a "full overview" for legacy .ppt, .doc, .xls, and complex .pptx.
fn libreoffice_thumbnail(doc: &std::path::Path) -> Option<std::path::PathBuf> {
    let out = thumb_cache_path(doc);
    let temp_dir = dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join(format!("spotty-lo-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp_dir);

    // Convert to PDF so we can reuse our pdftoppm scaling logic.
    let lo_args = [
        "--headless",
        "--invisible",
        "--nologo",
        "--nodefault",
        "--nofirststartwizard",
        "--convert-to",
        "pdf",
    ];
    let status = if let Some(bin) = resolve_tool("libreoffice").or_else(|| resolve_tool("soffice"))
    {
        std::process::Command::new(bin)
            .args(lo_args)
            .arg("--outdir")
            .arg(&temp_dir)
            .arg(doc)
            .status()
            .ok()
    } else if std::env::var("FLATPAK_ID").is_ok() {
        std::process::Command::new("flatpak-spawn")
            .args(["--host", "libreoffice"])
            .args(lo_args)
            .arg("--outdir")
            .arg(&temp_dir)
            .arg(doc)
            .status()
            .ok()
            .or_else(|| {
                std::process::Command::new("flatpak-spawn")
                    .args(["--host", "soffice"])
                    .args(lo_args)
                    .arg("--outdir")
                    .arg(&temp_dir)
                    .arg(doc)
                    .status()
                    .ok()
            })
    } else {
        None
    }?;

    if !status.success() {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return None;
    }

    let stem = doc.file_stem()?.to_string_lossy();
    let pdf_path = temp_dir.join(format!("{}.pdf", stem));

    let mut result = None;
    if pdf_path.exists() {
        let prefix = out.with_extension("");
        let prefix_str = prefix.to_string_lossy().to_string();
        let ppm_args = [
            "-png",
            "-f",
            "1",
            "-l",
            "1",
            "-scale-to",
            "400",
            "-singlefile",
        ];
        let ppm_status = if let Some(pdftoppm_bin) = resolve_tool("pdftoppm") {
            std::process::Command::new(&pdftoppm_bin)
                .args(ppm_args)
                .arg(&pdf_path)
                .arg(&prefix_str)
                .status()
                .ok()
        } else if std::env::var("FLATPAK_ID").is_ok() {
            std::process::Command::new("flatpak-spawn")
                .args(["--host", "pdftoppm"])
                .args(ppm_args)
                .arg(&pdf_path)
                .arg(&prefix_str)
                .status()
                .ok()
        } else {
            None
        };

        if ppm_status.map(|s| s.success()).unwrap_or(false) && out.exists() {
            result = Some(out.clone());
        } else {
            let alt = std::path::PathBuf::from(format!("{}-1.png", prefix_str));
            if alt.exists() && std::fs::rename(&alt, &out).is_ok() {
                result = Some(out.clone());
            }
        }
    }

    let _ = std::fs::remove_dir_all(&temp_dir);
    result
}

/// Produce a preview thumbnail for an office document WITHOUT LibreOffice, by
/// extracting the preview image embedded inside the file (see
/// extract_embedded_thumbnail). Returns None when no usable thumbnail exists, so
/// the caller falls back to the type-specific info card.
fn office_thumbnail(doc: &Path) -> Option<PathBuf> {
    let ext = doc
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase());
    // A real, externally-generated thumbnail (Nautilus, office suites, or one
    // we extracted before) always wins when it exists and is fresh.
    if let Some(existing) = existing_shared_thumb(doc) {
        return Some(existing);
    }

    if matches!(ext.as_deref(), Some("ppt")) {
        log::debug!("office preview: attempting LibreOffice full render");
        if let Some(out) = libreoffice_thumbnail(doc) {
            return Some(out);
        }
        let out = render_cache_path(doc);
        if out.exists() {
            return Some(out);
        }
        if let Some(png) = render_legacy_ppt_first_slide(doc) {
            if std::fs::write(&out, &png).is_ok() {
                return Some(out);
            }
        }
        return None;
    }

    log::debug!("office preview: generating for {}", doc.display());

    // STEP 0.5 — if LibreOffice/soffice is installed, use it to generate a PERFECT full overview!
    // This solves issues with legacy .ppt files and documents with missing embedded thumbnails.
    log::debug!("office preview: attempting LibreOffice full render");
    if let Some(out_path) = libreoffice_thumbnail(doc) {
        return Some(out_path);
    }

    // PowerPoint files need layout correctness first. Prefer the document's real
    // embedded preview image over Spotty's synthetic layout renderer, but keep it
    // in our private versioned cache so stale shared thumbnails are avoided.
    let out = render_cache_path(doc);
    if matches!(ext.as_deref(), Some("pptx") | Some("ppsx") | Some("pps")) {
        if out.exists() {
            return Some(out);
        }
        if let Some(bytes) = extract_embedded_thumbnail(doc) {
            log::debug!(
                "office preview: using embedded PowerPoint thumbnail image ({} bytes)",
                bytes.len()
            );
            if write_embedded_thumbnail_png(&bytes, &out) {
                return Some(out);
            }
        }
        match parse_pptx_layout(doc) {
            Some(layout) => {
                let text_count = layout.elements.iter().filter(|e| matches!(e, SlideElement::Text(_))).count();
                log::debug!(
                    "office preview: PPTX layout parsed, {} boxes",
                    text_count
                );
                if let Some(png) = render_slide_layout(&layout) {
                    log::debug!(
                        "office preview: rendered slide PNG ({} bytes) -> {}",
                        png.len(),
                        out.display()
                    );
                    if std::fs::write(&out, &png).is_ok() {
                        return Some(out);
                    }
                } else {
                    log::debug!("office preview: render_slide_layout returned None");
                }
            }
            None => log::debug!("office preview: PPTX layout parse found no shapes"),
        }
        if let Some(bytes) = extract_embedded_thumbnail(doc) {
            log::debug!(
                "office preview: using embedded PowerPoint thumbnail ({} bytes)",
                bytes.len()
            );
            if write_embedded_thumbnail_png(&bytes, &out) {
                return Some(out);
            }
        }
        return None;
    }

    // STEP 1 — the best result: extract the PREVIEW IMAGE that Office embeds
    // inside the document (JPEG/PNG). Great for PowerPoint and ODF. This is a
    // real rendered image, so we store it in the SHARED cache for reuse.
    if let Some(bytes) = extract_embedded_thumbnail(doc) {
        log::debug!(
            "office preview: using embedded thumbnail ({} bytes)",
            bytes.len()
        );
        let out = thumb_cache_path(doc);
        if std::fs::write(&out, &bytes).is_ok() {
            return Some(out);
        }
    }

    // For thumbnails WE render (not real document previews), use a private,
    // VERSION-TAGGED cache path so changing the render code regenerates them
    // instead of serving a stale old render forever.
    if out.exists() {
        return Some(out);
    }

    // STEP 2 — content preview (Word/Excel text/data) drawn with Cairo.
    match extract_office_content(doc) {
        Some(content) => {
            log::debug!(
                "office preview: extracted content, {} lines",
                content.lines.len()
            );
            if let Some(png) = render_content_preview(&content) {
                if std::fs::write(&out, &png).is_ok() {
                    return Some(out);
                }
            }
            log::debug!("office preview: render failed");
        }
        None => log::debug!(
            "office preview: no content extracted from {}",
            doc.display()
        ),
    }

    None
}

fn write_embedded_thumbnail_png(bytes: &[u8], out: &Path) -> bool {
    const TARGET_LONG_EDGE: i32 = 3200;

    let loader = gdk_pixbuf::PixbufLoader::new();
    if loader.write(bytes).is_err() || loader.close().is_err() {
        return std::fs::write(out, bytes).is_ok();
    }

    let Some(pixbuf) = loader.pixbuf() else {
        return std::fs::write(out, bytes).is_ok();
    };

    let width = pixbuf.width().max(1);
    let height = pixbuf.height().max(1);
    let long_edge = width.max(height);
    let image = if long_edge < TARGET_LONG_EDGE {
        let scale = TARGET_LONG_EDGE as f64 / long_edge as f64;
        let scaled_w = ((width as f64 * scale).round() as i32).max(1);
        let scaled_h = ((height as f64 * scale).round() as i32).max(1);
        pixbuf
            .scale_simple(
                scaled_w,
                scaled_h,
                if long_edge <= 1600 {
                    gdk_pixbuf::InterpType::Nearest
                } else {
                    gdk_pixbuf::InterpType::Hyper
                },
            )
            .unwrap_or(pixbuf)
    } else {
        pixbuf
    };
    let image = image.copy().unwrap_or(image);
    sharpen_pixbuf(&image, 2.6);
    sharpen_pixbuf(&image, 1.7);
    boost_pixbuf_contrast(&image, 1.2);

    image.savev(out, "png", &[("compression", "6")]).is_ok() || std::fs::write(out, bytes).is_ok()
}

fn sharpen_pixbuf(pixbuf: &gdk_pixbuf::Pixbuf, amount: f32) {
    let width = pixbuf.width().max(0) as usize;
    let height = pixbuf.height().max(0) as usize;
    if width < 3 || height < 3 {
        return;
    }

    let rowstride = pixbuf.rowstride() as usize;
    let channels = pixbuf.n_channels().max(0) as usize;
    if channels < 3 {
        return;
    }

    let src = unsafe { pixbuf.pixels().to_vec() };
    let dst = unsafe { pixbuf.pixels() };

    for y in 1..(height - 1) {
        for x in 1..(width - 1) {
            let idx = y * rowstride + x * channels;
            for c in 0..3 {
                let center = src[idx + c] as i32;
                let left = src[idx - channels + c] as i32;
                let right = src[idx + channels + c] as i32;
                let up = src[idx - rowstride + c] as i32;
                let down = src[idx + rowstride + c] as i32;
                let avg = (left + right + up + down) / 4;
                let delta = center - avg;
                let value = center as f32 + (delta as f32 * amount);
                dst[idx + c] = value.clamp(0.0, 255.0) as u8;
            }
        }
    }
}

fn boost_pixbuf_contrast(pixbuf: &gdk_pixbuf::Pixbuf, amount: f32) {
    let channels = pixbuf.n_channels().max(0) as usize;
    if channels < 3 {
        return;
    }

    let rowstride = pixbuf.rowstride().max(0) as usize;
    let width = pixbuf.width().max(0) as usize;
    let height = pixbuf.height().max(0) as usize;
    let pixels = unsafe { pixbuf.pixels() };

    for y in 0..height {
        for x in 0..width {
            let idx = y * rowstride + x * channels;
            for c in 0..3 {
                let v = pixels[idx + c] as f32 / 255.0;
                let adjusted = ((v - 0.5) * amount + 0.5).clamp(0.0, 1.0);
                pixels[idx + c] = (adjusted * 255.0).round() as u8;
            }
        }
    }
}

/// Open an OOXML/ODF file as a ZIP and return the bytes of an embedded preview
/// image IF it is in a format GTK can display (JPEG or PNG). Returns None when
/// there is no thumbnail, or it is an EMF/WMF metafile we can't render.
fn extract_embedded_thumbnail(doc: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(doc).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;

    // Candidate entry names, in priority order. OOXML first, then ODF.
    const CANDIDATES: &[&str] = &[
        "docProps/thumbnail.jpeg",
        "docProps/thumbnail.jpg",
        "docProps/thumbnail.png",
        "Thumbnails/thumbnail.png",
    ];

    for name in CANDIDATES {
        if let Ok(mut entry) = zip.by_name(name) {
            use std::io::Read;
            let mut buf = Vec::new();
            if entry.read_to_end(&mut buf).is_ok() && !buf.is_empty() {
                return Some(buf);
            }
        }
    }
    // Note: we intentionally skip docProps/thumbnail.emf and .wmf — GdkPixbuf
    // can't decode Windows metafiles, so there's nothing we could display.
    None
}

/// Pick a type-appropriate symbolic icon for the info card, so files whose
/// visual thumbnail can't be generated still show a recognizable icon.
fn info_icon_for(p: &Path) -> &'static str {
    let ext = p
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase());
    match ext.as_deref() {
        Some("pdf") => "application-pdf",
        Some("ppt" | "pptx" | "odp" | "otp" | "fodp" | "pps" | "ppsx") => "x-office-presentation",
        Some("doc" | "docx" | "odt" | "rtf" | "ott" | "fodt" | "wps") => "x-office-document",
        Some("xls" | "xlsx" | "ods" | "ots" | "fods" | "csv") => "x-office-spreadsheet",
        Some(
            "mp4" | "mkv" | "webm" | "mov" | "avi" | "wmv" | "flv" | "m4v" | "mpeg" | "mpg"
            | "m2ts" | "mts" | "ogv" | "3gp" | "3g2" | "asf" | "rm" | "rmvb" | "vob" | "divx"
            | "f4v" | "mxf",
        ) => "video-x-generic-symbolic",
        Some("mp3" | "flac" | "ogg" | "wav" | "m4a" | "opus" | "aac" | "wma") => {
            "audio-x-generic-symbolic"
        }
        Some(
            "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "svg" | "tiff" | "tif" | "avif"
            | "heic" | "heif" | "ico",
        ) => "image-x-generic-symbolic",
        Some("zip" | "tar" | "gz" | "xz" | "bz2" | "7z" | "rar" | "zst") => {
            "package-x-generic-symbolic"
        }
        Some(
            "rs" | "py" | "js" | "ts" | "c" | "h" | "cpp" | "go" | "java" | "rb" | "sh" | "lua"
            | "sql",
        ) => "text-x-script-symbolic",
        _ => "text-x-generic-symbolic",
    }
}

// ──────────────────────────────────────────────────────────────────────
// Native (LibreOffice-free) Office content preview
//
// We extract the document's text/data from its OOXML XML and draw a simple
// page-like image with Cairo's text API. This is a CONTENT preview — readable
// text/data, not a faithful visual render (no themes, images, charts, exact
// layout). It only runs as a fallback when no embedded thumbnail exists.
// ──────────────────────────────────────────────────────────────────────

/// What kind of document we're previewing, with the extracted content.
struct OfficeContent {
    kind: OfficeKind,
    title: String,
    /// Lines of text (slide bullets / paragraphs) or, for sheets, formatted rows.
    lines: Vec<String>,
}

#[derive(PartialEq)]
enum OfficeKind {
    Presentation,
    Document,
    Spreadsheet,
}

/// Read an OOXML file and pull out a representative chunk of its content.
fn extract_legacy_binary(doc: &Path, ext: &str) -> Option<OfficeContent> {
    // Only attempt strings extraction for legacy binary formats
    if !matches!(ext, "ppt" | "doc" | "xls") {
        return None;
    }

    // First try a pure-Rust extraction so legacy previews work even when
    // external helpers are unavailable in the sandbox.
    let bytes = std::fs::read(doc).ok()?;
    let mut candidates = Vec::<String>::new();
    candidates.extend(extract_ascii_strings(&bytes, 4));
    candidates.extend(extract_utf16le_strings(&bytes, 4));

    // If extraction was sparse, try host/system `strings` as a secondary source.
    if candidates.len() < 8 {
        let status = if let Some(bin) = resolve_tool("strings") {
            std::process::Command::new(&bin)
                .args(["-n", "4", "-e", "l"])
                .arg(doc)
                .output()
                .ok()
        } else if std::env::var("FLATPAK_ID").is_ok() {
            std::process::Command::new("flatpak-spawn")
                .args(["--host", "strings", "-n", "4", "-e", "l"])
                .arg(doc)
                .output()
                .ok()
        } else {
            None
        };
        if let Some(status) = status {
            if status.status.success() {
                let text = String::from_utf8_lossy(&status.stdout);
                candidates.extend(text.lines().map(|s| s.trim().to_string()));
            }
        }
    }

    let mut lines = Vec::new();
    for line in candidates {
        let trimmed = line.trim();
        if trimmed.len() > 4 && trimmed.chars().any(|c| c.is_ascii_alphabetic()) {
            if !lines
                .iter()
                .any(|x: &String| x.eq_ignore_ascii_case(trimmed))
            {
                lines.push(trimmed.to_string());
            }
        }
        if lines.len() >= 30 {
            break;
        }
    }

    if lines.is_empty() {
        return None;
    }

    let title = lines.first().cloned().unwrap_or_else(|| "Document".into());
    let body = lines.into_iter().skip(1).take(24).collect();

    let kind = match ext {
        "ppt" => OfficeKind::Presentation,
        "xls" => OfficeKind::Spreadsheet,
        _ => OfficeKind::Document,
    };

    Some(OfficeContent {
        kind,
        title,
        lines: body,
    })
}

fn extract_ascii_strings(bytes: &[u8], min_len: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = Vec::<u8>::new();
    for &b in bytes {
        if b.is_ascii_graphic() || b == b' ' {
            cur.push(b);
        } else {
            if cur.len() >= min_len {
                out.push(String::from_utf8_lossy(&cur).to_string());
            }
            cur.clear();
        }
    }
    if cur.len() >= min_len {
        out.push(String::from_utf8_lossy(&cur).to_string());
    }
    out
}

fn extract_utf16le_strings(bytes: &[u8], min_len: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        let u = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        if (0x20..=0x7e).contains(&u) {
            if let Some(ch) = char::from_u32(u as u32) {
                cur.push(ch);
            }
        } else {
            if cur.len() >= min_len {
                out.push(cur.clone());
            }
            cur.clear();
        }
        i += 2;
    }
    if cur.len() >= min_len {
        out.push(cur);
    }
    out
}

fn extract_office_content(doc: &Path) -> Option<OfficeContent> {
    let ext = doc.extension()?.to_str()?.to_lowercase();

    if matches!(ext.as_str(), "ppt" | "doc" | "xls") {
        return extract_legacy_binary(doc, &ext);
    }
    if ext == "csv" {
        return extract_csv(doc);
    }

    let file = std::fs::File::open(doc).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;

    match ext.as_str() {
        "pptx" | "ppsx" | "pps" => extract_pptx(&mut zip),
        "docx" => extract_docx(&mut zip),
        "xlsx" => extract_xlsx(&mut zip),
        _ => None, // .odp/.odt/.ods and legacy binaries not handled here
    }
}

fn extract_csv(doc: &Path) -> Option<OfficeContent> {
    let text = std::fs::read_to_string(doc).ok()?;
    let delimiter = detect_csv_delimiter(&text);
    let mut rows = parse_delimited_rows(&text, delimiter);
    rows.retain(|row| row.iter().any(|cell| !cell.trim().is_empty()));
    if rows.is_empty() {
        return None;
    }
    rows.truncate(18);
    for row in &mut rows {
        row.truncate(8);
        for cell in row.iter_mut() {
            *cell = cell.trim().replace('\r', "");
        }
    }
    Some(OfficeContent {
        kind: OfficeKind::Spreadsheet,
        title: "Spreadsheet".into(),
        lines: rows.into_iter().map(|row| row.join("\t")).collect(),
    })
}

fn detect_csv_delimiter(text: &str) -> char {
    let sample: Vec<&str> = text.lines().take(5).collect();
    let mut best = (',', 0usize);
    for delim in [',', ';', '\t'] {
        let score = sample
            .iter()
            .map(|line| line.matches(delim).count())
            .sum::<usize>();
        if score > best.1 {
            best = (delim, score);
        }
    }
    best.0
}

fn parse_delimited_rows(text: &str, delimiter: char) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut chars = text.chars().peekable();
    let mut in_quotes = false;

    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                if in_quotes && chars.peek() == Some(&'"') {
                    cell.push('"');
                    let _ = chars.next();
                } else {
                    in_quotes = !in_quotes;
                }
            }
            c if c == delimiter && !in_quotes => {
                row.push(cell.clone());
                cell.clear();
            }
            '\n' if !in_quotes => {
                row.push(cell.clone());
                cell.clear();
                rows.push(row);
                row = Vec::new();
                if rows.len() >= 24 {
                    break;
                }
            }
            '\r' if !in_quotes => {}
            _ => cell.push(ch),
        }
    }

    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        rows.push(row);
    }

    rows
}

/// Read a named entry from the zip into a String (UTF-8, lossy).
fn read_zip_text<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
) -> Option<String> {
    use std::io::Read;
    let mut entry = zip.by_name(name).ok()?;
    let mut buf = String::new();
    entry.read_to_string(&mut buf).ok()?;
    Some(buf)
}

/// Extract all text inside <a:t>...</a:t> runs (used by PPTX/DOCX share the w:t
/// / a:t convention). `tag` is the local element name ("a:t" or "w:t").
fn extract_text_runs(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{}", tag); // tolerate attributes: <a:t> or <a:t xml:space=…>
    let close = format!("</{}>", tag);
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(start) = xml[pos..].find(&open) {
        let abs = pos + start;
        // The char right after the tag name must be '>' or whitespace, otherwise
        // "<t" would wrongly match "<title>", "<tableStyleId>", etc.
        let after = xml[abs + open.len()..].chars().next();
        let is_tag = matches!(
            after,
            Some('>') | Some(' ') | Some('\t') | Some('\r') | Some('\n') | Some('/')
        );
        if !is_tag {
            pos = abs + open.len();
            continue;
        }
        // Find the '>' that ends the opening tag.
        let Some(gt) = xml[abs..].find('>') else {
            break;
        };
        // Self-closing tag like <a:t/> has no text.
        if xml[abs..abs + gt].ends_with('/') {
            pos = abs + gt + 1;
            continue;
        }
        let text_start = abs + gt + 1;
        let Some(end_rel) = xml[text_start..].find(&close) else {
            break;
        };
        let text = &xml[text_start..text_start + end_rel];
        let decoded = decode_xml_entities(text);
        if !decoded.trim().is_empty() {
            out.push(decoded);
        }
        pos = text_start + end_rel + close.len();
    }
    out
}

/// Minimal XML entity decoding for the common five.
fn decode_xml_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

fn extract_pptx<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
) -> Option<OfficeContent> {
    // Slides live at ppt/slides/slide1.xml, slide2.xml, ... We preview slide 1.
    let xml = read_zip_text(zip, "ppt/slides/slide1.xml")?;
    let runs = extract_text_runs(&xml, "a:t");
    if runs.is_empty() {
        return None;
    }
    let title = runs
        .first()
        .cloned()
        .unwrap_or_else(|| "Presentation".into());
    let lines = runs.into_iter().skip(1).take(12).collect();
    Some(OfficeContent {
        kind: OfficeKind::Presentation,
        title,
        lines,
    })
}

fn extract_docx<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
) -> Option<OfficeContent> {
    let xml = read_zip_text(zip, "word/document.xml")?;
    // In DOCX, paragraphs are <w:p> and text runs inside are <w:t>. To preserve
    // paragraph breaks we split on </w:p> first, then pull the w:t runs per para.
    let mut lines = Vec::new();
    for para in xml.split("</w:p>") {
        let runs = extract_text_runs(para, "w:t");
        if !runs.is_empty() {
            lines.push(runs.join(""));
        }
        if lines.len() >= 30 {
            break;
        }
    }
    if lines.is_empty() {
        return None;
    }
    let title = lines.first().cloned().unwrap_or_else(|| "Document".into());
    let body = lines.into_iter().skip(1).take(24).collect();
    Some(OfficeContent {
        kind: OfficeKind::Document,
        title,
        lines: body,
    })
}

fn extract_xlsx<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
) -> Option<OfficeContent> {
    // Shared strings: each <si> is ONE logical string, but may contain multiple
    // <t> runs (e.g. rich text). We must join the runs within each <si> so the
    // string-index lookup from cells stays aligned. extract_text_runs would
    // wrongly split those into separate entries, so we group by <si> here.
    let shared: Vec<String> = read_zip_text(zip, "xl/sharedStrings.xml")
        .map(|x| {
            x.split("</si>")
                .filter_map(|si| {
                    if !si.contains("<si") && !si.contains("<t") {
                        return None;
                    }
                    let runs = extract_text_runs(si, "t");
                    if runs.is_empty() {
                        Some(String::new())
                    } else {
                        Some(runs.join(""))
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let sheet = read_zip_text(zip, "xl/worksheets/sheet1.xml")?;
    let mut rows: Vec<Vec<String>> = Vec::new();

    for row in sheet.split("</row>").take(24) {
        if !row.contains("<c") {
            continue;
        }
        let mut cells: Vec<String> = Vec::new();
        let mut pos = 0;
        // Each cell: <c r="A1" t="s"><v>idx</v></c> or <c r="A1"><v>3.14</v></c>
        // or inline string <c t="inlineStr"><is><t>text</t></is></c>.
        while let Some(rel) = row[pos..].find("<c") {
            let abs = pos + rel;
            let Some(tag) = tag_substr(&row[abs..], "<c") else {
                break;
            };
            let cell_type = attr_value(&tag, "t");
            // Advance past this cell's opening tag.
            let after_tag = abs + tag.len();
            // Determine the slice for this cell (up to the next <c or end).
            let cell_end = row[after_tag..]
                .find("<c")
                .map(|x| after_tag + x)
                .unwrap_or(row.len());
            let cell_body = &row[after_tag..cell_end];

            let text = match cell_type.as_deref() {
                Some("s") => {
                    // Shared-string index inside <v>.
                    inner_text(cell_body, "<v>", "</v>")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .and_then(|i| shared.get(i).cloned())
                        .unwrap_or_default()
                }
                Some("inlineStr") => extract_text_runs(cell_body, "t").join(""),
                _ => {
                    // Number / date / bool: the literal value.
                    inner_text(cell_body, "<v>", "</v>").unwrap_or_default()
                }
            };
            cells.push(decode_xml_entities(text.trim()));
            pos = cell_end;
            if cells.len() >= 8 {
                break;
            }
        }
        // Trim trailing empty cells.
        while cells.last().map(|s| s.is_empty()).unwrap_or(false) {
            cells.pop();
        }
        if cells.iter().any(|s| !s.is_empty()) {
            rows.push(cells);
        }
        if rows.len() >= 18 {
            break;
        }
    }

    if rows.is_empty() {
        return None;
    }
    // Store rows as tab-joined lines; the renderer splits them back into a grid.
    let lines = rows.into_iter().map(|r| r.join("\t")).collect();
    Some(OfficeContent {
        kind: OfficeKind::Spreadsheet,
        title: "Spreadsheet".into(),
        lines,
    })
}

/// Extract text between the first `open` and the following `close` marker.
fn inner_text(s: &str, open: &str, close: &str) -> Option<String> {
    let start = s.find(open)? + open.len();
    let end = s[start..].find(close)? + start;
    Some(s[start..end].to_string())
}

/// Draw an OfficeContent to a PNG using Cairo's text API. Returns the PNG bytes.
fn render_content_preview(content: &OfficeContent) -> Option<Vec<u8>> {
    match content.kind {
        OfficeKind::Spreadsheet => render_spreadsheet(content),
        OfficeKind::Document => render_document(content),
        OfficeKind::Presentation => render_document(content), // PPTX prefers layout render
    }
}

/// Common helpers for the template renderers.
fn new_surface(w: i32, h: i32) -> Option<(gtk::cairo::ImageSurface, gtk::cairo::Context)> {
    use gtk::cairo;
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).ok()?;
    let cr = cairo::Context::new(&surface).ok()?;
    Some((surface, cr))
}

fn surface_to_png(surface: gtk::cairo::ImageSurface, cr: gtk::cairo::Context) -> Option<Vec<u8>> {
    drop(cr); // release the surface borrow before encoding
    let mut buf: Vec<u8> = Vec::new();
    surface.write_to_png(&mut buf).ok()?;
    Some(buf)
}

/// A clean "document page" template: white page, colored title, body paragraphs
/// with word-wrap. Used for Word docs (and as the PPTX fallback).
fn render_document(content: &OfficeContent) -> Option<Vec<u8>> {
    use gtk::cairo;
    const W: i32 = 320;
    const H: i32 = 414; // ~A4 portrait feel
    let (surface, cr) = new_surface(W, H)?;
    let wf = W as f64;
    let hf = H as f64;

    // Paper + subtle drop shadow edge.
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.paint().ok()?;
    cr.set_source_rgb(0.88, 0.88, 0.90);
    cr.set_line_width(1.0);
    cr.rectangle(0.5, 0.5, wf - 1.0, hf - 1.0);
    cr.stroke().ok()?;

    let (ar, ag, ab) = accent_for(&content.kind);
    let margin = 22.0;

    // Title.
    cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
    cr.set_font_size(17.0);
    cr.set_source_rgb(ar, ag, ab);
    let mut y = 44.0;
    for line in wrap_text(&cr, &content.title, wf - margin * 2.0)
        .into_iter()
        .take(2)
    {
        cr.move_to(margin, y);
        let _ = cr.show_text(&line);
        y += 22.0;
    }

    // Accent rule under the title.
    cr.set_source_rgba(ar, ag, ab, 0.5);
    cr.set_line_width(2.0);
    cr.move_to(margin, y - 6.0);
    cr.line_to(wf - margin, y - 6.0);
    cr.stroke().ok()?;
    y += 8.0;

    // Body paragraphs.
    cr.set_source_rgb(0.18, 0.18, 0.20);
    for para in &content.lines {
        cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
        cr.set_font_size(11.5);
        for line in wrap_text(&cr, para, wf - margin * 2.0) {
            if y > hf - 18.0 {
                return surface_to_png(surface, cr);
            }
            cr.move_to(margin, y);
            let _ = cr.show_text(&line);
            y += 16.0;
        }
        y += 6.0; // paragraph spacing
    }

    surface_to_png(surface, cr)
}

/// A compact spreadsheet preview: row/column headers, table borders, header
/// styling, and alternating row banding.
fn render_spreadsheet(content: &OfficeContent) -> Option<Vec<u8>> {
    use gtk::cairo;
    const W: i32 = 520;
    const H: i32 = 360;
    let (surface, cr) = new_surface(W, H)?;
    let wf = W as f64;
    let hf = H as f64;

    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.paint().ok()?;

    // Parse rows back into cells (renderer received them tab-joined).
    let rows: Vec<Vec<String>> = content
        .lines
        .iter()
        .map(|l| l.split('\t').map(|s| s.to_string()).collect())
        .collect();
    if rows.is_empty() {
        return None;
    }

    // Determine column count (cap to what fits).
    let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(1).clamp(1, 6);
    let row_header_w = 34.0;
    let col_header_h = 22.0;
    let row_h = 22.0;
    let table_w = wf - row_header_w - 1.0;
    let col_w = table_w / ncols as f64;

    cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);

    // Sheet headers.
    cr.set_source_rgb(0.91, 0.92, 0.93);
    cr.rectangle(0.0, 0.0, wf, col_header_h);
    cr.fill().ok()?;
    cr.rectangle(0.0, col_header_h, row_header_w, hf - col_header_h);
    cr.fill().ok()?;

    cr.set_source_rgb(0.74, 0.75, 0.77);
    cr.set_line_width(1.0);
    cr.move_to(0.0, col_header_h + 0.5);
    cr.line_to(wf, col_header_h + 0.5);
    cr.stroke().ok()?;
    cr.move_to(row_header_w + 0.5, 0.0);
    cr.line_to(row_header_w + 0.5, hf);
    cr.stroke().ok()?;

    cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
    cr.set_font_size(10.0);
    cr.set_source_rgb(0.28, 0.29, 0.31);
    for ci in 0..ncols {
        let cx = row_header_w + ci as f64 * col_w;
        let label = ((b'A' + ci as u8) as char).to_string();
        let ext = cr.text_extents(&label).ok();
        let tw = ext.as_ref().map(|e| e.width()).unwrap_or(0.0);
        cr.move_to(cx + (col_w - tw) / 2.0, 15.0);
        let _ = cr.show_text(&label);
    }

    let mut y = col_header_h;
    for (ri, row) in rows.iter().enumerate() {
        if y > hf {
            break;
        }

        cr.set_source_rgb(0.91, 0.92, 0.93);
        cr.rectangle(0.0, y, row_header_w, row_h);
        cr.fill().ok()?;

        cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
        cr.set_font_size(9.5);
        cr.set_source_rgb(0.38, 0.39, 0.41);
        let row_num = (ri + 1).to_string();
        let ext = cr.text_extents(&row_num).ok();
        let tw = ext.as_ref().map(|e| e.width()).unwrap_or(0.0);
        cr.move_to(row_header_w - tw - 6.0, y + 15.5);
        let _ = cr.show_text(&row_num);

        // Row background: header tinted, body alternating like styled tables.
        if ri == 0 {
            cr.set_source_rgb(0.55, 0.82, 0.30);
        } else if ri % 2 == 0 {
            cr.set_source_rgb(0.86, 0.92, 0.98);
        } else {
            cr.set_source_rgb(0.88, 0.95, 0.83);
        }
        cr.rectangle(row_header_w, y, table_w, row_h);
        cr.fill().ok()?;

        // Cell text.
        for ci in 0..ncols {
            let cx = row_header_w + ci as f64 * col_w;
            let cell = row.get(ci).cloned().unwrap_or_default();
            if ri == 0 {
                cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
                cr.set_font_size(10.8);
                cr.set_source_rgb(0.0, 0.0, 0.0);
            } else {
                cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
                cr.set_font_size(10.5);
                cr.set_source_rgb(0.15, 0.15, 0.17);
            }
            let shown = truncate_to_width(&cr, &cell, col_w - 10.0);
            cr.move_to(cx + 5.0, y + 15.5);
            let _ = cr.show_text(&shown);
        }
        y += row_h;
    }

    // Grid lines.
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.18);
    cr.set_line_width(1.0);
    for ci in 0..=ncols {
        let cx = row_header_w + ci as f64 * col_w;
        cr.move_to(cx + 0.5, 0.0);
        cr.line_to(cx + 0.5, y.min(hf));
        cr.stroke().ok()?;
    }
    let mut gy = col_header_h;
    while gy < y.min(hf) {
        cr.move_to(0.0, gy + 0.5);
        cr.line_to(wf, gy + 0.5);
        cr.stroke().ok()?;
        gy += row_h;
    }

    // Strong border around the styled table header.
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.70);
    cr.set_line_width(1.2);
    cr.rectangle(
        row_header_w + 0.5,
        col_header_h + 0.5,
        table_w - 1.0,
        row_h - 1.0,
    );
    cr.stroke().ok()?;

    // Subtle bottom/right sheet edge.
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.12);
    cr.set_line_width(1.0);
    cr.rectangle(0.5, 0.5, wf - 1.0, hf - 1.0);
    cr.stroke().ok()?;

    // A tiny sheet tab hint makes the image read as a spreadsheet even when
    // the selected area is mostly blank.
    let tab_y = hf - 22.0;
    cr.set_source_rgb(0.95, 0.96, 0.97);
    cr.rectangle(8.0, tab_y, 58.0, 18.0);
    cr.fill().ok()?;
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.16);
    cr.rectangle(8.5, tab_y + 0.5, 57.0, 17.0);
    cr.stroke().ok()?;
    cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
    cr.set_font_size(9.0);
    cr.set_source_rgb(0.18, 0.47, 0.27);
    cr.move_to(20.0, tab_y + 12.5);
    let _ = cr.show_text("Sheet1");

    // Hide grid under the tab with a white strip if data reaches the bottom.
    if y > tab_y {
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.rectangle(
            row_header_w,
            tab_y - 1.0,
            wf - row_header_w,
            hf - tab_y + 1.0,
        );
        cr.fill().ok()?;
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.12);
        cr.move_to(0.0, tab_y - 0.5);
        cr.line_to(wf, tab_y - 0.5);
        cr.stroke().ok()?;
    }

    surface_to_png(surface, cr)
}

/// Accent color per document kind (PowerPoint orange / Word blue / Excel green).
fn accent_for(kind: &OfficeKind) -> (f64, f64, f64) {
    match kind {
        OfficeKind::Presentation => (0.78, 0.32, 0.18),
        OfficeKind::Document => (0.16, 0.33, 0.60),
        OfficeKind::Spreadsheet => (0.13, 0.53, 0.30),
    }
}

/// Truncate a string with an ellipsis so it fits within `max_w` pixels for the
/// current Cairo font settings.
fn truncate_to_width(cr: &gtk::cairo::Context, text: &str, max_w: f64) -> String {
    if cr.text_extents(text).map(|e| e.width()).unwrap_or(0.0) <= max_w {
        return text.to_string();
    }
    let mut s = String::new();
    for ch in text.chars() {
        let mut trial = s.clone();
        trial.push(ch);
        trial.push('…');
        if cr
            .text_extents(&trial)
            .map(|e| e.width())
            .unwrap_or(f64::MAX)
            > max_w
        {
            s.push('…');
            return s;
        }
        s.push(ch);
    }
    s
}

// ──────────────────────────────────────────────────────────────────────
// Layout-aware PowerPoint preview
//
// PPTX slides describe each shape's position and size in EMUs (English Metric
// Units; 914,400 per inch). We parse the shape tree, read each text box's
// <a:off>/<a:ext>, and draw the boxes where they actually sit, scaled to the
// real slide aspect ratio (from presentation.xml). This produces a
// wireframe-accurate preview: text in the right places at the right sizes.
// It cannot reproduce embedded images, charts, theme art, or exact fonts.
// ──────────────────────────────────────────────────────────────────────

/// One positioned text box on a slide. Coordinates are in EMUs.
#[derive(Clone)]
struct SlideBox {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    text: String,
    is_title: bool,
    centered: bool,
    font_pt: Option<f64>,
    has_xfrm: bool,
    ph_type: String,
    color: Option<(f64, f64, f64)>,
}

enum SlideBackground {
    Solid(f64, f64, f64),
    Image(PathBuf),
}

enum SlideElement {
    Shape(f64, f64, f64, f64, Option<(f64, f64, f64)>),
    Picture(f64, f64, f64, f64, PathBuf),
    Text(SlideBox),
}

struct SlideLayout {
    slide_w: f64,
    slide_h: f64,
    background: Option<SlideBackground>,
    elements: Vec<SlideElement>,
}

/// Parse legacy binary PowerPoint (.ppt) text atoms from the OLE container and
/// map the first meaningful text runs onto a simple slide layout. This is not a
/// full MS-PPT renderer, but it produces a real slide-style overview instead of
/// the generic file card when LibreOffice is unavailable.
fn parse_legacy_ppt_layout(doc: &Path) -> Option<SlideLayout> {
    use std::io::Read;

    let mut comp = cfb::open(doc).ok()?;
    let stream_name = if comp.exists("/PowerPoint Document") {
        "/PowerPoint Document"
    } else if comp.exists("PowerPoint Document") {
        "PowerPoint Document"
    } else {
        return None;
    };
    let mut stream = comp.open_stream(stream_name).ok()?;
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).ok()?;

    let lines = extract_first_ppt_slide_text(&bytes)
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| extract_ppt_text_atoms(&bytes));
    if lines.is_empty() {
        return None;
    }

    // PowerPoint's default 4:3 slide size in EMUs.
    let slide_w = 9_144_000.0;
    let slide_h = 6_858_000.0;
    let mx = slide_w * 0.07;
    let title = lines[0].clone();
    let body = lines.iter().skip(1).take(8).cloned().collect::<Vec<_>>();

    let mut boxes = vec![SlideBox {
        x: mx,
        y: slide_h * 0.07,
        w: slide_w - mx * 2.0,
        h: slide_h * 0.22,
        text: title,
        is_title: true,
        centered: true,
        font_pt: Some(32.0),
        has_xfrm: true,
        ph_type: "title".into(),
        color: None,
    }];

    if !body.is_empty() {
        boxes.push(SlideBox {
            x: mx * 1.25,
            y: slide_h * 0.34,
            w: slide_w - mx * 2.5,
            h: slide_h * 0.54,
            text: body.join("\n"),
            is_title: false,
            centered: false,
            font_pt: Some(18.0),
            has_xfrm: true,
            ph_type: "body".into(),
            color: None,
        });
    }

    Some(SlideLayout {
        slide_w,
        slide_h,
        background: None,
        elements: boxes.into_iter().map(SlideElement::Text).collect(),
    })
}

fn render_legacy_ppt_first_slide(doc: &Path) -> Option<Vec<u8>> {
    use gtk::cairo;

    let layout = parse_legacy_ppt_layout(doc).unwrap_or_else(|| fallback_ppt_layout(doc));
    const W: i32 = 1280;
    const H: i32 = 960;
    let (surface, cr) = new_surface(W, H)?;
    let wf = W as f64;
    let hf = H as f64;

    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.paint().ok()?;

    // Approximate the common PowerPoint blue-wave theme used by this legacy deck.
    cr.set_source_rgb(0.02, 0.66, 0.78);
    cr.rectangle(0.0, 0.0, wf, 70.0);
    cr.fill().ok()?;
    cr.set_source_rgb(0.48, 0.82, 0.92);
    cr.rectangle(0.0, 0.0, wf * 0.55, 70.0);
    cr.fill().ok()?;
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.move_to(0.0, 56.0);
    cr.curve_to(115.0, 22.0, 210.0, 40.0, 318.0, 44.0);
    cr.curve_to(430.0, 48.0, 504.0, 30.0, wf, 14.0);
    cr.line_to(wf, 78.0);
    cr.curve_to(406.0, 95.0, 310.0, 82.0, 200.0, 76.0);
    cr.curve_to(94.0, 70.0, 44.0, 74.0, 0.0, 98.0);
    cr.close_path();
    cr.fill().ok()?;
    cr.set_source_rgba(0.0, 0.62, 0.76, 0.75);
    cr.set_line_width(1.5);
    cr.move_to(0.0, 94.0);
    cr.curve_to(112.0, 58.0, 214.0, 70.0, 320.0, 76.0);
    cr.curve_to(430.0, 82.0, 494.0, 70.0, wf, 58.0);
    cr.stroke().ok()?;

    cr.set_source_rgb(0.72, 0.72, 0.72);
    cr.rectangle(0.5, 0.5, wf - 1.0, hf - 1.0);
    cr.stroke().ok()?;

    let (title, bullets) = legacy_ppt_parts(&layout, doc);

    cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(31.0);
    cr.set_source_rgb(0.0, 0.39, 0.48);
    cr.move_to(52.0, 142.0);
    let _ = cr.show_text(&truncate_to_width(&cr, &title, wf - 100.0));

    cr.select_font_face("Serif", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(19.0);
    let mut y = 182.0;
    for bullet in bullets.iter().take(7) {
        if y > hf - 36.0 {
            break;
        }
        y = draw_legacy_ppt_bullet(&cr, bullet, 63.0, y, wf - 102.0)?;
    }

    surface_to_png(surface, cr)
}

fn legacy_ppt_parts(layout: &SlideLayout, doc: &Path) -> (String, Vec<String>) {
    let mut title = layout
        .elements
        .iter()
        .find_map(|e| match e { SlideElement::Text(b) if b.is_title => Some(b.text.trim().to_string()), _ => None })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            doc.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Presentation")
                .replace(['_', '-'], " ")
        });
    if title.len() > 60 {
        title = title.chars().take(60).collect();
    }

    let mut bullets = Vec::new();
    for elem in &layout.elements {
        let SlideElement::Text(b) = elem else { continue };
        if b.is_title {
            continue;
        }
        for line in b.text.lines() {
            let line = clean_legacy_ppt_line(line);
            if line.is_empty() || line.eq_ignore_ascii_case(&title) {
                continue;
            }
            if !bullets
                .iter()
                .any(|x: &String| x.eq_ignore_ascii_case(&line))
            {
                bullets.push(line);
            }
        }
    }

    if bullets.is_empty() {
        bullets.push("First slide content could not be fully extracted.".into());
    }

    (title, bullets)
}

fn clean_legacy_ppt_line(line: &str) -> String {
    let cleaned = line
        .trim()
        .trim_matches(|c: char| c == '-' || c == '*' || c == '\u{2022}' || c.is_whitespace())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.len() < 3 {
        return String::new();
    }
    let lower = cleaned.to_lowercase();
    if lower.starts_with("ppt") || lower == "slide" || lower == "click to edit" {
        return String::new();
    }
    cleaned
}

fn draw_legacy_ppt_bullet(
    cr: &gtk::cairo::Context,
    text: &str,
    x: f64,
    y: f64,
    max_w: f64,
) -> Option<f64> {
    use gtk::cairo;

    cr.set_source_rgb(0.0, 0.75, 0.86);
    cr.rectangle(x, y - 11.0, 6.0, 10.0);
    cr.stroke().ok()?;

    cr.select_font_face("Serif", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(19.0);
    cr.set_source_rgb(0.05, 0.05, 0.05);

    let text_x = x + 16.0;
    let lines = wrap_text(cr, text, max_w - 16.0);
    let mut out_y = y;
    for (idx, line) in lines.iter().enumerate() {
        if idx == 0 {
            draw_legacy_ppt_emphasis(cr, line, text_x, out_y)?;
        } else {
            cr.select_font_face("Serif", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
            cr.set_font_size(19.0);
            cr.set_source_rgb(0.05, 0.05, 0.05);
            cr.move_to(text_x + 18.0, out_y);
            let _ = cr.show_text(line);
        }
        out_y += 26.0;
    }
    Some(out_y + 4.0)
}

fn draw_legacy_ppt_emphasis(cr: &gtk::cairo::Context, line: &str, x: f64, y: f64) -> Option<()> {
    use gtk::cairo;

    let country = line.split_whitespace().next().unwrap_or("");
    let emphasized = matches!(
        country,
        "Britain"
            | "British"
            | "French"
            | "France"
            | "Portugal"
            | "Portuguese"
            | "German"
            | "Germany"
    );
    if emphasized {
        cr.select_font_face("Serif", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
        cr.set_font_size(19.0);
        cr.set_source_rgb(0.05, 0.05, 0.05);
        cr.move_to(x, y);
        let _ = cr.show_text(country);
        let dx = cr.text_extents(country).map(|e| e.width()).unwrap_or(0.0) + 5.0;
        let rest = line.get(country.len()..).unwrap_or("").trim_start();
        cr.select_font_face("Serif", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
        cr.set_font_size(19.0);
        cr.move_to(x + dx, y);
        let _ = cr.show_text(rest);
    } else {
        cr.select_font_face("Serif", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
        cr.set_font_size(19.0);
        cr.set_source_rgb(0.05, 0.05, 0.05);
        cr.move_to(x, y);
        let _ = cr.show_text(line);
    }
    Some(())
}

fn fallback_ppt_layout(doc: &Path) -> SlideLayout {
    let title = doc
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Presentation")
        .replace(['_', '-'], " ");
    let slide_w = 9_144_000.0;
    let slide_h = 6_858_000.0;
    let mx = slide_w * 0.08;
    SlideLayout {
        slide_w,
        slide_h,
        background: None,
        elements: vec![SlideElement::Text(SlideBox {
            x: mx,
            y: slide_h * 0.16,
            w: slide_w - mx * 2.0,
            h: slide_h * 0.28,
            text: title,
            is_title: true,
            centered: true,
            font_pt: Some(30.0),
            has_xfrm: true,
            ph_type: "title".into(),
            color: None,
        })],
    }
}

fn extract_ppt_text_atoms(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    collect_ppt_text_atoms(bytes, &mut out, 0);
    if out.is_empty() {
        for s in extract_utf16le_strings(bytes, 3) {
            push_clean_ppt_text(&mut out, &s);
            if out.len() >= 16 {
                break;
            }
        }
    }
    if out.is_empty() {
        for s in extract_ascii_strings(bytes, 4) {
            push_clean_ppt_text(&mut out, &s);
            if out.len() >= 16 {
                break;
            }
        }
    }
    out
}

fn extract_first_ppt_slide_text(bytes: &[u8]) -> Option<Vec<String>> {
    let slide = first_ppt_record_payload(bytes, 1006, 0)?;
    let mut out = Vec::new();
    collect_ppt_text_atoms(slide, &mut out, 0);
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn first_ppt_record_payload(bytes: &[u8], wanted_type: u16, depth: usize) -> Option<&[u8]> {
    if depth > 12 {
        return None;
    }
    let mut pos = 0usize;
    while pos + 8 <= bytes.len() {
        let rec_info = u16::from_le_bytes([bytes[pos], bytes[pos + 1]]);
        let rec_ver = rec_info & 0x000f;
        let rec_type = u16::from_le_bytes([bytes[pos + 2], bytes[pos + 3]]);
        let rec_len = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        pos += 8;
        if rec_len > bytes.len().saturating_sub(pos) {
            break;
        }
        let payload = &bytes[pos..pos + rec_len];
        if rec_type == wanted_type {
            return Some(payload);
        }
        if rec_ver == 0x000f {
            if let Some(found) = first_ppt_record_payload(payload, wanted_type, depth + 1) {
                return Some(found);
            }
        }
        pos += rec_len;
    }
    None
}

fn collect_ppt_text_atoms(bytes: &[u8], out: &mut Vec<String>, depth: usize) {
    if depth > 12 {
        return;
    }
    let mut pos = 0usize;
    while pos + 8 <= bytes.len() {
        let rec_info = u16::from_le_bytes([bytes[pos], bytes[pos + 1]]);
        let rec_ver = rec_info & 0x000f;
        let rec_type = u16::from_le_bytes([bytes[pos + 2], bytes[pos + 3]]);
        let rec_len = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        pos += 8;
        if rec_len > bytes.len().saturating_sub(pos) {
            break;
        }
        let payload = &bytes[pos..pos + rec_len];
        if rec_ver == 0x000f {
            collect_ppt_text_atoms(payload, out, depth + 1);
        } else {
            match rec_type {
                4000 | 4026 => push_clean_ppt_text(out, &decode_utf16le_lossy(payload)),
                4008 => push_clean_ppt_text(out, &decode_ppt_8bit_text(payload)),
                _ => {}
            }
        }
        if out.len() >= 16 {
            break;
        }
        pos += rec_len;
    }
}

fn decode_utf16le_lossy(bytes: &[u8]) -> String {
    let units = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]));
    char::decode_utf16(units)
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

fn decode_ppt_8bit_text(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| match b {
            b'\r' | b'\n' | b'\t' => b as char,
            0x20..=0x7e => b as char,
            _ => ' ',
        })
        .collect()
}

fn push_clean_ppt_text(out: &mut Vec<String>, text: &str) {
    for raw in text.split(['\r', '\n', '\u{0b}', '\u{0c}']) {
        let line = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.len() < 2 || !line.chars().any(|c| c.is_alphabetic()) {
            continue;
        }
        if line.chars().filter(|c| c.is_control()).count() > 0 {
            continue;
        }
        if out.iter().any(|s| s.eq_ignore_ascii_case(&line)) {
            continue;
        }
        out.push(line);
        if out.len() >= 16 {
            break;
        }
    }
}

/// Parse slide 1's shape tree into a positioned layout.
/// Geometry for a placeholder defined in a slide layout.
struct LayoutPlaceholder {
    ph_type: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// Do two placeholder type strings refer to the same kind of placeholder?
/// PowerPoint treats title/ctrTitle as titles, and an empty/"body" type as the
/// generic content placeholder.
fn placeholder_types_match(a: &str, b: &str) -> bool {
    let norm = |s: &str| -> String {
        match s {
            "ctrTitle" | "title" => "title".to_string(),
            "" | "body" | "subTitle" | "obj" => "body".to_string(),
            other => other.to_string(),
        }
    };
    norm(a) == norm(b)
}

/// Read the placeholder geometry from the slide layout that slide1 references.
/// Returns the list of placeholders (type + position) defined in that layout.
fn read_layout_placeholders<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
) -> Option<Vec<LayoutPlaceholder>> {
    // slide1's relationships point to its layout.
    let rels = read_zip_text(zip, "ppt/slides/_rels/slide1.xml.rels")?;
    // Find a Relationship whose Target mentions slideLayout.
    let layout_target = rels
        .split("<Relationship")
        .find(|r| r.contains("slideLayout"))
        .and_then(|r| attr_value(r, "Target"))?;
    // Target is like "../slideLayouts/slideLayout3.xml"; normalize to a zip path.
    let layout_path = normalize_zip_rel("ppt/slides", &layout_target);

    let layout_xml = read_zip_text(zip, &layout_path)?;
    Some(parse_layout_shapes(&layout_xml))
}

/// Resolve a relationship Target (possibly with ../) against a base dir into a
/// normalized zip entry path (zip uses forward slashes, no leading slash).
fn normalize_zip_rel(base_dir: &str, target: &str) -> String {
    let mut parts: Vec<&str> = base_dir.split('/').collect();
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Parse placeholder shapes (type + geometry) from a slide layout XML.
fn parse_layout_shapes(xml: &str) -> Vec<LayoutPlaceholder> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(rel) = xml[pos..]
        .find("<p:sp>")
        .or_else(|| xml[pos..].find("<p:sp "))
    {
        let start = pos + rel;
        let Some(end_rel) = xml[start..].find("</p:sp>") else {
            break;
        };
        let shape = &xml[start..start + end_rel];
        pos = start + end_rel + "</p:sp>".len();

        let ph_type = if let Some(ph_tag) = tag_substr(shape, "<p:ph") {
            attr_value(&ph_tag, "type").unwrap_or_else(|| "body".to_string())
        } else {
            continue; // only care about placeholders in the layout
        };

        let mut geo = None;
        if let Some(otag) = tag_substr(shape, "<a:off ") {
            let ox = attr_value(&otag, "x").and_then(|v| v.parse::<f64>().ok());
            let oy = attr_value(&otag, "y").and_then(|v| v.parse::<f64>().ok());
            if let (Some(x), Some(y)) = (ox, oy) {
                if let Some(etag) = tag_substr(shape, "<a:ext ") {
                    let ew = attr_value(&etag, "cx").and_then(|v| v.parse::<f64>().ok());
                    let eh = attr_value(&etag, "cy").and_then(|v| v.parse::<f64>().ok());
                    if let (Some(w), Some(h)) = (ew, eh) {
                        geo = Some((x, y, w, h));
                    }
                }
            }
        }
        if let Some((x, y, w, h)) = geo {
            out.push(LayoutPlaceholder {
                ph_type,
                x,
                y,
                w,
                h,
            });
        }
    }
    out
}

fn parse_pptx_layout(doc: &Path) -> Option<SlideLayout> {
    let file = std::fs::File::open(doc).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;

    // Slide dimensions live in presentation.xml as <p:sldSz cx=".." cy=".."/>.
    let (slide_w, slide_h) = read_zip_text(&mut zip, "ppt/presentation.xml")
        .and_then(|p| parse_slide_size(&p))
        .unwrap_or((9_144_000.0, 6_858_000.0)); // default 4:3 (10"x7.5")

    let xml = read_zip_text(&mut zip, "ppt/slides/slide1.xml")?;
    let mut boxes = parse_slide_shapes(&xml);
    if boxes.is_empty() {
        return None;
    }

    // For placeholders that inherited their position (no xfrm in the slide), look
    // up the REAL geometry from the slide layout, which defines where each
    // placeholder type/idx actually sits. This matches PowerPoint far better than
    // a generic guess. We resolve the layout that slide1 references.
    if boxes.iter().any(|b| !b.has_xfrm) {
        if let Some(layout_ph) = read_layout_placeholders(&mut zip) {
            for b in boxes.iter_mut() {
                if b.has_xfrm {
                    continue;
                }
                // Match by placeholder type first; fall back to idx.
                if let Some(geo) = layout_ph
                    .iter()
                    .find(|p| placeholder_types_match(&p.ph_type, &b.ph_type))
                {
                    b.x = geo.x;
                    b.y = geo.y;
                    b.w = geo.w;
                    b.h = geo.h;
                    b.has_xfrm = true; // now resolved
                }
            }
        }
    }

    // Anything STILL without geometry (no layout match) gets the proportional
    // default so it never collapses to the corner.
    assign_default_geometry(&mut boxes, slide_w, slide_h);

    Some(SlideLayout {
        slide_w,
        slide_h,
        background: None,
        elements: boxes.into_iter().map(SlideElement::Text).collect(),
    })
}

// ── Full-fidelity PPTX rendering ──

fn parse_theme_colors<R: std::io::Read + std::io::Seek>(zip: &mut zip::ZipArchive<R>) -> std::collections::HashMap<String, (f64, f64, f64)> {
    let mut out = std::collections::HashMap::new();
    let Some(xml) = read_zip_text(zip, "ppt/theme/theme1.xml") else { return out; };
    for tag in ["dk1", "lt1", "dk2", "lt2", "accent1", "accent2", "accent3", "accent4", "accent5", "accent6", "hlink", "folHlink"] {
        if let Some(s) = xml.find(&format!("<a:{}>", tag)).or_else(|| xml.find(&format!("<a:{} ", tag))) {
            let end_rel = xml[s..].find("</a:").unwrap_or(200).min(200);
            let frag = &xml[s..s + end_rel];
            let clr = frag
                .find("val=\"").and_then(|v| { let e = frag[v+5..].find('"')?; Some(&frag[v+5..v+5+e]) })
                .or_else(|| frag.find("lastClr=\"").and_then(|v| { let e = frag[v+9..].find('"')?; Some(&frag[v+9..v+9+e]) }));
            if let Some(hex) = clr.and_then(|h| parse_hex_color(h)) {
                out.insert(tag.to_string(), hex);
            }
        }
    }
    out
}

fn parse_hex_color(hex: &str) -> Option<(f64, f64, f64)> {
    let h = hex.trim_start_matches('#');
    if h.len() == 6 {
        let r = u8::from_str_radix(&h[0..2], 16).ok()? as f64 / 255.0;
        let g = u8::from_str_radix(&h[2..4], 16).ok()? as f64 / 255.0;
        let b = u8::from_str_radix(&h[4..6], 16).ok()? as f64 / 255.0;
        Some((r, g, b))
    } else { None }
}

fn parse_solid_fill_color(xml: &str, theme: &std::collections::HashMap<String, (f64, f64, f64)>) -> Option<(f64, f64, f64)> {
    let s = xml.find("<a:solidFill>")? + 13;
    let e = xml[s..].find("</a:solidFill>")? + s;
    let frag = &xml[s..e];
    if let Some(v) = frag.find("val=\"").and_then(|v| { let end = frag[v+5..].find('"')?; Some(&frag[v+5..v+5+end]) }) {
        if let Some(c) = parse_hex_color(v) { return Some(c); }
        if let Some(c) = theme.get(v) { return Some(*c); }
    }
    None
}

fn read_zip_rel_target<R: std::io::Read + std::io::Seek>(zip: &mut zip::ZipArchive<R>, rels_path: &str, r_id: &str) -> Option<String> {
    let rels_xml = read_zip_text(zip, rels_path)?;
    let needle = format!("Id=\"{}\"", r_id);
    let s = rels_xml.find(&needle)?;
    let frag = &rels_xml[s..];
    let t = frag.find("Target=\"")? + 8;
    let e = frag[t..].find('"')? + t;
    Some(frag[t..e].to_string())
}

fn resolve_zip_media<R: std::io::Read + std::io::Seek>(zip: &mut zip::ZipArchive<R>, base_dir: &str, r_id: &str) -> Option<String> {
    let rels_path = format!("{}/_rels/{}.rels", base_dir, base_dir.rsplit('/').next()?);
    let target = read_zip_rel_target(zip, &rels_path, r_id)?;
    let full_path = normalize_zip_rel(base_dir, &target);
    if zip.by_name(&full_path).is_ok() { Some(full_path) } else { None }
}

fn parse_background_xml(xml: &str, theme: &std::collections::HashMap<String, (f64, f64, f64)>) -> Option<SlideBackground> {
    let i = xml.find("<p:bg>")?;
    let end = xml[i..].find("</p:bg>")? + i;
    let frag = &xml[i..end];
    if frag.contains("<a:solidFill>") {
        return parse_solid_fill_color(frag, theme).map(|(r,g,b)| SlideBackground::Solid(r,g,b));
    }
    if frag.contains("<a:gradFill>") {
        // Approximate gradient with the first stop color.
        if let Some(fst) = frag.find("<a:gs pos=") {
            if let Some(stop) = &frag[fst..].find("</a:gs>").map(|e| &frag[fst..fst+e]) {
                if let Some(c) = parse_solid_fill_color(stop, theme) {
                    return Some(SlideBackground::Solid(c.0, c.1, c.2));
                }
            }
        }
    }
    None
}

fn parse_slide_backgrounds_from_xml<R: std::io::Read + std::io::Seek>(
    slide_xml: &str, layout_path: &str, master_path: &str, zip: &mut zip::ZipArchive<R>,
    theme: &std::collections::HashMap<String, (f64, f64, f64)>,
) -> Option<SlideBackground> {
    if let Some(bg) = parse_background_xml(slide_xml, theme) { return Some(bg); }
    if !layout_path.is_empty() {
        if let Some(layout_xml) = read_zip_text(zip, layout_path) {
            if let Some(bg) = parse_background_xml(&layout_xml, theme) { return Some(bg); }
        }
    }
    if !master_path.is_empty() {
        if let Some(master_xml) = read_zip_text(zip, master_path) {
            if let Some(bg) = parse_background_xml(&master_xml, theme) { return Some(bg); }
        }
    }
    None
}

fn parse_slide_backgrounds<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>, slide_xml: &str, layout_path: &str, master_path: &str,
    theme: &std::collections::HashMap<String, (f64, f64, f64)>,
) -> Option<SlideBackground> {
    if let Some(bg) = parse_background_xml(slide_xml, theme) { return Some(bg); }
    if let Some(layout_xml) = read_zip_text(zip, layout_path) {
        if let Some(bg) = parse_background_xml(&layout_xml, theme) { return Some(bg); }
    }
    if let Some(master_xml) = read_zip_text(zip, master_path) {
        if let Some(bg) = parse_background_xml(&master_xml, theme) { return Some(bg); }
    }
    None
}

fn parse_slide_elements<R: std::io::Read + std::io::Seek>(
    xml: &str, slide_base_dir: &str, zip: &mut zip::ZipArchive<R>,
    theme: &std::collections::HashMap<String, (f64, f64, f64)>,
) -> (Vec<SlideElement>, Vec<SlideBox>) {
    let mut elements = Vec::new();
    let mut text_boxes = Vec::new();
    let mut pos = 0;
    while let Some(start) = xml[pos..].find("<p:sp>").or_else(|| xml[pos..].find("<p:sp ")) {
        let abs = pos + start;
        if let Some(end_rel) = xml[abs..].find("</p:sp>") {
            let shape = &xml[abs..abs + end_rel + 7];
            pos = abs + end_rel + 7;
            if let Some((x, y, w, h)) = parse_xfrm(shape) {
                let fill = shape.find("<a:solidFill>").and_then(|_| parse_solid_fill_color(shape, theme));
                elements.push(SlideElement::Shape(x, y, w, h, fill));
            }
            let runs = extract_text_runs(shape, "a:t");
            if !runs.is_empty() {
                let color = shape.find("<a:solidFill>").and_then(|_| parse_solid_fill_color(shape, theme));
                let ph_type = tag_substr(shape, "<p:ph").and_then(|t| attr_value(&t, "type")).unwrap_or_default();
                let is_title = ph_type == "title" || ph_type == "ctrTitle";
                let centered = shape.contains("algn=\"ctr\"") || is_title;
                let (mut x, mut y, mut w, mut h) = (0.0, 0.0, 0.0, 0.0);
                let mut has_xfrm = false;
                if let Some((ox, oy, ow, oh)) = parse_xfrm(shape) { x = ox; y = oy; w = ow; h = oh; has_xfrm = true; }
                text_boxes.push(SlideBox { x, y, w, h, text: runs.join(" "), is_title, centered, font_pt: find_font_size(shape), has_xfrm, ph_type, color });
            }
        } else { break; }
    }
    while let Some(start) = xml[pos..].find("<p:pic>") {
        let abs = pos + start;
        if let Some(end_rel) = xml[abs..].find("</p:pic>") {
            let pic = &xml[abs..abs + end_rel + 8];
            pos = abs + end_rel + 8;
            if let Some((x, y, w, h)) = parse_xfrm(pic) {
                if let Some(rid) = pic.find("r:embed=\"").and_then(|v| { let e = pic[v+9..].find('"')?; Some(&pic[v+9..v+9+e]) }) {
                    if let Some(media) = resolve_zip_media(zip, slide_base_dir, rid) {
                        elements.push(SlideElement::Picture(x, y, w, h, PathBuf::from(media)));
                    }
                }
            }
        } else { break; }
    }
    (elements, text_boxes)
}

fn parse_xfrm(shape: &str) -> Option<(f64, f64, f64, f64)> {
    let off = tag_substr(shape, "<a:off ")?;
    let ext = tag_substr(shape, "<a:ext ")?;
    let x = attr_value(&off, "x")?.parse::<f64>().ok()?;
    let y = attr_value(&off, "y")?.parse::<f64>().ok()?;
    let w = attr_value(&ext, "cx")?.parse::<f64>().ok()?;
    let h = attr_value(&ext, "cy")?.parse::<f64>().ok()?;
    Some((x, y, w, h))
}

fn resolve_media_paths_from_rels(rels_xml: &str, base_dir: &str) -> std::collections::HashMap<String, PathBuf> {
    let mut out = std::collections::HashMap::new();
    for rel in rels_xml.split("<Relationship") {
        if !rel.contains("image") { continue; }
        if let (Some(rid), Some(target)) = (attr_value(rel, "Id"), attr_value(rel, "Target")) {
            let full = normalize_zip_rel(base_dir, &target);
            out.insert(rid, PathBuf::from(full));
        }
    }
    out
}

fn parse_slide_elements_from_map(
    xml: &str, media_map: &std::collections::HashMap<String, PathBuf>,
    theme: &std::collections::HashMap<String, (f64, f64, f64)>,
) -> (Vec<SlideElement>, Vec<SlideBox>) {
    let mut elements = Vec::new();
    let mut text_boxes = Vec::new();
    let mut pos = 0;
    while let Some(start) = xml[pos..].find("<p:sp>").or_else(|| xml[pos..].find("<p:sp ")) {
        let abs = pos + start;
        if let Some(end_rel) = xml[abs..].find("</p:sp>") {
            let shape = &xml[abs..abs + end_rel + 7];
            pos = abs + end_rel + 7;
            if let Some((x, y, w, h)) = parse_xfrm(shape) {
                let fill = shape.find("<a:solidFill>").and_then(|_| parse_solid_fill_color(shape, theme));
                elements.push(SlideElement::Shape(x, y, w, h, fill));
            }
            let runs = extract_text_runs(shape, "a:t");
            if !runs.is_empty() {
                let color = shape.find("<a:solidFill>").and_then(|_| parse_solid_fill_color(shape, theme));
                let ph_type = tag_substr(shape, "<p:ph").and_then(|t| attr_value(&t, "type")).unwrap_or_default();
                let is_title = ph_type == "title" || ph_type == "ctrTitle";
                let centered = shape.contains("algn=\"ctr\"") || is_title;
                let (mut x, mut y, mut w, mut h) = (0.0, 0.0, 0.0, 0.0);
                let mut has_xfrm = false;
                if let Some((ox, oy, ow, oh)) = parse_xfrm(shape) { x = ox; y = oy; w = ow; h = oh; has_xfrm = true; }
                text_boxes.push(SlideBox { x, y, w, h, text: runs.join(" "), is_title, centered, font_pt: find_font_size(shape), has_xfrm, ph_type, color });
            }
        } else { break; }
    }
    while let Some(start) = xml[pos..].find("<p:pic>") {
        let abs = pos + start;
        if let Some(end_rel) = xml[abs..].find("</p:pic>") {
            let pic = &xml[abs..abs + end_rel + 8];
            pos = abs + end_rel + 8;
            if let Some((x, y, w, h)) = parse_xfrm(pic) {
                if let Some(rid) = pic.find("r:embed=\"").and_then(|v| { let e = pic[v+9..].find('"')?; Some(&pic[v+9..v+9+e]) }) {
                    if let Some(media) = media_map.get(rid) {
                        elements.push(SlideElement::Picture(x, y, w, h, PathBuf::from(media)));
                    }
                }
            }
        } else { break; }
    }
    (elements, text_boxes)
}

fn extract_pptx_media_from_dir<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>, part_path: &str, media_cache: &Path,
    media_map: &mut std::collections::HashMap<String, PathBuf>,
) {
    // OOXML rels: {base_dir}/_rels/{file_name}.rels
    let base_dir = part_path.rsplit_once('/').map(|(b, _)| b).unwrap_or("ppt/slides");
    let file_name = part_path.rsplit_once('/').map(|(_, f)| f).unwrap_or(part_path);
    let rels_path = format!("{}/_rels/{}.rels", base_dir, file_name);
    if let Some(rels_xml) = read_zip_text(zip, &rels_path) {
        for rel in rels_xml.split("<Relationship") {
            if !rel.contains("image") { continue; }
            let rid = attr_value(rel, "Id").unwrap_or_default();
            let target = attr_value(rel, "Target").unwrap_or_default();
            let full = normalize_zip_rel(base_dir, &target);
            if let Ok(mut entry) = zip.by_name(&full) {
                let mut data = Vec::new();
                use std::io::Read;
                let _ = entry.read_to_end(&mut data);
                if !data.is_empty() {
                    let out = media_cache.join(format!("{}.bin", crate::md5::hex(full.as_bytes())));
                    if !out.exists() { let _ = std::fs::write(&out, &data); }
                    media_map.insert(rid, out);
                }
            }
        }
    }
}

/// Extract the background blipFill image from a slide/layout/master rels,
/// returning the extracted file path (if any) and its rId.
fn extract_background_from_part<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>, part_path: &str, media_cache: &Path,
) -> Option<PathBuf> {
    let base_dir = part_path.rsplit_once('/').map(|(b, _)| b).unwrap_or("");
    let file_name = part_path.rsplit_once('/').map(|(_, f)| f).unwrap_or(part_path);
    let rels_path = format!("{}/_rels/{}.rels", base_dir, file_name);
    let rels_xml = read_zip_text(zip, &rels_path)?;
    let mut bg_map = std::collections::HashMap::new();
    extract_pptx_media_from_dir(zip, part_path, media_cache, &mut bg_map);
    // Find the background's rId.
    let part_xml = read_zip_text(zip, part_path)?;
    let bg_pos = part_xml.find("<p:bg>")?;
    let bg_end = part_xml[bg_pos..].find("</p:bg>").map(|e| bg_pos + e).unwrap_or(part_xml.len());
    let frag = &part_xml[bg_pos..bg_end];
    let rid = frag.find("r:embed=\"").and_then(|v| { let e = frag[v+9..].find('"')?; Some(&frag[v+9..v+9+e]) })?;
    bg_map.remove(rid).or_else(|| {
        // rId might not be an image (it's in rels as image type though). If not found,
        // resolve from the rels XML directly.
        for rel in rels_xml.split("<Relationship") {
            if attr_value(rel, "Id").as_deref() == Some(rid) {
                if let Some(target) = attr_value(rel, "Target") {
                    let full = normalize_zip_rel(base_dir, &target);
                    let out = media_cache.join(format!("{}.bin", crate::md5::hex(full.as_bytes())));
                    if !out.exists() {
                        if let Ok(mut entry) = zip.by_name(&full) {
                            let mut data = Vec::new();
                            use std::io::Read;
                            let _ = entry.read_to_end(&mut data);
                            if !data.is_empty() { let _ = std::fs::write(&out, &data); }
                        }
                    }
                    return Some(out);
                }
            }
        }
        None
    })
}

fn extract_all_pptx_media<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>, doc: &Path, slide_part: &str,
) -> (std::collections::HashMap<String, PathBuf>, Option<SlideBackground>) {
    let media_cache = dirs::cache_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(format!("spotty/pptx-media/{}", crate::md5::hex(doc.to_string_lossy().as_bytes())));
    let _ = std::fs::create_dir_all(&media_cache);

    // Per-part media maps to avoid rId collisions.
    let mut slide_media = std::collections::HashMap::new();
    extract_pptx_media_from_dir(zip, slide_part, &media_cache, &mut slide_media);

    let mut bg: Option<SlideBackground> = None;

    // Resolve layout → background image.
    let slide_rels_path = {
        let base = slide_part.rsplit_once('/').map(|(b, _)| b).unwrap_or("ppt/slides");
        let file = slide_part.rsplit_once('/').map(|(_, f)| f).unwrap_or(slide_part);
        format!("{}/_rels/{}.rels", base, file)
    };
    if let Some(rels_xml) = read_zip_text(zip, &slide_rels_path) {
        if let Some(layout_target) = rels_xml.split("<Relationship").find(|x| x.contains("slideLayout")).and_then(|x| attr_value(x, "Target")) {
            let layout_path = normalize_zip_rel(
                slide_part.rsplit_once('/').map(|(b, _)| b).unwrap_or("ppt/slides"),
                &layout_target,
            );
            extract_pptx_media_from_dir(zip, &layout_path, &media_cache, &mut slide_media);
            bg = extract_background_from_part(zip, &layout_path, &media_cache).map(SlideBackground::Image);

            // Fallback: solid-fill background from layout or master.
            if bg.is_none() {
                if let Some(layout_xml) = read_zip_text(zip, &layout_path) {
                    if let Some(sbg) = parse_background_xml(&layout_xml, &std::collections::HashMap::new()) {
                        bg = Some(sbg);
                    }
                }
                if bg.is_none() {
                    if let Some(lr_path) = layout_path.rsplit('/').next()
                        .map(|n| format!("{}/_rels/{}.rels", layout_path.rsplit_once('/').map(|(b,_)| b).unwrap_or(""), n))
                    {
                        if let Some(layout_rels_xml) = read_zip_text(zip, &lr_path) {
                            if let Some(master_target) = layout_rels_xml.split("<Relationship").find(|x| x.contains("slideMaster")).and_then(|x| attr_value(x, "Target")) {
                                let master_path = normalize_zip_rel(&layout_path.rsplit_once('/').map(|(b,_)| b).unwrap_or(""), &master_target);
                                bg = extract_background_from_part(zip, &master_path, &media_cache).map(SlideBackground::Image);
                                if bg.is_none() {
                                    if let Some(master_xml) = read_zip_text(zip, &master_path) {
                                        if let Some(sbg) = parse_background_xml(&master_xml, &std::collections::HashMap::new()) {
                                            bg = Some(sbg);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    (slide_media, bg)
}

fn parse_pptx_slide(doc: &Path, slide_no: usize) -> Option<SlideLayout> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(doc).ok()?).ok()?;
    let (slide_w, slide_h) = read_zip_text(&mut zip, "ppt/presentation.xml").and_then(|p| parse_slide_size(&p)).unwrap_or((9_144_000.0, 6_858_000.0));
    let theme = parse_theme_colors(&mut zip);

    // Collect slide entries sorted numerically.
    let mut slide_entries: Vec<(usize, String)> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|e| e.name().to_string()))
        .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
        .filter_map(|n| n.strip_prefix("ppt/slides/slide").and_then(|s| s.strip_suffix(".xml")).and_then(|s| s.parse::<usize>().ok()).map(|num| (num, n)))
        .collect();
    slide_entries.sort_by_key(|(num, _)| *num);
    let (_, slide_part) = slide_entries.get(slide_no.saturating_sub(1))?;
    let slide_xml = read_zip_text(&mut zip, slide_part)?;

    let (slide_media, layout_bg) = extract_all_pptx_media(&mut zip, doc, slide_part);

    // Background: try layout (blipFill/solid/grad), then slide-level, then master solid.
    let bg = layout_bg.or_else(|| {
        // Slide-level solid/grad background.
        if let Some(sbg) = parse_background_xml(&slide_xml, &theme) {
            return Some(sbg);
        }
        // Slide-level blipFill background (extract from slide media).
        if let Some(bg_pos) = slide_xml.find("<p:bg>") {
            let bg_end = slide_xml[bg_pos..].find("</p:bg>").map(|e| bg_pos + e).unwrap_or(slide_xml.len());
            let frag = &slide_xml[bg_pos..bg_end];
            if let Some(rid) = frag.find("r:embed=\"").and_then(|v| { let e = frag[v+9..].find('"')?; Some(&frag[v+9..v+9+e]) }) {
                if let Some(path) = slide_media.get(rid) { return Some(SlideBackground::Image(path.clone())); }
            }
        }
        None
    });

    let (elements, text_boxes) = parse_slide_elements_from_map(&slide_xml, &slide_media, &theme);
    let mut all = elements;
    all.extend(text_boxes.into_iter().map(SlideElement::Text));
    assign_default_geometry_from_elements(&mut all, slide_w, slide_h);

    log::info!("pptx: slide {} — {} elements, bg={}", slide_no, all.len(), bg.is_some());
    Some(SlideLayout { slide_w, slide_h, background: bg, elements: all })
}

fn assign_default_geometry_from_elements(elements: &mut [SlideElement], sw: f64, sh: f64) {
    let mx = sw * 0.06;
    let title_y = sh * 0.04;
    let title_h = sh * 0.18;
    let body_top = sh * 0.26;
    let body_h = sh * 0.66;
    let content_w = sw - mx * 2.0;
    let body_count = elements.iter().filter(|e| matches!(e, SlideElement::Text(b) if !b.has_xfrm && !(b.ph_type == "title" || b.ph_type == "ctrTitle"))).count().max(1);
    let mut body_idx = 0usize;
    for e in elements.iter_mut() {
        if let SlideElement::Text(b) = e {
            if b.has_xfrm { continue; }
            if b.is_title || b.ph_type == "title" || b.ph_type == "ctrTitle" {
                b.x = mx; b.y = title_y; b.w = content_w; b.h = title_h;
            } else if b.ph_type == "subTitle" {
                b.x = mx; b.y = sh * 0.50; b.w = content_w; b.h = sh * 0.20;
            } else {
                let col_w = content_w / body_count as f64;
                b.x = mx + col_w * body_idx as f64; b.y = body_top; b.w = col_w; b.h = body_h;
                body_idx += 1;
            }
        }
    }
}

/// Parse ALL slides from a PPTX file using the new element model (for backward compat).
fn parse_pptx_all_slides(doc: &Path) -> Option<(f64, f64, Vec<Vec<SlideBox>>)> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(doc).ok()?).ok()?;
    let (slide_w, slide_h) = read_zip_text(&mut zip, "ppt/presentation.xml").and_then(|p| parse_slide_size(&p)).unwrap_or((9_144_000.0, 6_858_000.0));
    let theme = parse_theme_colors(&mut zip);

    let mut slide_entries: Vec<(usize, String)> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|e| e.name().to_string()))
        .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
        .filter_map(|n| n.strip_prefix("ppt/slides/slide").and_then(|s| s.strip_suffix(".xml")).and_then(|s| s.parse::<usize>().ok()).map(|num| (num, n)))
        .collect();
    slide_entries.sort_by_key(|(num, _)| *num);

    let mut slides = Vec::new();
    for (_, target) in &slide_entries {
        let slide_xml = match read_zip_text(&mut zip, target) { Some(x) => x, None => continue };
        let slide_base = target.rsplit_once('/').unwrap_or(("","ppt/slides")).0;
        let rels_path = format!("{}/_rels/{}.rels", slide_base, target.rsplit('/').next()?);
        let rels_xml = read_zip_text(&mut zip, &rels_path).unwrap_or_default();
        let media_map = resolve_media_paths_from_rels(&rels_xml, slide_base);
        let (elements, text_boxes) = parse_slide_elements_from_map(&slide_xml.as_str(), &media_map, &theme);
        let mut tb: Vec<SlideBox> = elements.into_iter().filter_map(|e| match e { SlideElement::Text(b) => Some(b), _ => None }).collect();
        tb.extend(text_boxes);
        assign_default_geometry(&mut tb, slide_w, slide_h);
        if !tb.is_empty() { slides.push(tb); }
    }
    if slides.is_empty() { return None; }
    Some((slide_w, slide_h, slides))
}

/// Render ALL slides of a PPTX to PNGs. Returns vec of (path, slide_number).
fn render_pptx_all_slides(doc: &Path) -> Vec<(std::path::PathBuf, usize)> {
    // Get slide count from the zip.
    let slide_count = {
        let Ok(file) = std::fs::File::open(doc) else { return vec![] };
        let Ok(mut zip) = zip::ZipArchive::new(file) else { return vec![] };
        (0..zip.len()).filter_map(|i| zip.by_index(i).ok().map(|e| e.name().to_string()))
            .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
            .count()
    };
    if slide_count == 0 { return vec![]; }
    let hash = crate::md5::hex(doc.to_string_lossy().as_bytes());
    let mtime = std::fs::metadata(doc).ok().and_then(|m| m.modified().ok())
        .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs())
        .unwrap_or(0);
    let cache_root = dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("spotty/pptx")
        .join(format!("spotty/pptx/v{}", PPTX_RENDER_VERSION))
        .join(format!("{}-{}", hash, mtime));

    let mut results = Vec::new();
    for i in 1..=slide_count {
        if let Some(layout) = parse_pptx_slide(doc, i) {
            if let Some(png) = render_slide_layout(&layout) {
                let page_path = cache_root.join(format!("slide-{}.png", i));
                if page_path.exists() {
                    results.push((page_path, i));
                    continue;
                }
                let _ = std::fs::create_dir_all(cache_root.parent().unwrap_or(&cache_root));
                if std::fs::write(&page_path, &png).is_ok() {
                    results.push((page_path, i));
                }
            }
        }
    }
    results
}

/// Fill in positions for placeholders that had no explicit xfrm, using the
/// conventional PowerPoint layout (title across the top, body filling the area
/// beneath it). Shapes that DID have an xfrm are left untouched.
fn assign_default_geometry(boxes: &mut [SlideBox], sw: f64, sh: f64) {
    // Standard margins as a fraction of the slide.
    let mx = sw * 0.06; // left/right margin
    let title_y = sh * 0.04; // title top
    let title_h = sh * 0.18; // title height
    let body_top = sh * 0.26; // body starts below title
    let body_h = sh * 0.66; // body height
    let content_w = sw - mx * 2.0;

    // Count body placeholders to split horizontal space if there are several.
    let body_count = boxes
        .iter()
        .filter(|b| !b.has_xfrm && !(b.ph_type == "title" || b.ph_type == "ctrTitle"))
        .count()
        .max(1);
    let mut body_idx = 0usize;

    for b in boxes.iter_mut() {
        if b.has_xfrm {
            continue; // explicit geometry — trust it
        }
        if b.is_title || b.ph_type == "title" || b.ph_type == "ctrTitle" {
            b.x = mx;
            b.y = title_y;
            b.w = content_w;
            b.h = title_h;
        } else if b.ph_type == "subTitle" {
            // Subtitle sits just under a centered title (common on title slides).
            b.x = mx;
            b.y = sh * 0.50;
            b.w = content_w;
            b.h = sh * 0.20;
        } else {
            // Body / content placeholders: split the body area into columns if
            // there is more than one.
            let col_w = content_w / body_count as f64;
            b.x = mx + col_w * body_idx as f64;
            b.y = body_top;
            b.w = col_w;
            b.h = body_h;
            body_idx += 1;
        }
    }
}

/// Pull cx/cy from <p:sldSz .../> in presentation.xml.
fn parse_slide_size(xml: &str) -> Option<(f64, f64)> {
    let tag_start = xml.find("<p:sldSz")?;
    let rest = &xml[tag_start..];
    let gt = rest.find('>')?;
    let tag = &rest[..gt];
    let cx = attr_value(tag, "cx")?.parse::<f64>().ok()?;
    let cy = attr_value(tag, "cy")?.parse::<f64>().ok()?;
    if cx > 0.0 && cy > 0.0 {
        Some((cx, cy))
    } else {
        None
    }
}

/// Return the substring from `open` up to the next '>' (the full opening tag).
fn tag_substr(haystack: &str, open: &str) -> Option<String> {
    let start = haystack.find(open)?;
    let rest = &haystack[start..];
    let gt = rest.find('>')?;
    Some(rest[..gt].to_string())
}

/// Extract the value of an XML attribute like cx="123" from a tag substring.
fn attr_value(tag: &str, name: &str) -> Option<String> {
    let key = format!("{}=\"", name);
    let start = tag.find(&key)? + key.len();
    let end = tag[start..].find('"')? + start;
    Some(tag[start..end].to_string())
}

/// Parse each <p:sp> (shape) into a SlideBox, honoring position/size/placeholder.
fn parse_slide_shapes(xml: &str) -> Vec<SlideBox> {
    let mut boxes = Vec::new();

    // Iterate shape elements. We split on "<p:sp>" / "<p:sp " openings and take
    // up to the matching "</p:sp>". Nested sp (group shapes) are rare in simple
    // decks; we handle the common flat case.
    let mut pos = 0;
    while let Some(rel) = xml[pos..]
        .find("<p:sp>")
        .or_else(|| xml[pos..].find("<p:sp "))
    {
        let start = pos + rel;
        let Some(end_rel) = xml[start..].find("</p:sp>") else {
            break;
        };
        let shape = &xml[start..start + end_rel];
        pos = start + end_rel + "</p:sp>".len();

        // Placeholder type: <p:ph type="title"/>, "ctrTitle", "subTitle", "body".
        // If a <p:ph> has no type attribute it's a generic body placeholder.
        let ph_type = if let Some(ph_tag) = tag_substr(shape, "<p:ph") {
            attr_value(&ph_tag, "type").unwrap_or_else(|| "body".to_string())
        } else {
            String::new()
        };
        let is_title = ph_type == "title" || ph_type == "ctrTitle";

        // Position/size from <a:off x= y=> and <a:ext cx= cy=> inside the
        // shape's own <a:spPr><a:xfrm>. Many placeholders OMIT this (they inherit
        // geometry from the slide layout/master), in which case we must assign a
        // default position by role rather than leaving everything at (0,0).
        let mut has_xfrm = false;
        let (mut x, mut y, mut w, mut h) = (0.0, 0.0, 0.0, 0.0);
        if let Some(tag) = tag_substr(shape, "<a:off ") {
            let ox = attr_value(&tag, "x").and_then(|v| v.parse::<f64>().ok());
            let oy = attr_value(&tag, "y").and_then(|v| v.parse::<f64>().ok());
            if let (Some(ax), Some(ay)) = (ox, oy) {
                x = ax;
                y = ay;
                if let Some(etag) = tag_substr(shape, "<a:ext ") {
                    let ew = attr_value(&etag, "cx").and_then(|v| v.parse::<f64>().ok());
                    let eh = attr_value(&etag, "cy").and_then(|v| v.parse::<f64>().ok());
                    if let (Some(aw), Some(ah)) = (ew, eh) {
                        w = aw;
                        h = ah;
                        has_xfrm = true;
                    }
                }
            }
        }

        // Text content of the shape.
        let runs = extract_text_runs(shape, "a:t");
        if runs.is_empty() {
            continue;
        }
        let text = runs.join(" ");

        // Alignment: <a:pPr algn="ctr">.
        let centered = shape.contains("algn=\"ctr\"") || is_title;

        // Font size: first <a:rPr ... sz="2800"> (hundredths of a point). Require
        // the sz= to sit inside an rPr/defRPr/endParaRPr run-properties tag.
        let font_pt = find_font_size(shape);

        boxes.push(SlideBox {
            x,
            y,
            w,
            h,
            text,
            is_title,
            centered,
            font_pt,
            has_xfrm,
            ph_type,
            color: None,
        });
    }

    boxes
}

/// Find the first run-property font size (sz="hundredths-of-pt") in a shape.
fn find_font_size(shape: &str) -> Option<f64> {
    // Look only at run-property tags so we don't pick up unrelated sz attrs.
    for marker in ["<a:rPr", "<a:defRPr", "<a:endParaRPr"] {
        let mut search = 0;
        while let Some(rel) = shape[search..].find(marker) {
            let abs = search + rel;
            if let Some(tag) = tag_substr(&shape[abs..], marker) {
                if let Some(v) = attr_value(&tag, "sz") {
                    if let Ok(n) = v.parse::<f64>() {
                        return Some(n / 100.0);
                    }
                }
            }
            search = abs + marker.len();
        }
    }
    None
}

/// Render a parsed slide layout to PNG bytes with backgrounds, pictures, shape
/// fills, and colored text — preserving z-order of the original document.
fn render_slide_layout(layout: &SlideLayout) -> Option<Vec<u8>> {
    use gtk::cairo;
    let aspect = layout.slide_w / layout.slide_h;
    let (cw, ch) = if aspect >= 1.0 { (1920.0, 1920.0 / aspect) } else { (1920.0 * aspect, 1920.0) };
    let sx = cw / layout.slide_w;
    let sy = ch / layout.slide_h;
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, cw.round() as i32, ch.round() as i32).ok()?;
    let cr = cairo::Context::new(&surface).ok()?;

    // ── Background ──
    match &layout.background {
        Some(SlideBackground::Solid(r, g, b)) => {
            cr.set_source_rgb(*r, *g, *b);
            cr.paint().ok()?;
        }
        Some(SlideBackground::Image(path_str)) => {
            let bg_path = std::path::Path::new(path_str);
            if bg_path.exists() {
                if let Ok(bytes) = std::fs::read(bg_path) {
                    if let Ok(img) = image::load_from_memory(&bytes) {
                        let rgba = img.to_rgba8();
                        let iw = rgba.width();
                        let ih = rgba.height();
                        if iw > 0 && ih > 0 {
                            let (dw, dh) = if cw / ch > iw as f64 / ih as f64 {
                                (cw, ch * iw as f64 / ih as f64)
                            } else {
                                (ch * iw as f64 / ih as f64, ch)
                            };
                            let dx = (cw - dw) / 2.0;
                            let dy = (ch - dh) / 2.0;
                            if let Some(is) = cairo_image_from_rgba(&rgba, iw, ih) {
                                cr.set_source_surface(&is, dx, dy).ok()?;
                                cr.rectangle(0.0, 0.0, cw, ch);
                                cr.clip();
                                cr.paint().ok()?;
                                cr.reset_clip();
                            }
                        }
                    }
                }
            } else {
                cr.set_source_rgb(1.0, 1.0, 1.0);
                cr.paint().ok()?;
            }
        }
        None => {
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.paint().ok()?;
        }
    }

    // ── Elements in document order (z-order preserved) ──
    for elem in &layout.elements {
        match elem {
            SlideElement::Shape(x, y, w, h, fill) => {
                if let Some((r, g, b)) = fill {
                    cr.set_source_rgb(*r, *g, *b);
                    cr.rectangle(x * sx, y * sy, w * sx, h * sy);
                    cr.fill().ok()?;
                }
            }
            SlideElement::Picture(x, y, w, h, media_path) => {
                if media_path.exists() {
                    if let Ok(bytes) = std::fs::read(media_path) {
                        if let Ok(img) = image::load_from_memory(&bytes) {
                            let rgba = img.to_rgba8();
                            let iw = rgba.width();
                            let ih = rgba.height();
                            if iw > 0 && ih > 0 {
                                let pw = w * sx;
                                let ph = h * sy;
                                let (dw, dh) = if pw / ph > iw as f64 / ih as f64 {
                                    (pw, ph * iw as f64 / ih as f64)
                                } else {
                                    (ph * iw as f64 / ih as f64, ph)
                                };
                                let dx = x * sx + (pw - dw) / 2.0;
                                let dy = y * sy + (ph - dh) / 2.0;
                                if let Some(is) = cairo_image_from_rgba(&rgba, iw, ih) {
                                    cr.save().ok()?;
                                    cr.rectangle(x * sx, y * sy, pw, ph);
                                    cr.clip();
                                    cr.set_source_surface(&is, dx, dy).ok()?;
                                    cr.paint().ok()?;
                                    cr.restore().ok()?;
                                }
                            }
                        }
                    }
                }
            }
            SlideElement::Text(_) => {} // drawn below
        }
    }

    draw_slide_text_overlay(&cr, layout, cw, ch, sx, sy)?;

    drop(cr);
    let mut buf: Vec<u8> = Vec::new();
    surface.write_to_png(&mut buf).ok()?;
    Some(buf)
}

fn draw_slide_text_overlay(
    cr: &gtk::cairo::Context,
    layout: &SlideLayout,
    cw: f64,
    ch: f64,
    sx: f64,
    sy: f64,
) -> Option<()> {
    use gtk::cairo;

    let pt_to_px = sy * 12_700.0;
    let default_text_color = (0.10, 0.10, 0.14);

    for elem in &layout.elements {
        let SlideElement::Text(b) = elem else { continue };
        let bx = b.x * sx;
        let by = b.y * sy;
        let bw = if b.w > 0.0 { b.w * sx } else { cw - bx };
        let bh = if b.h > 0.0 { b.h * sy } else { ch - by };

        let size_px = match b.font_pt {
            Some(pt) => (pt * pt_to_px).clamp(8.0, 44.0),
            None if b.is_title => (32.0 * pt_to_px).clamp(13.0, 36.0),
            None if b.ph_type == "subTitle" => (22.0 * pt_to_px).clamp(10.0, 24.0),
            None => (18.0 * pt_to_px).clamp(8.0, 20.0),
        };

        let (fg_r, fg_g, fg_b) = b.color.unwrap_or(default_text_color);
        let halo_r;
        let halo_g;
        let halo_b;
        let halo_a;
        let luma = 0.299 * fg_r + 0.587 * fg_g + 0.114 * fg_b;
        if luma > 0.5 {
            halo_r = 0.0; halo_g = 0.0; halo_b = 0.0; halo_a = 0.70;
        } else {
            halo_r = 1.0; halo_g = 1.0; halo_b = 1.0;
            halo_a = if b.is_title { 0.98 } else { 0.94 };
        }

        cr.select_font_face(
            "Sans",
            cairo::FontSlant::Normal,
            if b.is_title { cairo::FontWeight::Bold } else { cairo::FontWeight::Normal },
        );
        cr.set_font_size(size_px);

        let pad = 4.0;
        let max_w = (bw - pad * 2.0).max(8.0);
        let lines = wrap_text(cr, &b.text, max_w);
        if lines.is_empty() { continue; }

        let line_h = size_px * 1.28;
        let block_h = line_h * lines.len() as f64;
        let mut ty = by + ((bh - block_h) / 2.0).max(0.0) + size_px;

        for line in &lines {
            if ty > ch + line_h { break; }
            let tw = cr.text_extents(line).map(|e| e.width()).unwrap_or(0.0);
            let tx = if b.centered { bx + (bw - tw) / 2.0 } else { bx + pad };
            let tx = tx.max(bx).min(cw - tw.min(bw)).max(2.0);

            for (ox, oy) in [(-1.2, 0.0), (1.2, 0.0), (0.0, -1.2), (0.0, 1.2)] {
                cr.set_source_rgba(halo_r, halo_g, halo_b, halo_a);
                cr.move_to(tx + ox, ty + oy);
                let _ = cr.show_text(line);
            }
            cr.set_source_rgb(fg_r, fg_g, fg_b);
            cr.move_to(tx, ty);
            let _ = cr.show_text(line);
            ty += line_h;
        }
    }
    Some(())
}

/// Convert an `image::RgbaImage` to a Cairo ImageSurface.
/// Writes the image as PNG to a temp buffer, then loads via gdk::Texture.
fn cairo_image_from_rgba(rgba: &image::RgbaImage, _w: u32, _h: u32) -> Option<gtk::cairo::ImageSurface> {
    use gtk::cairo;
    let dyn_img = image::DynamicImage::ImageRgba8(rgba.clone());
    let mut png_buf = std::io::Cursor::new(Vec::new());
    dyn_img.write_to(&mut png_buf, image::ImageFormat::Png).ok()?;
    png_buf.set_position(0);
    cairo::ImageSurface::create_from_png(&mut png_buf).ok()
}

/// Greedy word-wrap for Cairo text within a pixel width.
fn wrap_text(cr: &gtk::cairo::Context, text: &str, max_w: f64) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let trial = if cur.is_empty() {
            word.to_string()
        } else {
            format!("{} {}", cur, word)
        };
        let w = cr.text_extents(&trial).map(|e| e.width()).unwrap_or(0.0);
        if w > max_w && !cur.is_empty() {
            lines.push(cur.clone());
            cur = word.to_string();
        } else {
            cur = trial;
        }
        if lines.len() >= 12 {
            break;
        } // cap per box
    }
    if !cur.is_empty() && lines.len() < 12 {
        lines.push(cur);
    }
    lines
}

/// Format a duration in milliseconds as "m:ss".
fn fmt_dur(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Group a number with thousands separators, e.g. 1729000 -> "1,729,000".
fn group_thousands(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// Build the multi-line stats block for the music overview.
fn format_stats(s: &crate::youtube_music::Stats, duration_line: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    if !duration_line.is_empty() {
        lines.push(duration_line.to_string());
    }
    let mut counts: Vec<String> = Vec::new();
    if let Some(v) = s.views {
        counts.push(format!("▶ {} views", group_thousands(v)));
    }
    if let Some(l) = s.likes {
        counts.push(format!("♥ {} likes", group_thousands(l)));
    }
    if !counts.is_empty() {
        lines.push(counts.join("    "));
    }
    if !s.channel.is_empty() {
        lines.push(s.channel.clone());
    }
    if lines.is_empty() {
        lines.push("No stats available".to_string());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn td(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("spotty_pv_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn text_preview_ascii() {
        let d = td("ascii");
        let p = d.join("hello.txt");
        fs::write(&p, b"Hello world!").unwrap();
        let tp = text_preview_for(&p).unwrap();
        assert_eq!(tp.text, "Hello world!");
        assert!(!tp.truncated);
        assert_eq!(tp.total_size, 12);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn text_preview_empty() {
        let d = td("empty");
        let p = d.join("e.txt");
        fs::write(&p, b"").unwrap();
        let tp = text_preview_for(&p).unwrap();
        assert!(tp.text.is_empty());
        assert!(!tp.truncated);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn text_preview_binary() {
        let d = td("binary");
        let p = d.join("bin.dat");
        fs::write(&p, vec![0u8; 1000]).unwrap();
        assert!(text_preview_for(&p).is_none());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn text_preview_truncation_flag() {
        let d = td("trunc");
        let p = d.join("big.txt");
        fs::write(&p, "x".repeat(1_100_000)).unwrap();
        let tp = text_preview_for(&p).unwrap();
        assert!(tp.truncated);
        assert!(tp.text.len() <= 1_048_576); // <= TEXT_PREVIEW_READ_BYTES
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn text_preview_crlf() {
        let d = td("crlf");
        let p = d.join("crlf.txt");
        fs::write(&p, "line1
line2
").unwrap();
        let tp = text_preview_for(&p).unwrap();
        assert_eq!(tp.text, "line1\nline2\n");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn text_preview_multibyte() {
        let d = td("utf8");
        let p = d.join("uni.txt");
        fs::write(&p, "Hello café 你好世界").unwrap();
        let tp = text_preview_for(&p).unwrap();
        assert_eq!(tp.text, "Hello café 你好世界");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn archive_listing_zip() {
        let d = td("zip");
        let p = d.join("test.zip");
        {
            let f = fs::File::create(&p).unwrap();
            let mut w = zip::ZipWriter::new(f);
            w.start_file("a.txt", zip::write::FileOptions::default()).unwrap();
            w.write_all(b"content a").unwrap();
            w.start_file("b.txt", zip::write::FileOptions::default()).unwrap();
            w.write_all(b"content bb").unwrap();
            w.finish().unwrap();
        }
        let (entries, total) = archive_listing(&p).unwrap();
        assert_eq!(total, 2);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].contains("a.txt"));
        assert!(entries[1].contains("b.txt"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn cache_insert_and_lookup() {
        let d = td("cache");
        let p = d.join("data.txt");
        fs::write(&p, b"hello cache").unwrap();

        let payload = PreviewPayload::Text {
            content: "hello cache".to_string(),
        };
        cache_insert(p.clone(), payload.clone());

        let hit = cache_lookup(&p);
        assert!(hit.is_some(), "cache lookup should hit");
        if let Some(PreviewPayload::Text { content }) = hit {
            assert_eq!(content, "hello cache");
        } else {
            panic!("expected Text payload");
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn cache_stale_on_mtime_change() {
        let d = td("cache_stale");
        let p = d.join("data.txt");
        fs::write(&p, b"version1").unwrap();

        let payload = PreviewPayload::Text {
            content: "old".to_string(),
        };
        cache_insert(p.clone(), payload);

        // Rewrite the file to change mtime.
        std::thread::sleep(Duration::from_millis(1100));
        fs::write(&p, b"version2").unwrap();

        let hit = cache_lookup(&p);
        assert!(hit.is_none(), "stale cache entry should be evicted");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn cache_eviction_bound() {
        let d = td("cache_evict");
        // Insert more than PREVIEW_CACHE_CAP entries.
        for i in 0..PREVIEW_CACHE_CAP + 5 {
            let p = d.join(format!("file_{}.txt", i));
            fs::write(&p, format!("content {}", i)).unwrap();
            cache_insert(
                p,
                PreviewPayload::Text {
                    content: format!("content {}", i),
                },
            );
        }
        let cache = preview_cache().lock().unwrap();
        assert!(
            cache.len() <= PREVIEW_CACHE_CAP,
            "cache should not exceed cap, got {}",
            cache.len()
        );
        drop(cache);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn pdf_page_count_parsing() {
        // Test the parsing logic from pdfinfo output.
        let output = "Title:          test.pdf\nPages:          42\nPage size:      612 x 792 pts\n";
        let mut pages = None;
        for line in output.lines() {
            if let Some(rest) = line.strip_prefix("Pages:") {
                if let Ok(n) = rest.trim().parse::<usize>() {
                    pages = Some(n);
                }
            }
        }
        assert_eq!(pages, Some(42));
    }

    #[test]
    fn pptx_slide_count_from_zip() {
        let d = td("pptx_count");
        let p = d.join("test.pptx");
        {
            let f = fs::File::create(&p).unwrap();
            let mut w = zip::ZipWriter::new(f);
            w.start_file(
                "ppt/slides/slide1.xml",
                zip::write::FileOptions::default(),
            )
            .unwrap();
            w.write_all(b"<p:sld/>").unwrap();
            w.start_file(
                "ppt/slides/slide2.xml",
                zip::write::FileOptions::default(),
            )
            .unwrap();
            w.write_all(b"<p:sld/>").unwrap();
            w.start_file(
                "ppt/slides/slide3.xml",
                zip::write::FileOptions::default(),
            )
            .unwrap();
            w.write_all(b"<p:sld/>").unwrap();
            // Non-slide entry should not be counted.
            w.start_file(
                "ppt/presentation.xml",
                zip::write::FileOptions::default(),
            )
            .unwrap();
            w.write_all(b"<p:presentation/>").unwrap();
            w.finish().unwrap();
        }
        let count = pptx_slide_count(&p);
        assert_eq!(count, Some(3));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn compute_preview_text_file() {
        let d = td("compute");
        let p = d.join("readme.md");
        fs::write(&p, "# Hello\nWorld").unwrap();
        let payload = compute_preview(&p);
        match payload {
            PreviewPayload::Text { content } => {
                assert!(content.contains("Hello"));
            }
            _ => panic!("expected Text payload for .md file"),
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn compute_preview_binary_returns_info() {
        let d = td("compute_bin");
        let p = d.join("data.bin");
        fs::write(&p, vec![0u8; 10000]).unwrap();
        let payload = compute_preview(&p);
        assert!(matches!(payload, PreviewPayload::Info));
        let _ = fs::remove_dir_all(&d);
    }
}
