//! Squish: makes PDFs smaller.
//!
//! Most of a bloated PDF is images, usually scanned or photographed at a much
//! higher resolution than anyone needs on screen or for email. Squish works out
//! how big each image is actually drawn on the page, scales it down to the
//! chosen resolution and re-encodes it as JPEG, keeping whichever version is
//! smaller. Then it compresses any uncompressed streams, drops unused objects
//! and writes the file with compressed object streams.

use std::{collections::HashMap, path::{Path, PathBuf}};

use anyhow::Result;
use image::{DynamicImage, GrayImage, RgbImage, imageops::FilterType};
use lopdf::{Document, Object, ObjectId, Stream};
use rayon::prelude::*;

use crate::{content, pages};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Level {
    /// Print quality, barely touched.
    Light,
    /// Screen and email: the default.
    #[default]
    Medium,
    /// As small as it gets while staying readable.
    Strong,
}

impl Level {
    pub const ALL: [Level; 3] = [Level::Light, Level::Medium, Level::Strong];

    /// Highest resolution kept, in pixels per inch of page.
    fn dpi(self) -> f64 {
        match self {
            Level::Light => 300.0,
            Level::Medium => 150.0,
            Level::Strong => 100.0,
        }
    }

    fn jpeg_quality(self) -> u8 {
        match self {
            Level::Light => 85,
            Level::Medium => 70,
            Level::Strong => 50,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Level::Light => "light",
            Level::Medium => "medium",
            Level::Strong => "strong",
        }
    }

    pub fn parse(s: &str) -> Option<Level> {
        Level::ALL.into_iter().find(|l| l.label().eq_ignore_ascii_case(s))
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Report {
    pub images: usize,
    pub shrunk: usize,
}

/// A replacement is only worth it if it saves at least this share of the original.
const MIN_SAVING: f64 = 0.1;

/// Shrinks the document in place.
pub fn squish(doc: &mut Document, level: Level) -> Report {
    // How large is each image drawn, at most, in points?
    let mut drawn: HashMap<ObjectId, (f64, f64)> = HashMap::new();
    for page in doc.get_pages().into_values() {
        content::page_images(doc, page, &mut |id, m| {
            let (w, h) = (m[0].hypot(m[1]), m[2].hypot(m[3]));
            let size = drawn.entry(id).or_default();
            *size = (size.0.max(w), size.1.max(h));
        });
    }

    let candidates: Vec<(ObjectId, Stream, Option<(f64, f64)>)> = doc
        .objects
        .iter()
        .filter_map(|(&id, obj)| match obj {
            Object::Stream(s) if s.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Image") => {
                Some((id, s.clone(), drawn.get(&id).copied()))
            }
            _ => None,
        })
        .collect();

    let replacements: Vec<(ObjectId, Stream)> = candidates
        .par_iter()
        .filter_map(|(id, stream, size)| Some((*id, shrink_image(doc, stream, *size, level)?)))
        .collect();

    let report = Report { images: candidates.len(), shrunk: replacements.len() };
    for (id, stream) in replacements {
        doc.objects.insert(id, Object::Stream(stream));
    }

    // Embedded page thumbnails are ignored by every modern viewer.
    for page in doc.get_pages().into_values() {
        if let Ok(dict) = doc.get_dictionary_mut(page) {
            dict.remove(b"Thumb");
        }
    }
    doc.compress();
    pages::prune_unreachable(doc);
    report
}

/// Serialises with compressed object streams, which alone often saves a good
/// chunk on text PDFs.
pub fn to_bytes(doc: &mut Document) -> Result<Vec<u8>> {
    doc.renumber_objects();
    let mut bytes = Vec::new();
    doc.save_modern(&mut bytes)?;
    Ok(bytes)
}

pub fn default_output(input: &Path) -> PathBuf {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("document");
    input.with_file_name(format!("{stem}.squished.pdf"))
}

/// A smaller version of the image, or `None` if it can't be (safely) made smaller.
fn shrink_image(doc: &Document, stream: &Stream, drawn: Option<(f64, f64)>, level: Level) -> Option<Stream> {
    let dict = &stream.dict;
    let get_int = |key: &[u8]| dict.get(key).ok().and_then(|o| o.as_i64().ok());
    let (w, h) = (get_int(b"Width")? as u32, get_int(b"Height")? as u32);
    // Masks, odd bit depths and custom decode ranges are left alone; they're rarely big.
    if get_int(b"BitsPerComponent")? != 8
        || dict.has(b"Decode")
        || dict.get(b"ImageMask").and_then(Object::as_bool).unwrap_or(false)
        || dict.has(b"SMaskInData")
        || w < 16
        || h < 16
    {
        return None;
    }
    let components = components(doc, dict.get(b"ColorSpace").ok()?)?;
    let filters: Vec<&[u8]> = match dict.get(b"Filter").ok()? {
        Object::Name(n) => vec![n.as_slice()],
        Object::Array(a) => a.iter().filter_map(|f| f.as_name().ok()).collect(),
        _ => return None,
    };

    let image = match filters.as_slice() {
        [b"DCTDecode"] => {
            let img = image::load_from_memory_with_format(&stream.content, image::ImageFormat::Jpeg).ok()?;
            // CMYK and other exotic JPEGs would come back converted; don't touch those.
            if (components == 1) != matches!(img, DynamicImage::ImageLuma8(_)) {
                return None;
            }
            img
        }
        f if f.iter().all(|f| matches!(*f, b"FlateDecode" | b"LZWDecode")) => {
            let data = stream.decompressed_content().ok()?;
            match components {
                1 => DynamicImage::ImageLuma8(GrayImage::from_raw(w, h, data.get(..(w * h) as usize)?.to_vec())?),
                _ => DynamicImage::ImageRgb8(RgbImage::from_raw(w, h, data.get(..(w * h * 3) as usize)?.to_vec())?),
            }
        }
        _ => return None,
    };

    // Images that are drawn nowhere (or whose size we couldn't work out) keep their size.
    let scale = drawn
        .map(|(dw, dh)| {
            let (tw, th) = (dw / 72.0 * level.dpi(), dh / 72.0 * level.dpi());
            (tw / w as f64).max(th / h as f64)
        })
        .unwrap_or(1.0)
        .min(1.0);
    let image = if scale < 0.9 {
        let (nw, nh) = (((w as f64 * scale).round() as u32).max(1), ((h as f64 * scale).round() as u32).max(1));
        image.resize_exact(nw, nh, FilterType::CatmullRom)
    } else {
        image
    };

    let mut jpeg = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, level.jpeg_quality());
    match &image {
        DynamicImage::ImageLuma8(g) => g.write_with_encoder(encoder).ok()?,
        other => other.to_rgb8().write_with_encoder(encoder).ok()?,
    }
    if (jpeg.len() as f64) > stream.content.len() as f64 * (1.0 - MIN_SAVING) {
        return None;
    }

    let mut dict = dict.clone();
    dict.set("Filter", "DCTDecode");
    dict.remove(b"DecodeParms");
    dict.set("Width", image.width() as i64);
    dict.set("Height", image.height() as i64);
    dict.set("BitsPerComponent", 8);
    let mut new = Stream::new(dict, jpeg);
    new.allows_compression = false;
    Some(new)
}

/// The number of colour components, for the colour spaces Squish handles (gray and RGB).
fn components(doc: &Document, space: &Object) -> Option<u32> {
    let space = match space {
        Object::Reference(id) => doc.get_object(*id).ok()?,
        other => other,
    };
    match space {
        Object::Name(n) => match n.as_slice() {
            b"DeviceGray" | b"CalGray" => Some(1),
            b"DeviceRGB" | b"CalRGB" => Some(3),
            _ => None,
        },
        Object::Array(a) => match a.first()?.as_name().ok()? {
            b"CalGray" => Some(1),
            b"CalRGB" => Some(3),
            b"ICCBased" => {
                let profile = doc.get_object(a.get(1)?.as_reference().ok()?).ok()?.as_stream().ok()?;
                match profile.dict.get(b"N").ok()?.as_i64().ok()? {
                    n @ (1 | 3) => Some(n as u32),
                    _ => None,
                }
            }
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    /// A page with a 2400×3300 RGB photo-like image (a gradient with noise)
    /// drawn at letter size, stored losslessly: about 400 dpi.
    fn bloated_pdf() -> Document {
        let (w, h) = (2400u32, 3300u32);
        let mut seed = 1u32;
        let img = RgbImage::from_fn(w, h, |x, y| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let noise = (seed >> 24) as u8 / 8;
            image::Rgb([(x * 255 / w) as u8 ^ noise, (y * 255 / h) as u8, 128u8.wrapping_add(noise)])
        });
        let mut doc = crate::pages::tests::text_pdf(&["photo"]);
        let image_id = doc.add_object({
            let mut s = Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Image",
                    "Width" => w as i64, "Height" => h as i64,
                    "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
                },
                img.into_raw(),
            );
            s.compress().unwrap();
            s
        });
        let page = doc.get_pages()[&1];
        let content = doc.add_object(Stream::new(dictionary! {}, b"q 612 0 0 792 0 0 cm /Im0 Do Q".to_vec()));
        let dict = doc.get_dictionary_mut(page).unwrap();
        dict.set("Contents", content);
        dict.set("Resources", dictionary! { "XObject" => dictionary! { "Im0" => image_id } });
        doc
    }

    fn size(doc: &mut Document) -> usize {
        let mut out = Vec::new();
        doc.save_modern(&mut out).unwrap();
        out.len()
    }

    #[test]
    fn shrinks_oversized_images() {
        let mut doc = bloated_pdf();
        let before = size(&mut doc);
        let report = squish(&mut doc, Level::Medium);
        let after = size(&mut doc);
        assert_eq!((report.images, report.shrunk), (1, 1));
        assert!(after * 5 < before, "{before} → {after}");

        // 8.5 inches wide at 150 dpi; the height follows the image's own aspect ratio.
        let (_, image) = doc
            .objects
            .iter()
            .find(|(_, o)| o.as_stream().is_ok_and(|s| s.dict.has(b"Width")))
            .unwrap();
        let dict = &image.as_stream().unwrap().dict;
        assert_eq!(dict.get(b"Width").unwrap().as_i64().unwrap(), 1275);
        assert_eq!(dict.get(b"Height").unwrap().as_i64().unwrap(), 1753);
        assert_eq!(dict.get(b"Filter").unwrap().as_name().unwrap(), b"DCTDecode");

        // And it still opens and renders.
        let mut bytes = Vec::new();
        doc.save_modern(&mut bytes).unwrap();
        let thumbs = crate::render::thumbnails(std::sync::Arc::new(bytes), 100.0);
        assert!(thumbs[0].is_some());
    }

    #[test]
    fn stronger_is_smaller() {
        let sizes: Vec<usize> = Level::ALL
            .iter()
            .map(|&level| {
                let mut doc = bloated_pdf();
                squish(&mut doc, level);
                size(&mut doc)
            })
            .collect();
        assert!(sizes[0] > sizes[1] && sizes[1] > sizes[2], "{sizes:?}");
    }

    #[test]
    fn leaves_small_and_odd_images_alone() {
        let mut doc = crate::pdf::tests::fixture_pdf();
        // The fixture scan is already a modest JPEG drawn at page size.
        let report = squish(&mut doc, Level::Light);
        assert_eq!(report.shrunk, 0);
        assert!(Level::parse("STRONG") == Some(Level::Strong) && Level::parse("max").is_none());
    }
}
