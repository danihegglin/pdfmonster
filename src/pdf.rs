//! Reading scanned pages out of a PDF and writing them back straightened.
//!
//! Straightening is lossless: the scanned images are never re-encoded. Each
//! page's content stream is wrapped in a rotation matrix around the page centre,
//! on top of a white background so the uncovered corners stay clean.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use image::GrayImage;
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use rayon::prelude::*;

use crate::deskew::{self, Skew};

/// Longest side of the preview kept per page.
const PREVIEW_SIZE: u32 = 720;
/// Images smaller than this (on either side) are logos or icons, not page scans.
const MIN_SCAN_SIDE: i64 = 300;
/// Anything below this is treated as already straight.
pub const MIN_CORRECTION: f32 = 0.05;
/// Below this the detected lines are not trustworthy.
const MIN_CONFIDENCE: f32 = 1.3;

pub struct Page {
    pub number: u32,
    pub id: ObjectId,
    /// Downscaled grayscale scan, as stored in the PDF (before /Rotate).
    pub preview: Option<GrayImage>,
    /// The page's /Rotate, normalised to 0, 90, 180 or 270 (clockwise).
    pub rotate: u32,
    pub skew: Option<Skew>,
    /// Why a page could not be analysed.
    pub note: Option<&'static str>,
}

impl Page {
    /// The correction we'd apply without user input.
    pub fn suggested_angle(&self) -> f32 {
        match self.skew {
            Some(s) if s.confidence >= MIN_CONFIDENCE && s.degrees.abs() >= MIN_CORRECTION => {
                s.degrees
            }
            _ => 0.0,
        }
    }
}

pub fn open(path: &Path) -> Result<Document> {
    Document::load(path).with_context(|| format!("could not open {}", path.display()))
}

/// Finds the scan on every page and measures its skew, all pages in parallel.
pub fn analyze(doc: &Document) -> Vec<Page> {
    let pages: Vec<(u32, ObjectId)> = doc.get_pages().into_iter().collect();
    pages
        .into_par_iter()
        .map(|(number, id)| {
            let rotate = inherited(doc, id, b"Rotate")
                .and_then(|o| o.as_i64().ok())
                .unwrap_or(0);
            let rotate = (rotate.rem_euclid(360) / 90 * 90) as u32;
            let quarter_turn = rotate % 180 == 90;
            let mut page = Page {
                number,
                id,
                preview: None,
                rotate,
                skew: None,
                note: None,
            };
            match scan_image(doc, id) {
                Ok(img) => {
                    page.skew = Some(deskew::detect(&img, quarter_turn));
                    page.preview = Some(deskew::downscale(&img, PREVIEW_SIZE));
                }
                Err(note) => page.note = Some(note),
            }
            page
        })
        .collect()
}

/// Rotates the given pages counter-clockwise by the given angles (degrees) and saves the result.
pub fn save_straightened(
    mut doc: Document,
    corrections: &[(ObjectId, f32)],
    out: &Path,
) -> Result<usize> {
    let mut changed = 0;
    for &(page_id, degrees) in corrections {
        if degrees.abs() < MIN_CORRECTION {
            continue;
        }
        rotate_page(&mut doc, page_id, degrees)?;
        changed += 1;
    }
    doc.save(out)
        .with_context(|| format!("could not write {}", out.display()))?;
    Ok(changed)
}

pub fn default_output(input: &Path) -> PathBuf {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("document");
    input.with_file_name(format!("{stem}.aligned.pdf"))
}

fn rotate_page(doc: &mut Document, page_id: ObjectId, degrees: f32) -> Result<()> {
    let media = inherited(doc, page_id, b"MediaBox")
        .and_then(|o| rect(doc, o))
        .unwrap_or([0.0, 0.0, 612.0, 792.0]);
    let visible = inherited(doc, page_id, b"CropBox")
        .and_then(|o| rect(doc, o))
        .unwrap_or(media);
    let cx = (visible[0] + visible[2]) / 2.0;
    let cy = (visible[1] + visible[3]) / 2.0;

    // PDF space is y-up, so a positive angle turns counter-clockwise.
    let (s, c) = (degrees as f64).to_radians().sin_cos();
    let e = cx - c * cx + s * cy;
    let f = cy - s * cx - c * cy;
    let [x0, y0, x1, y1] = media;
    let prefix = format!(
        "q 1 g {x0:.3} {y0:.3} {:.3} {:.3} re f {c:.6} {s:.6} {:.6} {c:.6} {e:.4} {f:.4} cm\n",
        x1 - x0,
        y1 - y0,
        -s,
    );

    let mut contents: Vec<Object> = vec![doc.add_object(Stream::new(Dictionary::new(), prefix.into_bytes())).into()];
    contents.extend(doc.get_page_contents(page_id).into_iter().map(Object::Reference));
    contents.push(doc.add_object(Stream::new(Dictionary::new(), b"\nQ\n".to_vec())).into());

    doc.get_dictionary_mut(page_id)?
        .set("Contents", Object::Array(contents));
    Ok(())
}

/// Looks a page attribute up, following the page tree for inherited values.
fn inherited<'a>(doc: &'a Document, page_id: ObjectId, key: &[u8]) -> Option<&'a Object> {
    let mut dict = doc.get_dictionary(page_id).ok()?;
    for _ in 0..32 {
        if let Ok(value) = dict.get(key) {
            return Some(value);
        }
        let parent = dict.get(b"Parent").ok()?.as_reference().ok()?;
        dict = doc.get_dictionary(parent).ok()?;
    }
    None
}

fn rect(doc: &Document, obj: &Object) -> Option<[f64; 4]> {
    let obj = match obj {
        Object::Reference(id) => doc.get_object(*id).ok()?,
        o => o,
    };
    let arr = obj.as_array().ok()?;
    let n: Vec<f64> = arr.iter().filter_map(|o| o.as_float().ok().map(f64::from)).collect();
    let [a, b, c, d] = n[..] else { return None };
    Some([a.min(c), b.min(d), a.max(c), b.max(d)])
}

/// Decodes the largest image on the page as grayscale.
fn scan_image(doc: &Document, page_id: ObjectId) -> Result<GrayImage, &'static str> {
    let images = doc.get_page_images(page_id).map_err(|_| "unreadable page")?;
    let img = images
        .iter()
        .filter(|i| i.width >= MIN_SCAN_SIDE && i.height >= MIN_SCAN_SIDE)
        .max_by_key(|i| i.width * i.height)
        .ok_or("no scanned image")?;
    let (w, h) = (img.width as u32, img.height as u32);
    let filters: Vec<&str> = img.filters.iter().flatten().map(String::as_str).collect();

    match filters.as_slice() {
        ["DCTDecode"] => image::load_from_memory_with_format(img.content, image::ImageFormat::Jpeg)
            .map(|i| i.to_luma8())
            .map_err(|_| "undecodable JPEG"),
        f if f.iter().all(|f| matches!(*f, "FlateDecode" | "LZWDecode" | "ASCII85Decode")) => {
            let data = if f.is_empty() {
                img.content.to_vec()
            } else {
                let stream = doc
                    .get_object(img.id)
                    .and_then(Object::as_stream)
                    .map_err(|_| "unreadable image")?;
                stream.decompressed_content().map_err(|_| "undecodable image")?
            };
            let bpc = img.bits_per_component.unwrap_or(8) as u32;
            let invert = img
                .origin_dict
                .get(b"Decode")
                .and_then(Object::as_array)
                .ok()
                .and_then(|d| d.first())
                .and_then(|v| v.as_float().ok())
                .is_some_and(|v| v > 0.5);
            raw_to_gray(&data, w, h, bpc, invert).ok_or("unsupported image layout")
        }
        _ => Err("unsupported image encoding (CCITT/JBIG2/JPX)"),
    }
}

/// Converts uncompressed PDF image samples (1/8 bpc, 1/3/4 components) to grayscale.
fn raw_to_gray(data: &[u8], w: u32, h: u32, bpc: u32, invert: bool) -> Option<GrayImage> {
    let comps = [1u32, 3, 4]
        .into_iter()
        .rev()
        .find(|&c| ((w * c * bpc).div_ceil(8) * h) as usize <= data.len())?;
    if !matches!((bpc, comps), (1, 1) | (8, _)) {
        return None;
    }
    let row_bytes = (w * comps * bpc).div_ceil(8) as usize;
    let mut out = GrayImage::from_fn(w, h, |x, y| {
        let row = &data[y as usize * row_bytes..];
        let v = match (bpc, comps) {
            (1, 1) => {
                let bit = row[x as usize / 8] >> (7 - x % 8) & 1;
                bit * 255
            }
            (8, 1) => row[x as usize],
            (8, 3) => {
                let p = &row[x as usize * 3..];
                ((p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8) as u8
            }
            (8, 4) => {
                let p = &row[x as usize * 4..];
                let ink = (p[0] as u32 + p[1] as u32 + p[2] as u32) / 3 + p[3] as u32;
                255 - ink.min(255) as u8
            }
            _ => unreachable!(),
        };
        image::Luma([v])
    });
    if invert {
        image::imageops::invert(&mut out);
    }
    Some(out)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use lopdf::dictionary;

    /// Builds a one-page PDF with the fixture scan embedded as a JPEG.
    pub fn fixture_pdf() -> Document {
        let img = image::open(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/skewed-scan.png"))
            .unwrap()
            .to_luma8();
        let (w, h) = img.dimensions();
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85)
            .encode_image(&img)
            .unwrap();

        let mut doc = Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let image_id = doc.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image",
                "Width" => w as i64, "Height" => h as i64,
                "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8,
                "Filter" => "DCTDecode",
            },
            jpeg,
        ));
        let content = doc.add_object(Stream::new(dictionary! {}, b"q 595 0 0 842 0 0 cm /Im0 Do Q".to_vec()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content,
            "Resources" => dictionary! { "XObject" => dictionary! { "Im0" => image_id } },
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            }),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        doc
    }

    #[test]
    fn analyzes_and_straightens() {
        let doc = fixture_pdf();
        let pages = analyze(&doc);
        assert_eq!(pages.len(), 1);
        let angle = pages[0].suggested_angle();
        assert!(angle > 0.5 && angle < 1.5, "{angle}");

        let out = std::env::temp_dir().join("pdfmonster-test.aligned.pdf");
        let changed = save_straightened(doc, &[(pages[0].id, angle)], &out).unwrap();
        assert_eq!(changed, 1);

        let saved = Document::load(&out).unwrap();
        let page = *saved.get_pages().get(&1).unwrap();
        let content = String::from_utf8(saved.get_page_content(page).unwrap()).unwrap();
        assert!(content.starts_with("q 1 g"), "{content}");
        assert!(content.contains("/Im0 Do"));
        assert!(content.trim_end().ends_with('Q'));
    }

    #[test]
    fn raw_one_bit() {
        // 8x1 image: alternating black/white, then inverted by /Decode [1 0].
        let g = raw_to_gray(&[0b1010_1010], 8, 1, 1, false).unwrap();
        assert_eq!(g.get_pixel(0, 0).0[0], 255);
        assert_eq!(g.get_pixel(1, 0).0[0], 0);
        let g = raw_to_gray(&[0b1010_1010], 8, 1, 1, true).unwrap();
        assert_eq!(g.get_pixel(0, 0).0[0], 0);
    }
}
