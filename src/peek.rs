//! Peek: reads scanned pages and makes them searchable.
//!
//! Each page is drawn to a bitmap and read with ocrs, a pure-Rust OCR engine
//! whose models are built into the program, so nothing leaves the computer.
//! The words it finds are written back onto the page as invisible text (render
//! mode 3), each one stretched over the word it came from, so you can search,
//! select and copy it while the page looks exactly the same. Pages that
//! already have text are left alone.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use anyhow::{Context as _, Result};
use hayro::hayro_syntax::Pdf;
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, StringFormat, content::{Content, Operation}, dictionary};
use ocrs::{ImageSource, OcrEngine, OcrEngineParams, TextItem};
use rayon::prelude::*;
use rten::Model;

use crate::{content::Matrix, hush::invert, pdf::inherited, render, std_widths};

static DETECTION_MODEL: &[u8] = include_bytes!("../models/text-detection.rten");
static RECOGNITION_MODEL: &[u8] = include_bytes!("../models/text-recognition.rten");

/// Pixels per inch pages are read at. Body text comes out about 30 px tall,
/// which the models handle well, and a page takes well under a second.
const DPI: f32 = 200.0;

/// Helvetica's ascent and descent, as a fraction of the font size.
const ASCENT: f64 = 0.718;
const DESCENT: f64 = 0.207;

/// What Peek made of one page.
pub enum PageText {
    /// The page has text already, so it was left alone.
    HadText,
    /// The page couldn't be drawn, so it couldn't be read.
    Unreadable,
    /// The lines Peek read, top to bottom.
    Read(Vec<Line>),
}

pub struct Line {
    pub words: Vec<Word>,
}

impl Line {
    pub fn text(&self) -> String {
        self.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
    }
}

pub struct Word {
    pub text: String,
    /// Where the word sits, in page coordinates: bottom left, bottom right and
    /// top left corners. Three corners, not a box, so turned pages work.
    pub corners: [[f64; 2]; 3],
}

#[derive(Default)]
pub struct Report {
    pub read: usize,
    pub had_text: usize,
    pub words: usize,
}

pub fn default_output(input: &Path) -> PathBuf {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("document");
    input.with_file_name(format!("{stem}.searchable.pdf"))
}

/// The OCR engine, loaded once and shared.
fn engine() -> Result<&'static OcrEngine> {
    static ENGINE: OnceLock<Result<OcrEngine, String>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let load = || -> Result<OcrEngine> {
                OcrEngine::new(OcrEngineParams {
                    detection_model: Some(Model::load_static_slice(DETECTION_MODEL)?),
                    recognition_model: Some(Model::load_static_slice(RECOGNITION_MODEL)?),
                    ..Default::default()
                })
            };
            load().map_err(|e| format!("{e:#}"))
        })
        .as_ref()
        .map_err(|e| anyhow::anyhow!("couldn't start the OCR engine: {e}"))
}

/// Reads every page that doesn't have text yet. `data` is the file `doc` was
/// loaded from. `on_page` is called after each page, from any thread.
pub fn read(doc: &Document, data: Arc<Vec<u8>>, on_page: &(dyn Fn() + Sync)) -> Result<Vec<PageText>> {
    let engine = engine()?;
    let pages: Vec<ObjectId> = doc.get_pages().into_values().collect();
    let geometry = render::page_geometry(data.clone());
    let todo: Vec<bool> = pages.iter().map(|&id| !has_text(doc, id)).collect();

    Ok((0..pages.len())
        .into_par_iter()
        .map_init(
            || Pdf::new(data.clone()).ok(),
            |pdf, ix| {
                let result = if !todo[ix] {
                    PageText::HadText
                } else {
                    match (pdf.as_ref(), geometry.get(ix).copied().flatten()) {
                        (Some(pdf), Some((matrix, _))) => read_page(engine, pdf, ix, &matrix),
                        _ => PageText::Unreadable,
                    }
                };
                on_page();
                result
            },
        )
        .collect())
}

fn read_page(engine: &OcrEngine, pdf: &Pdf, ix: usize, to_rendering: &Matrix) -> PageText {
    let scale = DPI / 72.0;
    let (Some(image), Some(from_rendering)) = (render::render_at(pdf, ix, scale), invert(to_rendering)) else {
        return PageText::Unreadable;
    };
    let gray = image::DynamicImage::ImageRgba8(image).into_luma8();
    let lines = (|| -> Result<Vec<Line>> {
        let input = engine.prepare_input(ImageSource::from_bytes(gray.as_raw(), gray.dimensions())?)?;
        let words = engine.detect_words(&input)?;
        let lines = engine.find_text_lines(&input, &words);
        let lines = engine.recognize_text(&input, &lines)?;
        Ok(lines
            .iter()
            .flatten()
            .map(|line| Line {
                // Words take their height from the whole line, so "on" and
                // "Typography" get the same size of text.
                words: line
                    .words()
                    .map(|word| {
                        let (r, l) = (word.bounding_rect(), line.bounding_rect());
                        // Pixels → rendering at scale 1 → page.
                        let corner = |x: i32, y: i32| {
                            let (x, y) = (x as f64 / scale as f64, y as f64 / scale as f64);
                            let m = &from_rendering;
                            [m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]]
                        };
                        Word {
                            text: word.to_string(),
                            corners: [
                                corner(r.left(), l.bottom()),
                                corner(r.right(), l.bottom()),
                                corner(r.left(), l.top()),
                            ],
                        }
                    })
                    .collect(),
            })
            .filter(|line: &Line| !line.words.is_empty())
            .collect())
    })();
    match lines {
        Ok(lines) => PageText::Read(lines),
        Err(_) => PageText::Unreadable,
    }
}

/// Whether the page draws any text of its own.
fn has_text(doc: &Document, page: ObjectId) -> bool {
    let Ok(content) = doc.get_page_content(page) else { return false };
    let Ok(content) = Content::decode(&content) else { return false };
    content.operations.iter().any(|op| matches!(op.operator.as_str(), "Tj" | "TJ" | "'" | "\""))
}

/// Writes what [`read`] found onto the pages as invisible text.
pub fn add_text(doc: &mut Document, pages: &[PageText]) -> Result<Report> {
    let mut report = Report::default();
    let widths = std_widths::widths(b"Helvetica").context("missing Helvetica metrics")?;
    let mut font = None;
    for (&id, page) in doc.get_pages().values().zip(pages) {
        let lines = match page {
            PageText::HadText => {
                report.had_text += 1;
                continue;
            }
            PageText::Unreadable => continue,
            PageText::Read(lines) => lines,
        };
        report.read += 1;
        let font = *font.get_or_insert_with(|| {
            doc.add_object(dictionary! {
                "Type" => "Font",
                "Subtype" => "Type1",
                "BaseFont" => "Helvetica",
                "Encoding" => "WinAnsiEncoding",
            })
        });
        let name = add_font(doc, id, font)?;
        let mut ops = vec![
            Operation::new("Q", vec![]),
            Operation::new("q", vec![]),
            Operation::new("BT", vec![]),
            Operation::new("Tr", vec![3.into()]),
            Operation::new("Tf", vec![Object::Name(name.into_bytes()), 1.into()]),
        ];
        for word in lines.iter().flat_map(|l| &l.words) {
            let Some(placed) = place(word, widths) else { continue };
            ops.extend(placed);
            report.words += 1;
        }
        ops.extend([Operation::new("ET", vec![]), Operation::new("Q", vec![])]);
        let ops = Content { operations: ops }.encode()?;

        let before = doc.add_object(Stream::new(dictionary! {}, b"q\n".to_vec()));
        let after = doc.add_object(Stream::new(dictionary! {}, ops));
        let page = doc.get_dictionary_mut(id).context("page is not a dictionary")?;
        let mut contents = match page.get(b"Contents") {
            Ok(Object::Array(items)) => items.clone(),
            Ok(other) => vec![other.clone()],
            Err(_) => Vec::new(),
        };
        contents.insert(0, before.into());
        contents.push(after.into());
        page.set("Contents", contents);
    }
    Ok(report)
}

/// The text operators that put `word` over where it was read, stretched to fit.
fn place(word: &Word, widths: &[u16; 224]) -> Option<[Operation; 2]> {
    let codes: Vec<u8> = word.text.chars().map(win_ansi).collect();
    let width: f64 = codes.iter().map(|&c| widths[(c - 32) as usize] as f64 / 1000.0).sum();
    let [p0, p1, p2] = word.corners;
    let along = [p1[0] - p0[0], p1[1] - p0[1]];
    let up = [p2[0] - p0[0], p2[1] - p0[1]];
    let height = up[0].hypot(up[1]);
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let size = height / (ASCENT + DESCENT);
    let unit_up = [up[0] / height, up[1] / height];
    let origin = [p0[0] + unit_up[0] * DESCENT * size, p0[1] + unit_up[1] * DESCENT * size];
    let tm = [
        along[0] / width,
        along[1] / width,
        unit_up[0] * size,
        unit_up[1] * size,
        origin[0],
        origin[1],
    ];
    // Three decimals is plenty, and keeps the stream small.
    let tm = tm.iter().map(|v| Object::Real(((v * 1000.0).round() / 1000.0) as f32)).collect();
    Some([
        Operation::new("Tm", tm),
        Operation::new("Tj", vec![Object::String(codes, StringFormat::Literal)]),
    ])
}

/// The WinAnsi code for `c`, or `?` for characters it doesn't have.
fn win_ansi(c: char) -> u8 {
    match c {
        ' '..='~' => c as u8,
        '€' => 0x80,
        '‘' => 0x91,
        '’' => 0x92,
        '“' => 0x93,
        '”' => 0x94,
        '–' => 0x96,
        '—' => 0x97,
        '\u{a0}'..='\u{ff}' => c as u8,
        _ => b'?',
    }
}

/// Adds `font` to the page's own resources and returns the name it got.
fn add_font(doc: &mut Document, page: ObjectId, font: ObjectId) -> Result<String> {
    let resolve = |doc: &Document, obj: &Object| -> Dictionary {
        match obj {
            Object::Reference(id) => doc.get_dictionary(*id).cloned().unwrap_or_default(),
            Object::Dictionary(d) => d.clone(),
            _ => Dictionary::new(),
        }
    };
    let mut resources = inherited(doc, page, b"Resources").map(|r| resolve(doc, r)).unwrap_or_default();
    let mut fonts = resources.get(b"Font").map(|f| resolve(doc, f)).unwrap_or_default();
    let name = (0..)
        .map(|n| if n == 0 { "PeekText".to_string() } else { format!("PeekText{n}") })
        .find(|name| !fonts.has(name.as_bytes()))
        .expect("some name is free");
    fonts.set(name.as_bytes(), font);
    resources.set("Font", fonts);
    doc.get_dictionary_mut(page)
        .context("page is not a dictionary")?
        .set("Resources", resources);
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::tests::text_pdf;

    /// A "scan": the text PDF drawn to an image, with no text left in it.
    fn scanned(labels: &[&str]) -> Document {
        let mut doc = text_pdf(labels);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        let pdf = Pdf::new(Arc::new(bytes)).unwrap();
        let mut scan = Document::with_version("1.7");
        let tree = scan.new_object_id();
        let kids: Vec<Object> = (0..labels.len())
            .map(|ix| {
                let image = image::DynamicImage::ImageRgba8(render::render_at(&pdf, ix, 2.0).unwrap()).into_luma8();
                let (w, h) = image.dimensions();
                let image = scan.add_object(Stream::new(
                    dictionary! {
                        "Type" => "XObject", "Subtype" => "Image", "Width" => w as i64, "Height" => h as i64,
                        "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8,
                    },
                    image.into_raw(),
                ));
                let content = scan.add_object(Stream::new(dictionary! {}, b"q 612 0 0 792 0 0 cm /Im0 Do Q".to_vec()));
                scan.add_object(dictionary! {
                    "Type" => "Page", "Parent" => tree, "Contents" => content,
                    "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                    "Resources" => dictionary! { "XObject" => dictionary! { "Im0" => image } },
                })
                .into()
            })
            .collect();
        scan.objects.insert(
            tree,
            Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => labels.len() as i64 }),
        );
        let catalog = scan.add_object(dictionary! { "Type" => "Catalog", "Pages" => tree });
        scan.trailer.set("Root", catalog);
        scan
    }

    fn bytes(doc: &mut Document) -> Arc<Vec<u8>> {
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        Arc::new(bytes)
    }

    #[test]
    fn makes_scans_searchable() {
        let mut doc = scanned(&["Hello monsters", "Invoice 2024"]);
        let data = bytes(&mut doc);
        let pages = read(&doc, data, &|| {}).unwrap();
        let texts: Vec<String> = pages
            .iter()
            .map(|p| match p {
                PageText::Read(lines) => lines.iter().map(Line::text).collect::<Vec<_>>().join("\n"),
                _ => panic!("page wasn't read"),
            })
            .collect();
        assert_eq!(texts, ["Hello monsters", "Invoice 2024"]);

        let report = add_text(&mut doc, &pages).unwrap();
        assert_eq!((report.read, report.had_text, report.words), (2, 0, 4));
        let saved = Document::load_mem(&bytes(&mut doc)).unwrap();
        let first = saved.extract_text(&[1]).unwrap();
        assert!(first.contains("Hello") && first.contains("monsters"), "{first:?}");

        // The word sits where it was drawn: 24 pt text at (72, 720).
        let PageText::Read(lines) = &pages[0] else { unreachable!() };
        let [bottom_left, _, top_left] = lines[0].words[0].corners;
        assert!((bottom_left[0] - 72.0).abs() < 6.0, "{bottom_left:?}");
        assert!((bottom_left[1] - 715.0).abs() < 8.0, "{bottom_left:?}");
        assert!((top_left[1] - bottom_left[1] - 22.0).abs() < 8.0, "{top_left:?}");

        // Running it again leaves the now-searchable pages alone.
        let again = read(&saved, bytes(&mut saved.clone()), &|| {}).unwrap();
        assert!(again.iter().all(|p| matches!(p, PageText::HadText)));
    }

    #[test]
    fn skips_pages_with_text() {
        let mut doc = text_pdf(&["already text"]);
        let data = bytes(&mut doc);
        let pages = read(&doc, data, &|| {}).unwrap();
        assert!(matches!(pages[..], [PageText::HadText]));
        assert_eq!(add_text(&mut doc, &pages).unwrap().had_text, 1);
    }
}
