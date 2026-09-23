//! Built-in thumbnail generation for .pptx and .xlsx files.
//!
//! Tier 0 (presentations): the **real rendered slide** — the same PNG the
//! preview pane shows (native renderer), downscaled to card size.
//! Tier 1: embedded `docProps/thumbnail` extraction (zero rendering).
//! Tier 2: synthesized content card (pptx) or grid (xlsx) via Cairo — the
//! fallback when nothing above produces an image (sheets, or a deck the
//! renderer can't parse).
//!
//! Tier 0 output bypasses this module's card cache (it lives in the
//! preview's versioned render cache), so list icons can't go stale when the
//! renderer changes. Cards/grids are cached under
//! `$XDG_CACHE_HOME/spotty/thumbnails/` keyed by `(path, mtime, size)`,
//! never regenerated for unchanged files.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

// ── card constants ──────────────────────────────────────────────────
const CARD_W: u32 = 320;
const CARD_H: u32 = 240;
const THUMB_VERSION: u32 = 1;
const MAX_ENTRY_BYTES: u64 = 8 * 1024 * 1024;

// ── memo (in-memory; avoids repeated disk stat + cache reads) ───────
#[derive(Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    path: PathBuf,
    mtime: u64,
    size: u64,
}

#[derive(Clone)]
enum ThumbSlot {
    Pending,
    Done(Option<PathBuf>),
}

fn memo() -> &'static Mutex<HashMap<CacheKey, ThumbSlot>> {
    static M: OnceLock<Mutex<HashMap<CacheKey, ThumbSlot>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

// ── public API ──────────────────────────────────────────────────────

/// Request a thumbnail for `path` into `icon`. Fast-path: if cached, swap
/// the icon synchronously (stat + in-memory memo); otherwise spawn a worker.
pub fn request(icon: &gtk::Image, path: &Path) {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if !matches!(ext.as_str(), "pptx" | "ppsx" | "pps" | "odp" | "xlsx" | "ods" | "csv") {
        return;
    }
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return,
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let key = CacheKey {
        path: path.to_path_buf(),
        mtime,
        size: meta.len(),
    };

    // Fast-path: already computed or in flight.
    let cached = memo().lock().unwrap().get(&key).cloned();
    if let Some(ref slot) = cached {
        if let ThumbSlot::Done(Some(p)) = slot {
            icon.set_from_file(Some(p));
            return;
        }
        return; // Pending or Done(None)
    }

    // Mark pending and spawn a worker (per-request thread, deduped by memo).
    memo().lock().unwrap().insert(key.clone(), ThumbSlot::Pending);
    let icon = icon.clone();
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let thumb = thumbnail_for(&key.path);
        memo().lock().unwrap().insert(key, ThumbSlot::Done(thumb.clone()));
        let _ = tx.send(thumb);
    });
    glib::MainContext::default().spawn_local(async move {
        if let Some(path) = rx.await.ok().flatten() {
            icon.set_from_file(Some(path));
        }
    });
}

// ── entry point ─────────────────────────────────────────────────────

/// Generate a thumbnail for `path`, returning the PNG path on success.
pub fn thumbnail_for(path: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let key = CacheKey {
        path: path.to_path_buf(),
        mtime,
        size: meta.len(),
    };

    let cache_path = cache_path_for(&key);

    // Tier 0 — presentations: the actual rendered slide, the same image the
    // preview pane shows. Checked before the card cache so result icons are
    // the faithful render and never a stale synthesized text card.
    if let Some(slide_png) = crate::preview::pptx_first_slide_png(path) {
        return match downscale_slide_icon(&slide_png, &cache_path) {
            Some(small) => Some(small),
            None => Some(slide_png),
        };
    }

    if cache_path.exists() && cache_path.metadata().map(|m| m.len() > 0).unwrap_or(false) {
        return Some(cache_path);
    }

    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let png = match ext.as_str() {
        "pptx" | "ppsx" | "pps" | "odp" => {
            tier1_embedded_thumbnail(path).or_else(|| render_pptx_card(path))
        }
        "xlsx" | "ods" | "csv" => render_xlsx_grid(path),
        _ => return None,
    };

    if let Some(ref buf) = png {
        let _ = std::fs::create_dir_all(cache_path.parent().unwrap());
        let _ = std::fs::write(&cache_path, buf);
        Some(cache_path)
    } else {
        None
    }
}

// ── cache path ──────────────────────────────────────────────────────

fn cache_path_for(key: &CacheKey) -> PathBuf {
    let raw = format!(
        "{}|{}|{}|v{}",
        key.path.display(),
        key.mtime,
        key.size,
        THUMB_VERSION,
    );
    let hash = crate::md5::hex(raw.as_bytes());
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("spotty")
        .join("thumbnails")
        .join(format!("{}.png", hash))
}

// ── Tier 0: faithful native slide render, downscaled ────────────────

/// Shrink the native slide PNG (e.g. 1200×900) to card size so the result
/// row keeps its layout — the preview pane still shows full resolution.
fn downscale_slide_icon(full_png: &Path, out: &Path) -> Option<PathBuf> {
    let img = image::open(full_png).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let scale = (CARD_W as f32 / w as f32)
        .min(CARD_H as f32 / h as f32)
        .min(1.0);
    let (nw, nh) = if scale < 1.0 {
        (
            ((w as f32 * scale).round() as u32).max(1),
            ((h as f32 * scale).round() as u32).max(1),
        )
    } else {
        (w, h)
    };
    let scaled = image::imageops::resize(&rgba, nw, nh, image::imageops::FilterType::Lanczos3);
    let mut buf = Vec::new();
    image::DynamicImage::ImageRgba8(scaled)
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .ok()?;
    let _ = std::fs::create_dir_all(out.parent()?);
    std::fs::write(out, &buf).ok()?;
    Some(out.to_path_buf())
}

// ── Tier 1: embedded thumbnail ─────────────────────────────────────

pub(crate) fn tier1_embedded_thumbnail(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let candidates = [
        "docProps/thumbnail.jpeg",
        "docProps/thumbnail.jpg",
        "docProps/thumbnail.png",
        "Thumbnails/thumbnail.png",
    ];
    for name in candidates {
        if let Ok(mut entry) = zip.by_name(name) {
            if entry.size() > MAX_ENTRY_BYTES {
                continue;
            }
            let mut buf = Vec::new();
            if entry.read_to_end(&mut buf).is_ok() && !buf.is_empty() {
                return Some(buf);
            }
        }
    }
    None
}

// ── Tier 2: pptx content card ──────────────────────────────────────

fn render_pptx_card(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let mut title = String::new();
    let mut body_lines: Vec<String> = Vec::new();
    let mut first_image_bytes: Option<Vec<u8>> = None;

    let slide_xml = read_zip_string(&mut zip, "ppt/slides/slide1.xml")?;
    for shape in slide_xml.split("<p:sp>") {
        let end = shape.find("</p:sp>").unwrap_or(shape.len());
        let frag = &shape[..end];
        let ph = frag
            .find("<p:ph")
            .and_then(|_| tag_attr(frag, "<p:ph", "type"))
            .unwrap_or_default();
        let runs = extract_a_t_runs(frag);
        if runs.is_empty() {
            continue;
        }
        let joined = runs.join(" ");
        if is_title_placeholder(&ph) && title.is_empty() {
            title = joined;
        } else {
            body_lines.push(joined);
        }
        if body_lines.len() >= 3 {
            break;
        }
    }
    if let Some(rels_xml) = read_zip_string(&mut zip, "ppt/slides/_rels/slide1.xml.rels") {
        if let Some(rid) = first_image_rid(&rels_xml) {
            if let Some(target) = rels_target(&rels_xml, &rid) {
                let full = normalize_pptx_rel("ppt/slides", &target);
                if let Ok(mut entry) = zip.by_name(&full) {
                    if entry.size() <= MAX_ENTRY_BYTES {
                        let mut buf = Vec::new();
                        let _ = entry.read_to_end(&mut buf);
                        if !buf.is_empty() {
                            first_image_bytes = Some(buf);
                        }
                    }
                }
            }
        }
    }
    let img_rgba = first_image_bytes.and_then(|b| {
        image::load_from_memory(&b).ok().map(|img| img.to_rgba8())
    });
    render_card(&title, &body_lines, img_rgba.as_ref())
}

fn render_card(title: &str, body: &[String], img: Option<&image::RgbaImage>) -> Option<Vec<u8>> {
    use gtk::cairo;
    let (surface, cr) = crate::preview::new_surface(CARD_W as i32, CARD_H as i32)?;
    let wf = CARD_W as f64;
    let hf = CARD_H as f64;
    cr.set_source_rgb(0.97, 0.97, 0.98);
    cr.rectangle(0.0, 0.0, wf, hf);
    cr.fill().ok()?;
    let margin = 14.0;
    let mut y = margin;

    cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
    cr.set_font_size(17.0);
    cr.set_source_rgb(0.12, 0.12, 0.14);
    for line in crate::preview::wrap_text(&cr, title, wf - margin * 2.0).into_iter().take(2) {
        if y > hf - 20.0 {
            break;
        }
        cr.move_to(margin, y + 14.0);
        let _ = cr.show_text(&line);
        y += 20.0;
    }
    y += 8.0;

    cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(12.0);
    cr.set_source_rgb(0.35, 0.35, 0.37);
    let body_max_w = if img.is_some() { wf - margin - 80.0 } else { wf - margin * 2.0 };
    for line in body.iter().take(3) {
        if y > hf - 20.0 {
            break;
        }
        for wl in crate::preview::wrap_text(&cr, line, body_max_w).into_iter().take(2) {
            if y > hf - 20.0 {
                break;
            }
            cr.move_to(margin, y + 12.0);
            let _ = cr.show_text(&wl);
            y += 15.0;
        }
    }
    if let Some(rgba) = img {
        let (iw, ih) = rgba.dimensions();
        if iw > 0 && ih > 0 {
            let sw = (iw as f64 * 0.3).min(70.0);
            let sh = (ih as f64 * 0.3).min(50.0);
            let sx = wf - margin - sw;
            let sy = hf - margin - sh;
            if let Some(is) = crate::preview::cairo_image_from_rgba(rgba, iw, ih) {
                cr.save().ok()?;
                cr.translate(sx, sy);
                cr.scale(sw / iw as f64, sh / ih as f64);
                cr.set_source_surface(&is, 0.0, 0.0).ok()?;
                cr.paint().ok()?;
                cr.restore().ok()?;
            }
        }
    }
    crate::preview::surface_to_png(surface, cr)
}

// ── Tier 2: xlsx mini grid ─────────────────────────────────────────

fn render_xlsx_grid(path: &Path) -> Option<Vec<u8>> {
    use calamine::Reader;
    let mut workbook = calamine::open_workbook_auto(path).ok()?;
    let range = workbook.worksheet_range_at(0)?.ok()?;
    let rows = 8.min(range.rows().count());
    let cols = 5.min(range.width());
    if rows == 0 || cols == 0 {
        return None;
    }
    use gtk::cairo;
    let (surface, cr) = crate::preview::new_surface(CARD_W as i32, CARD_H as i32)?;
    let wf = CARD_W as f64;
    let hf = CARD_H as f64;
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.rectangle(0.0, 0.0, wf, hf);
    cr.fill().ok()?;
    let margin = 8.0;
    let cell_h = 20.0;
    let col_w = (wf - margin * 2.0) / cols as f64;
    cr.select_font_face("Mono", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(10.0);
    for ri in 0..rows {
        let y = margin + ri as f64 * cell_h;
        if ri == 0 {
            cr.set_source_rgb(0.90, 0.91, 0.92);
            cr.rectangle(margin, y, wf - margin * 2.0, cell_h);
            cr.fill().ok()?;
        }
        for ci in 0..cols {
            let x = margin + ci as f64 * col_w;
            cr.set_source_rgb(0.82, 0.82, 0.84);
            cr.set_line_width(0.5);
            cr.rectangle(x, y, col_w, cell_h);
            cr.stroke().ok()?;
            let cell_val = range.get_value((ri as u32, ci as u32)).unwrap_or(&calamine::Data::Empty);
            let txt = cell_value_str(cell_val);
            let txt = cell_value_str(cell_val);
            if !txt.is_empty() {
                cr.set_source_rgb(0.12, 0.12, 0.14);
                cr.move_to(x + 3.0, y + 14.0);
                let ellipsized = truncate_to_width(&cr, &txt, col_w - 6.0);
                let _ = cr.show_text(&ellipsized);
            }
        }
    }
    crate::preview::surface_to_png(surface, cr)
}

fn cell_value_str(v: &calamine::Data) -> String {
    match v {
        calamine::Data::Empty => String::new(),
        calamine::Data::Bool(b) => b.to_string(),
        calamine::Data::Int(i) => i.to_string(),
        calamine::Data::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 1e15 {
                format!("{}", *f as i64)
            } else {
                format!("{:.4}", f)
            }
        }
        calamine::Data::Error(e) => format!("#{:?}", e),
        calamine::Data::String(s) => s.clone(),
        calamine::Data::DateTime(d) => format!("{}", d),
        calamine::Data::DateTimeIso(d) => d.clone(),
        calamine::Data::DurationIso(d) => d.clone(),
    }
}

// ── helpers ─────────────────────────────────────────────────────────

fn read_zip_string(zip: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Option<String> {
    let mut entry = zip.by_name(name).ok()?;
    let mut buf = String::new();
    entry.read_to_string(&mut buf).ok()?;
    Some(buf)
}

fn tag_attr<'a>(haystack: &'a str, tag: &str, attr: &str) -> Option<&'a str> {
    let i = haystack.find(tag)?;
    let frag = &haystack[i..];
    let key = format!("{}=\"", attr);
    let kpos = frag.find(&key)?;
    let val_start = kpos + key.len();
    let val_end = frag[val_start..].find('"')?;
    Some(&frag[val_start..val_start + val_end])
}

fn is_title_placeholder(ph: &str) -> bool {
    matches!(ph, "title" | "ctrTitle")
}

fn extract_a_t_runs(shape: &str) -> Vec<String> {
    let mut runs = Vec::new();
    let mut pos = 0;
    while let Some(start) = shape[pos..].find("<a:t>") {
        let abs = pos + start;
        if let Some(end) = shape[abs..].find("</a:t>") {
            let text = shape[abs + 5..abs + end].to_string();
            if !text.is_empty() {
                runs.push(text);
            }
            pos = abs + end + 6;
        } else {
            break;
        }
    }
    runs
}

fn first_image_rid(rels_xml: &str) -> Option<String> {
    for rel in rels_xml.split("<Relationship") {
        if !rel.contains("image") {
            continue;
        }
        let id = rel.find("Id=\"").and_then(|v| {
            let s = v + 4;
            let e = rel[s..].find('"')?;
            Some(&rel[s..s + e])
        })?;
        return Some(id.to_string());
    }
    None
}

fn rels_target(rels_xml: &str, rid: &str) -> Option<String> {
    let needle = format!("Id=\"{}\"", rid);
    let s = rels_xml.find(&needle)?;
    let frag = &rels_xml[s..];
    let t = frag.find("Target=\"")? + 8;
    let e = frag[t..].find('"')?;
    Some(frag[t..t + e].to_string())
}

fn normalize_pptx_rel(base_dir: &str, target: &str) -> String {
    let mut parts: Vec<&str> = base_dir.split('/').collect();
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => { parts.pop(); }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn truncate_to_width(cr: &gtk::cairo::Context, text: &str, max_w: f64) -> String {
    if cr.text_extents(text).map(|e| e.width()).unwrap_or(0.0) <= max_w {
        return text.to_string();
    }
    let mut s = String::new();
    for ch in text.chars() {
        let mut trial = s.clone();
        trial.push(ch);
        trial.push('…');
        if cr.text_extents(&trial).map(|e| e.width()).unwrap_or(f64::MAX) > max_w {
            s.push('…');
            return s;
        }
        s.push(ch);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn td(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("thumb_test_{}_{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn cache_path_deterministic() {
        let key = CacheKey {
            path: PathBuf::from("/test/file.pptx"),
            mtime: 1000,
            size: 2000,
        };
        let p1 = cache_path_for(&key);
        let p2 = cache_path_for(&key);
        assert_eq!(p1, p2);
        assert!(p1.to_string_lossy().contains("spotty/thumbnails/"));
    }

    #[test]
    fn embedded_thumbnail_from_zip() {
        let d = td("embedded");
        let p = d.join("test.pptx");
        {
            let f = std::fs::File::create(&p).unwrap();
            let mut w = zip::ZipWriter::new(f);
            w.start_file("docProps/thumbnail.png", zip::write::FileOptions::default()).unwrap();
            w.write_all(b"fake-png-data").unwrap();
            w.finish().unwrap();
        }
        let data = tier1_embedded_thumbnail(&p).unwrap();
        assert_eq!(data, b"fake-png-data");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn card_render_produces_png() {
        let card = render_card("Test Title", &["Line 1".into(), "Line 2".into()], None).unwrap();
        assert!(card.len() > 8);
        assert_eq!(&card[..4], b"\x89PNG"); // PNG magic
    }

    #[test]
    fn thumbnail_for_pptx_with_embedded_thumb() {
        let d = td("thumb");
        let p = d.join("file.pptx");
        {
            let f = std::fs::File::create(&p).unwrap();
            let mut w = zip::ZipWriter::new(f);
            w.start_file("docProps/thumbnail.png", zip::write::FileOptions::default()).unwrap();
            // Write a minimal valid 1x1 red PNG.
            let png: Vec<u8> = vec![
                0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a,
                0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
                0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
                0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
                0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41,
                0x54, 0x08, 0xd7, 0x63, 0xd8, 0xa8, 0xc0, 0x00,
                0x00, 0x00, 0x04, 0x00, 0x01, 0x27, 0x34, 0x21,
                0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44,
                0xae, 0x42, 0x60, 0x82,
            ];
            w.write_all(&png).unwrap();
            w.finish().unwrap();
        }
        let result = thumbnail_for(&p).unwrap();
        assert!(result.exists());
        let meta = std::fs::metadata(&result).unwrap();
        assert!(meta.len() > 0);
        let _ = std::fs::remove_dir_all(&d);
    }
}
