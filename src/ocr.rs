use std::collections::HashMap;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_uchar, c_void};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::OnceLock;

const TARGET_MAX: u32 = 2400;

// ── Tesseract FFI (moved verbatim from tesseract_ffi.rs) ──

type Handle = *mut c_void;
type DllHandle = *mut c_void;

struct Tesseract {
    _lib: DllHandle,
    api: Mutex<Handle>,
    set_image: unsafe extern "C" fn(Handle, *const c_uchar, c_int, c_int, c_int, c_int),
    set_source_resolution: unsafe extern "C" fn(Handle, c_int),
    set_page_seg_mode: unsafe extern "C" fn(Handle, c_int),
    set_variable: unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> c_int,
    get_utf8_text: unsafe extern "C" fn(Handle) -> *mut c_char,
    delete_text: unsafe extern "C" fn(*mut c_char),
    clear: unsafe extern "C" fn(Handle),
    // Optional: present in libtesseract ≥3.02, null-checked so an older
    // Flatpak runtime without this symbol still works (falls back to
    // first-non-empty scoring).
    mean_text_conf: Option<unsafe extern "C" fn(Handle) -> c_int>,
}

unsafe impl Send for Tesseract {}
unsafe impl Sync for Tesseract {}

static INSTANCE: OnceLock<Option<Tesseract>> = OnceLock::new();

fn instance() -> Option<&'static Tesseract> {
    INSTANCE.get_or_init(|| unsafe {
        let lib = libc::dlopen(
            b"libtesseract.so.5.5\0".as_ptr() as *const c_char,
            libc::RTLD_LAZY,
        );
        if lib.is_null() {
            log::warn!("tesseract: libtesseract.so.5.5 not found");
            return None;
        }

        macro_rules! sym {
            ($lib:expr, $name:literal) => {
                std::mem::transmute::<*mut c_void, _>(
                    libc::dlsym($lib, concat!($name, "\0").as_ptr() as *const c_char),
                )
            };
        }

        macro_rules! try_sym {
            ($lib:expr, $name:literal) => ({
                let raw = libc::dlsym($lib, concat!($name, "\0").as_ptr() as *const c_char);
                if raw.is_null() {
                    None
                } else {
                    Some(std::mem::transmute::<*mut c_void, _>(raw))
                }
            });
        }

        let create: unsafe extern "C" fn() -> Handle = sym!(lib, "TessBaseAPICreate");
        let init3: unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> c_int =
            sym!(lib, "TessBaseAPIInit3");
        let set_image: unsafe extern "C" fn(Handle, *const c_uchar, c_int, c_int, c_int, c_int) =
            sym!(lib, "TessBaseAPISetImage");
        let set_source_resolution: unsafe extern "C" fn(Handle, c_int) =
            sym!(lib, "TessBaseAPISetSourceResolution");
        let set_page_seg_mode: unsafe extern "C" fn(Handle, c_int) =
            sym!(lib, "TessBaseAPISetPageSegMode");
        let set_variable: unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> c_int =
            sym!(lib, "TessBaseAPISetVariable");
        let get_utf8_text: unsafe extern "C" fn(Handle) -> *mut c_char =
            sym!(lib, "TessBaseAPIGetUTF8Text");
        let delete_text: unsafe extern "C" fn(*mut c_char) = sym!(lib, "TessDeleteText");
        let clear: unsafe extern "C" fn(Handle) = sym!(lib, "TessBaseAPIClear");
        let mean_text_conf: Option<unsafe extern "C" fn(Handle) -> c_int> =
            try_sym!(lib, "TessBaseAPIMeanTextConf");

        let api = create();
        if api.is_null() {
            log::error!("tesseract: TessBaseAPICreate returned null");
            libc::dlclose(lib);
            return None;
        }

        let tessdata = find_tessdata();
        let Some(tessdata_path) = tessdata else {
            log::warn!("tesseract: no tessdata found (eng.traineddata missing)");
            (clear)(api);
            libc::dlclose(lib);
            return None;
        };

        let td = std::ffi::CString::new(tessdata_path).unwrap();
        let lang = std::ffi::CString::new("eng").unwrap();
        if (init3)(api, td.as_ptr(), lang.as_ptr()) != 0 {
            log::error!("tesseract: TessBaseAPIInit3 failed for {tessdata_path}");
            (clear)(api);
            libc::dlclose(lib);
            return None;
        }

        log::info!("tesseract: loaded ({tessdata_path})");
        Some(Tesseract {
            _lib: lib,
            api: Mutex::new(api),
            set_image,
            set_source_resolution,
            set_page_seg_mode,
            set_variable,
            get_utf8_text,
            delete_text,
            clear,
            mean_text_conf,
        })
    })
    .as_ref()
}

fn find_tessdata() -> Option<&'static str> {
    if let Ok(val) = std::env::var("TESSDATA_PREFIX") {
        let p = val.trim().to_string();
        let candidate = if p.ends_with('/') || p.ends_with("tessdata") {
            p.clone()
        } else {
            format!("{}/tessdata", p)
        };
        if std::path::Path::new(&candidate).join("eng.traineddata").exists() {
            return Some(Box::leak(candidate.into_boxed_str()));
        }
    }
    for path in [
        "/usr/share/tesseract/tessdata",
        "/usr/share/tessdata",
        "/usr/local/share/tessdata",
        "/app/share/tessdata",
    ] {
        if std::path::Path::new(path).join("eng.traineddata").exists() {
            return Some(path);
        }
    }
    None
}

pub fn is_available() -> bool {
    instance().is_some()
}

pub fn probe_availability() {
    if instance().is_none() {
        log::warn!("tesseract: UNAVAILABLE — OCR will be skipped for all images");
    } else {
        log::info!("tesseract: AVAILABLE — OCR is ready for image files");
    }
}

/// Pipeline version bumped when preprocessing or OCR logic changes so old
/// cache entries are lazily refreshed.
const OCR_PIPELINE_VERSION: u32 = 2;

/// PDF text version: poppler-first extraction. Separate from image pipeline
/// so images are NOT re-swept when PDFs are refreshed.
const PDF_TEXT_VERSION: u32 = 3;

pub fn ocr_image(image: &image::DynamicImage) -> Option<String> {
    ocr_image_scored(image).map(|(text, _conf)| text)
}

/// Scored multi-variant OCR. Returns `(text, confidence)`. The caller
/// stores the confidence alongside the text so old cache entries (written
/// by an earlier pipeline version) can be identified.
pub fn ocr_image_scored(image: &image::DynamicImage) -> Option<(String, i32)> {
    let plain = prepare_plain(image);
    let mut best: Option<(String, i32)> = None;

    // Variant 1: plain, PSM 3/6/11
    for psm in [3i32, 6, 11] {
        if let Some((text, conf)) = ocr_luma_with_psm(&plain, psm) {
            consider(&mut best, text, conf);
        }
    }
    if best.as_ref().is_some_and(|(_, c)| *c >= 85) {
        return best;
    }

    // Variant 2: bright isolate (white/light text isolation)
    {
        let bright = bright_threshold(&plain, 205);
        for psm in [6i32, 11] {
            if let Some((text, conf)) = ocr_luma_with_psm(&bright, psm) {
                consider(&mut best, text, conf);
            }
        }
        if best.as_ref().is_some_and(|(_, c)| *c >= 85) {
            return best;
        }
    }

    // Variant 3: Otsu binarization
    {
        let otsu = otsu_threshold(&plain);
        for psm in [6i32, 11] {
            if let Some((text, conf)) = ocr_luma_with_psm(&otsu, psm) {
                consider(&mut best, text, conf);
            }
        }
        if best.as_ref().is_some_and(|(_, c)| *c >= 85) {
            return best;
        }
    }

    // Variant 4: 2× upscale (for small images with tiny text)
    if plain.dimensions().1 <= 600 && best.as_ref().is_some_and(|(_, c)| *c < 60) {
        let upscaled = upscale_2x(&plain);
        for psm in [6i32, 11] {
            if let Some((text, conf)) = ocr_luma_with_psm(&upscaled, psm) {
                consider(&mut best, text, conf);
            }
        }
    }

    best
}

/// Update the best candidate: ignore empty text; prefer higher confidence;
/// within ±5 conf prefer longer text.
fn consider(best: &mut Option<(String, i32)>, text: String, conf: i32) {
    let text = text.trim().to_string();
    if text.is_empty() {
        return;
    }
    match best.as_ref() {
        None => *best = Some((text, conf)),
        Some((_, bc)) if conf > *bc + 5 => *best = Some((text, conf)),
        Some((bt, bc)) if conf + 5 >= *bc && text.len() > bt.len() => {
            *best = Some((text, conf))
        }
        _ => {}
    }
}

fn prepare_plain(image: &image::DynamicImage) -> image::GrayImage {
    let mut gray = if image.color().has_alpha() {
        let mut rgba = image.to_rgba8();
        for p in rgba.pixels_mut() {
            let a = p[3] as f32 / 255.0;
            p[0] = (p[0] as f32 * a + 255.0 * (1.0 - a)) as u8;
            p[1] = (p[1] as f32 * a + 255.0 * (1.0 - a)) as u8;
            p[2] = (p[2] as f32 * a + 255.0 * (1.0 - a)) as u8;
            p[3] = 255;
        }
        image::DynamicImage::ImageRgba8(rgba).to_luma8()
    } else {
        image.to_luma8()
    };

    // Downscale very large images but do NOT force-upscale small ones
    // (measured: forced min-height 600 hurts accuracy on small memes).
    let (w, h) = gray.dimensions();
    if h > TARGET_MAX {
        let scale = TARGET_MAX as f32 / h as f32;
        gray = image::imageops::resize(
            &gray,
            ((w as f32) * scale) as u32,
            TARGET_MAX,
            image::imageops::FilterType::Lanczos3,
        );
    }

    if is_mostly_dark(&gray) {
        image::imageops::invert(&mut gray);
    }

    contrast_stretch(&mut gray);
    gray
}

fn bright_threshold(img: &image::GrayImage, threshold: u8) -> image::GrayImage {
    let (w, h) = img.dimensions();
    let raw = img.as_raw();
    let mut out = image::GrayImage::new(w, h);
    for (p, &v) in out.pixels_mut().zip(raw.iter()) {
        *p = image::Luma([if v >= threshold { 255u8 } else { 0u8 }]);
    }
    out
}

fn otsu_threshold(img: &image::GrayImage) -> image::GrayImage {
    let mut hist = [0u32; 256];
    for p in img.pixels() {
        hist[p[0] as usize] += 1;
    }
    let total: usize = hist.iter().map(|&c| c as usize).sum();
    let sum_all: u64 = hist.iter().enumerate().map(|(v, &c)| v as u64 * c as u64).sum();
    let mut sum_b: u64 = 0;
    let mut w_b: usize = 0;
    let mut max_var: f64 = 0.0;
    let mut thr: u8 = 128;
    for i in 0..256 {
        w_b += hist[i] as usize;
        if w_b == 0 {
            continue;
        }
        let w_f = total - w_b;
        if w_f == 0 {
            break;
        }
        sum_b += i as u64 * hist[i] as u64;
        let m_b = sum_b as f64 / w_b as f64;
        let m_f = (sum_all - sum_b) as f64 / w_f as f64;
        let var = w_b as f64 * w_f as f64 * (m_b - m_f).powi(2);
        if var > max_var {
            max_var = var;
            thr = i as u8;
        }
    }
    let (w, h) = img.dimensions();
    let raw = img.as_raw();
    let mut out = image::GrayImage::new(w, h);
    for (p, &v) in out.pixels_mut().zip(raw.iter()) {
        *p = image::Luma([if v <= thr { 0u8 } else { 255u8 }]);
    }
    out
}

fn upscale_2x(img: &image::GrayImage) -> image::GrayImage {
    let (w, h) = img.dimensions();
    image::imageops::resize(
        img,
        w.saturating_mul(2).min(TARGET_MAX * 2),
        h.saturating_mul(2).min(TARGET_MAX * 2),
        image::imageops::FilterType::Lanczos3,
    )
}

fn is_mostly_dark(img: &image::GrayImage) -> bool {
    let threshold = 128u8;
    let total = img.pixels().count();
    if total == 0 {
        return false;
    }
    let dark = img.pixels().filter(|p| p[0] < threshold).count();
    dark * 2 > total
}

fn contrast_stretch(img: &mut image::GrayImage) {
    let mut min = 255u8;
    let mut max = 0u8;
    for p in img.pixels() {
        let v = p[0];
        if v < min {
            min = v;
        }
        if v > max {
            max = v;
        }
    }
    if min >= max {
        return;
    }
    let range = (max - min) as u16;
    for p in img.pixels_mut() {
        let v = p[0];
        let stretched = ((v as u16 - min as u16) * 255 / range) as u8;
        p[0] = stretched;
    }
}

fn ocr_luma_with_psm(image: &image::GrayImage, psm: i32) -> Option<(String, i32)> {
    let sess = instance()?;
    let (w, h) = image.dimensions();
    let raw = image.as_raw();
    let api = sess.api.lock().ok()?;
    unsafe {
        (sess.set_page_seg_mode)(*api, psm);

        (sess.set_image)(*api, raw.as_ptr(), w as c_int, h as c_int, 1, w as c_int);
        (sess.set_source_resolution)(*api, 300);
        let text_ptr = (sess.get_utf8_text)(*api);
        let conf = match sess.mean_text_conf {
            Some(f) => f(*api),
            None => -1,
        };
        if text_ptr.is_null() {
            (sess.clear)(*api);
            return None;
        }
        let text = CStr::from_ptr(text_ptr).to_string_lossy().into_owned();
        (sess.delete_text)(text_ptr);
        (sess.clear)(*api);
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            log::info!("tesseract: empty result for {}x{} image", w, h);
            None
        } else {
            Some((trimmed, conf))
        }
    }
}

fn ocr_file(path: &Path) -> Option<(String, i32)> {
    log::info!("ocr: processing {}", path.display());
    let bytes = std::fs::read(path).ok()?;
    let image = decode_image(&bytes, path)?;
    log::info!("ocr: decoded {} {}x{}", path.display(), image.width(), image.height());
    let (text, conf) = ocr_image_scored(&image)?;
    log::info!("ocr: {} -> {} chars (conf={})", path.display(), text.len(), conf);
    (!text.trim().is_empty()).then_some((text, conf))
}

fn decode_image(bytes: &[u8], path: &Path) -> Option<image::DynamicImage> {
    if is_heif_avif(bytes) {
        if let Some(img) = decode_heif(bytes) {
            return Some(img);
        }
        log::warn!("ocr: libheif decode failed for {}, trying image crate", path.display());
    }
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    if ext.eq_ignore_ascii_case("svg") {
        let svg = std::fs::read_to_string(path).ok()?;
        return decode_svg(&svg).or_else(|| {
            log::warn!("ocr: SVG decode failed for {}", path.display());
            None
        });
    }
    image::load_from_memory(bytes)
        .map_err(|e| log::warn!("ocr: image decode failed for {}: {e}", path.display()))
        .ok()
}

fn is_heif_avif(bytes: &[u8]) -> bool {
    bytes.len() >= 12
        && matches!(
            &bytes[4..12],
            b"ftypheic" | b"ftypmif1" | b"ftypmsf1" | b"ftypavif" | b"ftypavis"
        )
}

fn decode_heif(bytes: &[u8]) -> Option<image::DynamicImage> {
    use libheif_rs::{ColorSpace, HeifContext, LibHeif, RgbChroma};
    let libheif = LibHeif::new();
    let ctx = HeifContext::read_from_bytes(bytes).ok()?;
    let handle = ctx.primary_image_handle().ok()?;
    let decoded = libheif.decode(&handle, ColorSpace::Rgb(RgbChroma::Rgba), None).ok()?;
    let planes = decoded.planes();
    let interleaved = planes.interleaved?;
    let width = interleaved.width as usize;
    let height = interleaved.height as usize;
    let stride = interleaved.stride as usize;
    let mut rgba = image::RgbaImage::new(width as u32, height as u32);
    for y in 0..height {
        let row = y * stride;
        for x in 0..width {
            let i = row + x * 4;
            rgba.put_pixel(x as u32, y as u32, image::Rgba([
                interleaved.data[i],
                interleaved.data[i + 1],
                interleaved.data[i + 2],
                interleaved.data[i + 3],
            ]));
        }
    }
    Some(image::DynamicImage::ImageRgba8(rgba))
}

fn decode_svg(svg: &str) -> Option<image::DynamicImage> {
    let opt = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_str(svg, &opt).ok()?;
    let size = tree.size().to_int_size();
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size.width(), size.height())?;
    resvg::render(&tree, resvg::tiny_skia::Transform::identity(), &mut pixmap.as_mut());
    image::RgbaImage::from_raw(size.width(), size.height(), pixmap.data().to_vec())
        .map(image::DynamicImage::ImageRgba8)
}

// ── Persistent TSV cache ──

fn cache_path() -> PathBuf {
    let mut p = dirs::cache_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    p.push("spotty/ocr.tsv");
    p
}

static CACHE: OnceLock<Mutex<HashMap<PathBuf, (u64, u64, u32, i32, String)>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<PathBuf, (u64, u64, u32, i32, String)>> {
    CACHE.get_or_init(|| {
        let map = load_cache();
        log::info!("ocr: loaded {} cached entries", map.len());
        Mutex::new(map)
    })
}

fn parse_cache(raw: &str) -> HashMap<PathBuf, (u64, u64, u32, i32, String)> {
    let mut map = HashMap::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(6, '\t');
        let path_str = match parts.next() {
            Some(s) => s,
            None => continue,
        };
        let size: u64 = match parts.next().and_then(|s| s.parse().ok()) {
            Some(v) => v,
            None => continue,
        };
        let mtime: u64 = match parts.next().and_then(|s| s.parse().ok()) {
            Some(v) => v,
            None => continue,
        };
        let rest4 = parts.next().unwrap_or("");
        let rest5 = parts.next();
        let rest6 = parts.next();
        // New format: ver \t conf \t text (6 fields total)
        // Legacy:    text (4 fields total)
        let (ver, conf, text) = match (rest5, rest6) {
            (Some(conf_s), Some(t)) => {
                let ver: u32 = rest4.parse().unwrap_or(0);
                let conf: i32 = conf_s.parse().unwrap_or(0);
                (ver, conf, t.to_string())
            }
            _ => (0u32, 0i32, rest4.to_string()),
        };
        map.insert(PathBuf::from(path_str), (size, mtime, ver, conf, text));
    }
    map
}

fn serialize_cache(map: &HashMap<PathBuf, (u64, u64, u32, i32, String)>) -> String {
    let mut out = String::new();
    for (path, &(size, mtime, ver, conf, ref text)) in map.iter() {
        let path_str = path.to_string_lossy();
        let path_esc = path_str.replace('\t', " ").replace('\n', " ");
        let text_esc = text.replace('\t', " ").replace('\n', " ");
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            path_esc, size, mtime, ver, conf, text_esc
        ));
    }
    out
}

fn load_cache() -> HashMap<PathBuf, (u64, u64, u32, i32, String)> {
    let p = cache_path();
    let raw = match std::fs::read_to_string(&p) {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };
    parse_cache(&raw)
}

fn save_cache() {
    let p = cache_path();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let out = if let Ok(g) = cache().lock() {
        serialize_cache(&g)
    } else {
        return;
    };
    let _ = std::fs::write(&p, out);
}

fn freshness(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let size = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some((size, mtime))
}

// ── Image helpers (kept local; ponytail: dupe of files.rs is_image_file) ──

fn is_image(ext: &str) -> bool {
    matches!(
        ext,
        "png" | "jpg" | "jpeg" | "webp" | "tif" | "tiff" | "bmp" | "avif"
            | "gif" | "ico" | "pnm" | "pgm" | "ppm" | "pbm" | "qoi" | "tga"
            | "heic" | "heif" | "svg"
    )
}

fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| is_image(&e.to_ascii_lowercase()))
}

// ponytail: dupe of index.rs is_prune_dir; unify when a third copy appears
// ponytail: dupe of files.rs is_heavy_content_component + has_heavy_content_component;
// kept in sync. Unify when a third copy appears.
fn is_prune_dir(name: &str) -> bool {
    matches!(
        name,
        "node_modules"
            | "target"
            | ".git"
            | ".cache"
            | "__pycache__"
            | ".venv"
            | "venv"
            | ".npm"
            | ".cargo"
            | ".rustup"
            | ".local"
            | ".var"
            | ".mozilla"
            | ".thumbnails"
            | "snap"
            | ".gradle"
            | ".m2"
            | ".steam"
            | "Trash"
            | "tmp"
            | "GPUCache"
            | "Code Cache"
            | "Service Worker"
    )
}

// ── Public API ──

/// Find-mode entry point. Returns cached OCR text if fresh, otherwise OCRs
/// the image, caches the result (including empty text for failures), and returns.
/// Empty-text records (undecodable files, tesseract failures) return None — this
/// prevents the old bug where a bad file was re-OCR'd on every keystroke.
///
/// Images with `ver < OCR_PIPELINE_VERSION` are treated as stale so the improved
/// multi-variant pipeline replaces their text lazily.
pub fn text_for(path: &Path) -> Option<String> {
    let (size, mtime) = freshness(path)?;
    {
        let g = cache().lock().ok()?;
        if let Some(&(cs, cm, ver, _conf, ref text)) = g.get(path) {
            let fresh = cs == size && cm == mtime;
            let version_ok = !is_image_path(path) || ver >= OCR_PIPELINE_VERSION;
            if fresh && version_ok {
                if text.is_empty() { return None; }
                return Some(text.clone());
            }
        }
    }
    log::info!("ocr: text_for cache miss for {}", path.display());
    let result = ocr_file(path);
    let text = result.as_ref().map(|(t, _)| t.as_str()).unwrap_or("").replace('\t', " ").replace('\n', " ");
    let conf = result.as_ref().map(|(_, c)| *c).unwrap_or(0);
    if let Ok(mut g) = cache().lock() {
        g.insert(path.to_path_buf(), (size, mtime, OCR_PIPELINE_VERSION, conf, text.clone()));
    }
    save_cache();
    if text.is_empty() { None } else { Some(text) }
}

/// Look up cached OCR text without triggering an OCR run.
/// Returns Some(text) if the file has been OCR'd before and the cache is fresh.
/// Pure lookup — ignores version (callers that need refresh use `text_for`).
pub fn cached_text_for(path: &Path) -> Option<String> {
    let (size, mtime) = freshness(path)?;
    let g = cache().lock().ok()?;
    let &(cs, cm, _ver, _conf, ref text) = g.get(path)?;
    if cs != size || cm != mtime {
        return None;
    }
    if text.is_empty() {
        return None;
    }
    Some(text.clone())
}

/// OCR an explicit list of files (e.g. clipboard images), skipping entries
/// whose cache record is fresh (including cached-empty results, so a bad
/// file is never re-OCR'd on every startup). Images with old version are
/// treated as stale for lazy refresh. Saves the cache once at the end.
/// Returns the number of files that produced text.
pub fn scan_paths(paths: &[PathBuf]) -> usize {
    if !is_available() {
        return 0;
    }
    let mut newly = 0usize;
    let mut writes = 0usize;
    for path in paths {
        let Some((size, mtime)) = freshness(path) else {
            continue;
        };
        let is_img = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| is_image(&e.to_ascii_lowercase()));
        if !is_img {
            continue;
        }
        {
            let Ok(g) = cache().lock() else {
                continue;
            };
            if let Some(&(cs, cm, ver, _, _)) = g.get(path) {
                if cs == size && cm == mtime && ver >= OCR_PIPELINE_VERSION {
                    continue;
                }
            }
        }
        let result = ocr_file(path);
        let text = result.as_ref().map(|(t, _)| t.as_str()).unwrap_or("").replace('\t', " ").replace('\n', " ");
        let conf = result.as_ref().map(|(_, c)| *c).unwrap_or(0);
        if let Ok(mut g) = cache().lock() {
            g.insert(path.clone(), (size, mtime, OCR_PIPELINE_VERSION, conf, text.clone()));
        }
        writes += 1;
        if !text.is_empty() {
            newly += 1;
        }
    }
    if writes > 0 {
        save_cache();
        log::info!("ocr: scan_paths — {} files OCR'd ({} with text)", writes, newly);
    }
    newly
}

/// Test-only: insert a cache record for `path` without touching the
/// on-disk cache file. The caller must create the file first so `freshness`
/// matches at lookup time.
#[cfg(test)]
pub(crate) fn seed_test_cache(path: &Path, text: &str) {
    if let Some((size, mtime)) = freshness(path) {
        if let Ok(mut g) = cache().lock() {
            g.insert(path.to_path_buf(), (size, mtime, OCR_PIPELINE_VERSION, 100, text.to_string()));
        }
    }
}

/// Find-mode entry point for PDF text: poppler pdftotext first, then page
/// OCR for scans. Persistent cache keyed by path+size+mtime with
/// `PDF_TEXT_VERSION`, so repeat queries are instant and old entries
/// are refreshed lazily.
pub fn pdf_text_for(path: &Path) -> Option<String> {
    let (size, mtime) = freshness(path)?;
    {
        let g = cache().lock().ok()?;
        if let Some(&(cs, cm, ver, _conf, ref text)) = g.get(path) {
            if cs == size && cm == mtime && ver >= PDF_TEXT_VERSION {
                if text.is_empty() { return None; }
                return Some(text.clone());
            }
        }
    }
    log::info!("ocr: pdf_text_for cache miss for {}", path.display());

    // Try poppler pdftotext first — fast, handles all text encodings.
    if let Some((text, conf)) = pdftotext_extract(path) {
        let text = text.replace('\t', " ").replace('\n', " ");
        log::info!("ocr: pdftotext extracted {} chars (conf={}) for {}", text.len(), conf, path.display());
        if let Ok(mut g) = cache().lock() {
            g.insert(path.to_path_buf(), (size, mtime, PDF_TEXT_VERSION, conf, text.clone()));
        }
        save_cache();
        if text.is_empty() { None } else { Some(text) }
    } else {
        // pdftotext missing or no text — page OCR fallback (scanned/image PDFs).
        let (text, conf) = ocr_pdf_pages(path).unwrap_or_default();
        let text = text.replace('\t', " ").replace('\n', " ");
        log::info!("ocr: pdf OCR {} chars (conf={}) for {}", text.len(), conf, path.display());
        if let Ok(mut g) = cache().lock() {
            g.insert(path.to_path_buf(), (size, mtime, PDF_TEXT_VERSION, conf, text.clone()));
        }
        save_cache();
        if text.is_empty() { None } else { Some(text) }
    }
}

/// Run poppler pdftotext and return `(text, confidence)`. Returns None
/// if pdftotext is unavailable, fails, or yields too little text to be
/// meaningful (< 12 alphanumeric chars — indicates a scan or empty PDF).
fn pdftotext_extract(path: &Path) -> Option<(String, i32)> {
    let bin = resolve_pdftotext()?;
    let out = std::process::Command::new(&bin)
        .arg(path)
        .arg("-")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let text = text.replace('\t', " ");
    // pdftotext default (no -layout) gives reading-order text; that's
    // what we want for substring matching.
    let alnum: usize = text.chars().filter(|c| c.is_alphanumeric()).count();
    if alnum < 12 {
        return None;
    }
    // Confidence 100 for poppler (trusted source).
    Some((text, 100))
}

/// Render every page of `path` (a PDF) to a PNG at 200 dpi and OCR them,
/// concatenating the extracted text. Uses the bundled pdftoppm.
/// Returns `(text, min_page_conf)`.
fn ocr_pdf_pages(path: &Path) -> Option<(String, i32)> {
    let bin = resolve_pdftoppm()?;
    let dir = std::env::temp_dir().join(format!("spotty-pdfocr-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let prefix = dir.join("page");
    let status = std::process::Command::new(&bin)
        .args(["-png", "-r", "200"])
        .arg(path)
        .arg(&prefix)
        .status()
        .ok();
    let mut texts = Vec::new();
    let mut min_conf = 100i32;
    if status.is_some_and(|s| s.success()) {
        let mut i = 1;
        loop {
            let page = dir.join(format!("page-{}.png", i));
            if !page.exists() {
                break;
            }
            if let Some(bytes) = std::fs::read(&page).ok() {
                if let Some(img) = decode_image(&bytes, &page) {
                    if let Some((t, c)) = ocr_image_scored(&img) {
                        texts.push(t);
                        if c < min_conf { min_conf = c; }
                    }
                }
            }
            let _ = std::fs::remove_file(&page);
            i += 1;
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    if texts.is_empty() {
        None
    } else {
        Some((texts.join("\n"), min_conf))
    }
}

/// Locate `pdftoppm` for scanned-PDF OCR: bundled Flatpak path first, then
/// user-local bin, then PATH (same layout as preview.rs resolve_tool).
fn resolve_pdftoppm() -> Option<std::path::PathBuf> {
    let bundled = PathBuf::from("/app/bin/pdftoppm");
    if bundled.exists() {
        return Some(bundled);
    }
    if let Some(home) = dirs::home_dir() {
        let local = home.join(".local/bin/pdftoppm");
        if local.exists() {
            return Some(local);
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let cand = dir.join("pdftoppm");
            if cand.exists() {
                return Some(cand);
            }
        }
    }
    None
}

/// Locate `pdftotext` for PDF text extraction: bundled Flatpak path first,
/// then user-local bin, then PATH.
fn resolve_pdftotext() -> Option<std::path::PathBuf> {
    let bundled = PathBuf::from("/app/bin/pdftotext");
    if bundled.exists() {
        return Some(bundled);
    }
    if let Some(home) = dirs::home_dir() {
        let local = home.join(".local/bin/pdftotext");
        if local.exists() {
            return Some(local);
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let cand = dir.join("pdftotext");
            if cand.exists() {
                return Some(cand);
            }
        }
    }
    None
}

/// Walk `dir` recursively, OCR each image not yet cached (or stale).
/// Returns (new, updated, skipped). Saves every 25 writes and once at end.
pub fn scan(dir: &Path) -> (usize, usize, usize) {
    let mut new = 0usize;
    let mut updated = 0usize;
    let mut skipped = 0usize;
    let mut writes_since_save = 0usize;

    let walker = ignore::WalkBuilder::new(dir)
        .hidden(false)
        .git_global(false)
        .git_ignore(false)
        .git_exclude(false)
        .max_depth(Some(10))
        .filter_entry(|e| !is_prune_dir(&e.file_name().to_string_lossy()))
        .build();

    for result in walker {
        let path = match result {
            Ok(e) if e.file_type().is_some_and(|t| t.is_file()) => e.path().to_path_buf(),
            _ => continue,
        };
        let ext = match path.extension().and_then(|s| s.to_str()) {
            Some(e) => e,
            None => continue,
        };
        if !is_image(ext) {
            continue;
        }

        let (size, mtime) = match freshness(&path) {
            Some(v) => v,
            None => continue,
        };
        {
            let g = cache().lock().unwrap();
            if let Some(&(cs, cm, _ver, _, _)) = g.get(&path) {
                if cs == size && cm == mtime {
                    skipped += 1;
                    continue;
                }
            }
        }

        let was_present = cache().lock().map(|g| g.contains_key(&path)).unwrap_or(false);
        let result = ocr_file(&path);
        let text = result.as_ref().map(|(t, _)| t.as_str()).unwrap_or("").replace('\t', " ").replace('\n', " ");
        let conf = result.as_ref().map(|(_, c)| *c).unwrap_or(0);
        if let Ok(mut g) = cache().lock() {
            g.insert(path, (size, mtime, OCR_PIPELINE_VERSION, conf, text));
        }
        writes_since_save += 1;
        if writes_since_save >= 25 {
            save_cache();
            writes_since_save = 0;
        }
        if was_present { updated += 1 } else { new += 1 }
    }
    if writes_since_save > 0 {
        save_cache();
    }
    (new, updated, skipped)
}

/// Linear scan of cached entries whose text contains `query`.
pub fn search(query: &str) -> Vec<(PathBuf, String)> {
    let q = query.to_lowercase();
    let mut out: Vec<_> = cache()
        .lock()
        .ok()
        .map(|g| {
            g.iter()
                .filter(|(p, &(size, mtime, _ver, _conf, ref text))| {
                    freshness(p).map(|(s, m)| s == size && m == mtime).unwrap_or(false)
                        && text.to_lowercase().contains(&q)
                })
                .map(|(k, &(_, _, _, _, ref t))| (k.clone(), t.clone()))
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_best_prefers_higher_conf() {
        let mut best: Option<(String, i32)> = None;
        consider(&mut best, "short".into(), 60);
        consider(&mut best, "longer text".into(), 90);
        assert_eq!(best.as_ref().unwrap().0, "longer text");
        assert_eq!(best.as_ref().unwrap().1, 90);
    }

    #[test]
    fn pick_best_skips_empty_text() {
        let mut best: Option<(String, i32)> = None;
        consider(&mut best, "".into(), 99);
        assert!(best.is_none());
        consider(&mut best, "hello".into(), 50);
        assert_eq!(best.as_ref().unwrap().0, "hello");
    }

    #[test]
    fn pick_best_prefers_longer_on_tie() {
        let mut best: Option<(String, i32)> = None;
        consider(&mut best, "ab".into(), 80);
        consider(&mut best, "abcdef".into(), 80);
        assert_eq!(best.as_ref().unwrap().0, "abcdef");
    }

    #[test]
    fn pick_best_prefers_longer_within_five() {
        let mut best: Option<(String, i32)> = None;
        consider(&mut best, "short".into(), 82);
        consider(&mut best, "longer text here".into(), 80);
        // conf 82 > 80 + 5? 82 > 85? no → tie → longer wins
        assert_eq!(best.as_ref().unwrap().0, "longer text here");
    }

    #[test]
    fn pick_best_higher_conf_outweighs_longer() {
        let mut best: Option<(String, i32)> = None;
        consider(&mut best, "short".into(), 90);
        consider(&mut best, "much longer text".into(), 70);
        // 90 > 70 + 5 → higher conf wins
        assert_eq!(best.as_ref().unwrap().0, "short");
    }

    #[test]
    fn cache_round_trip_v2() {
        let mut map = HashMap::new();
        map.insert(
            PathBuf::from("/a/b.png"),
            (1234, 5678, 2u32, 85i32, "hello world".to_string()),
        );
        map.insert(
            PathBuf::from("/c/d.txt"),
            (99, 88, 0u32, 0i32, "some text".to_string()),
        );
        let serialized = serialize_cache(&map);
        let parsed = parse_cache(&serialized);
        assert_eq!(parsed.len(), 2);
        let e = parsed.get(&PathBuf::from("/a/b.png")).unwrap();
        assert_eq!(e.0, 1234);
        assert_eq!(e.1, 5678);
        assert_eq!(e.2, 2);
        assert_eq!(e.3, 85);
        assert_eq!(e.4, "hello world");
        let e2 = parsed.get(&PathBuf::from("/c/d.txt")).unwrap();
        assert_eq!(e2.2, 0);
        assert_eq!(e2.4, "some text");
    }

    #[test]
    fn cache_legacy_format_parses() {
        // Legacy 4-field: path \t size \t mtime \t text
        let legacy = "/foo/bar.png\t100\t200\told text here\n";
        let parsed = parse_cache(legacy);
        let e = parsed.get(&PathBuf::from("/foo/bar.png")).unwrap();
        assert_eq!(e.0, 100);
        assert_eq!(e.1, 200);
        assert_eq!(e.2, 0); // ver = 0 (legacy)
        assert_eq!(e.3, 0); // conf = 0 (legacy)
        assert_eq!(e.4, "old text here");
    }

    #[test]
    fn cache_text_with_tabs_and_newlines() {
        let mut map = HashMap::new();
        map.insert(
            PathBuf::from("/x.png"),
            (1, 2, 2u32, 50i32, "line1\nline2".to_string()),
        );
        let serialized = serialize_cache(&map);
        // Tabs and newlines in text are escaped on disk
        assert!(serialized.contains("line1 line2"));
        let parsed = parse_cache(&serialized);
        let e = parsed.get(&PathBuf::from("/x.png")).unwrap();
        assert_eq!(e.4, "line1 line2");
    }
}
