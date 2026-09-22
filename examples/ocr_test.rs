use std::path::PathBuf;

#[path = "../src/ocr.rs"]
mod ocr;

fn is_heic_bytes(bytes: &[u8]) -> bool {
    bytes.len() >= 12
        && (&bytes[4..12] == b"ftypheic"
            || &bytes[4..12] == b"ftypmif1"
            || &bytes[4..12] == b"ftypmsf1")
}

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

fn decode_svg_to_image(path: &std::path::Path) -> Result<image::DynamicImage, Box<dyn std::error::Error>> {
    let svg = std::fs::read_to_string(path)?;
    let opt = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_str(&svg, &opt)?;
    let size = tree.size().to_int_size();
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size.width(), size.height())
        .ok_or("failed to create pixmap")?;
    resvg::render(&tree, resvg::tiny_skia::Transform::identity(), &mut pixmap.as_mut());
    image::RgbaImage::from_raw(size.width(), size.height(), pixmap.data().to_vec())
        .map(image::DynamicImage::ImageRgba8)
        .ok_or_else(|| "failed to create image from pixmap".into())
}

fn load_image(path: &std::path::Path) -> Result<image::DynamicImage, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if is_heic_bytes(&bytes) {
        decode_heif_to_rgba(&bytes)
    } else if ext == "svg" {
        decode_svg_to_image(path)
    } else {
        Ok(image::load_from_memory(&bytes)?)
    }
}

fn main() {
    let path: PathBuf = std::env::args()
        .nth(1)
        .expect("usage: cargo run --example ocr_test -- <image>")
        .into();
    if !path.exists() {
        eprintln!("file not found: {}", path.display());
        std::process::exit(1);
    }

    let image = match load_image(&path) {
        Ok(img) => img,
        Err(e) => {
            eprintln!("failed to decode image {}: {}", path.display(), e);
            std::process::exit(1);
        }
    };

    println!("image: {}x{}", image.width(), image.height());
    let start = std::time::Instant::now();
    match ocr::ocr_image(&image) {
        Some(text) => println!("ocr ({:?}):\n{}", start.elapsed(), text),
        None => println!("ocr ({:?}): no text extracted", start.elapsed()),
    }
}
