//! Hush: redacts for real and wipes hidden metadata.
//!
//! Redaction removes what's under each box from the file, rather than only
//! covering it:
//! - text: each glyph that touches a box is taken out of its text-showing
//!   operator and replaced by an equal amount of spacing, so the rest of the
//!   line doesn't move;
//! - images: the pixels under the box are painted black in a copy of the image
//!   (other pages that share the image keep the original);
//! - anything Hush can't take apart safely (form XObjects, inline images,
//!   images in formats it can't decode, text in fonts it can't measure) is
//!   removed entirely if it touches a box, and reported;
//! - annotations and form fields touching a box are deleted;
//! - finally a black box is painted on top.

use std::collections::{HashMap, HashSet};

use anyhow::{Context as _, Result};
use image::{DynamicImage, GrayImage, RgbImage};
use lopdf::{
    Dictionary, Document, Object, ObjectId, Stream, StringFormat,
    Encoding,
    content::{Content, Operation},
};

use crate::{
    content::{IDENTITY, Matrix, multiply, number, resolve_dict},
    pages,
    pdf::inherited,
};

/// A box to black out, in the page's own coordinates (PDF points, y up,
/// before the page's /Rotate).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    /// 1-based page number.
    pub page: u32,
    pub rect: [f64; 4],
}

#[derive(Debug, Default)]
pub struct Report {
    pub glyphs: usize,
    pub images: usize,
    pub annotations: usize,
    /// Things that touched a box and were removed whole, because Hush couldn't
    /// cut them precisely.
    pub removed_whole: Vec<String>,
}

/// Glyph boxes reach this far below the baseline and above it, in units of the font size.
const DESCENT: f64 = 0.25;
const ASCENT: f64 = 0.9;

pub fn default_output(input: &std::path::Path) -> std::path::PathBuf {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("document");
    input.with_file_name(format!("{stem}.hushed.pdf"))
}

/// Finds every occurrence of `needle` (case-insensitive) and returns a box per match.
pub fn find_text(doc: &Document, needle: &str) -> Vec<Area> {
    let needle: Vec<char> = needle.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut found = Vec::new();
    for (number, page_id) in doc.get_pages() {
        let Some(walk) = Walk::page(doc, page_id) else { continue };
        // All the glyphs on the page in drawing order, spaces dropped so that
        // word spacing done with positioning instead of space characters still matches.
        let glyphs: Vec<(char, [f64; 4])> = walk
            .runs
            .iter()
            .flat_map(|run| run.glyphs.iter())
            .flat_map(|g| g.text.to_lowercase().chars().map(|c| (c, g.bbox)).collect::<Vec<_>>())
            .filter(|(c, _)| !c.is_whitespace())
            .collect();
        let mut i = 0;
        while i + needle.len() <= glyphs.len() {
            if glyphs[i..i + needle.len()].iter().map(|g| g.0).eq(needle.iter().copied()) {
                // A match can wrap onto the next line: one box per line.
                let mut current: Option<[f64; 4]> = None;
                for &(_, b) in &glyphs[i..i + needle.len()] {
                    current = match current {
                        Some(c) if overlaps_vertically(&c, &b) => Some(union(&c, &b)),
                        Some(c) => {
                            found.push(Area { page: number, rect: c });
                            Some(b)
                        }
                        None => Some(b),
                    };
                }
                found.extend(current.map(|rect| Area { page: number, rect }));
                i += needle.len();
            } else {
                i += 1;
            }
        }
    }
    found
}

/// Removes everything under the areas from the document and paints them black.
pub fn redact(doc: &mut Document, areas: &[Area]) -> Result<Report> {
    let mut report = Report::default();
    let pages = doc.get_pages();
    let mut by_page: HashMap<u32, Vec<[f64; 4]>> = HashMap::new();
    for area in areas {
        anyhow::ensure!(pages.contains_key(&area.page), "there is no page {}", area.page);
        by_page.entry(area.page).or_default().push(normalize(area.rect));
    }
    for (number, rects) in by_page {
        redact_page(doc, number, pages[&number], &rects, &mut report)?;
    }
    pages::prune_unreachable(doc);
    Ok(report)
}

fn redact_page(
    doc: &mut Document,
    page_number: u32,
    page_id: ObjectId,
    rects: &[[f64; 4]],
    report: &mut Report,
) -> Result<()> {
    let walk = Walk::page(doc, page_id).context("could not read the page's content")?;
    let hits = |b: &[f64; 4]| rects.iter().any(|r| intersects(r, b));

    let mut ops = walk.operations;
    let mut replace: HashMap<usize, Vec<Operation>> = HashMap::new();
    let mut drop: HashSet<usize> = HashSet::new();

    for run in &walk.runs {
        if !run.measurable {
            if hits(&run.extent) {
                drop.insert(run.op);
                report.removed_whole.push(format!("page {page_number}: a run of text in a font Hush can't measure"));
            }
            continue;
        }
        let cut: Vec<bool> = run.glyphs.iter().map(|g| hits(&g.bbox)).collect();
        if !cut.contains(&true) {
            continue;
        }
        report.glyphs += cut.iter().filter(|&&c| c).count();
        replace.insert(run.op, rewrite_run(&ops[run.op], run, &cut));
    }

    // Images get a redacted copy, wired up under a new name in the page's own resources.
    let mut renamed: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut new_xobjects: Vec<(Vec<u8>, ObjectId)> = Vec::new();
    for draw in &walk.draws {
        let bbox = transform_rect(&[0.0, 0.0, 1.0, 1.0], &draw.ctm);
        let inside: Vec<&[f64; 4]> = rects.iter().filter(|r| intersects(r, &bbox)).collect();
        if inside.is_empty() {
            continue;
        }
        match draw.kind {
            DrawKind::Image(id) => match burn_image(doc, id, &draw.ctm, &inside) {
                Some(stream) => {
                    let copy = doc.add_object(stream);
                    let name = format!("HushIm{}", copy.0).into_bytes();
                    renamed.push((draw.op, name.clone()));
                    new_xobjects.push((name, copy));
                    report.images += 1;
                }
                None => {
                    drop.insert(draw.op);
                    report.removed_whole.push(format!("page {page_number}: an image in a format Hush can't edit"));
                }
            },
            DrawKind::Form => {
                drop.insert(draw.op);
                report.removed_whole.push(format!("page {page_number}: a form XObject (embedded graphics)"));
            }
            DrawKind::Inline => {
                drop.insert(draw.op);
                report.removed_whole.push(format!("page {page_number}: an inline image"));
            }
        }
    }

    for (op, name) in renamed {
        ops[op].operands = vec![Object::Name(name)];
    }
    let mut out: Vec<Operation> = vec![Operation::new("q", vec![])];
    for (ix, op) in ops.into_iter().enumerate() {
        if drop.contains(&ix) {
            // Keep the text position moving even though the text is gone.
            if matches!(op.operator.as_str(), "'" | "\"") {
                out.push(Operation::new("T*", vec![]));
            }
            continue;
        }
        match replace.remove(&ix) {
            Some(new) => out.extend(new),
            None => out.push(op),
        }
    }
    out.push(Operation::new("Q", vec![]));
    out.push(Operation::new("q", vec![]));
    out.push(Operation::new("g", vec![0.into()]));
    for r in rects {
        out.push(Operation::new(
            "re",
            vec![r[0].into(), r[1].into(), (r[2] - r[0]).into(), (r[3] - r[1]).into()],
        ));
    }
    out.push(Operation::new("f", vec![]));
    out.push(Operation::new("Q", vec![]));

    let content = doc.add_object(Stream::new(Dictionary::new(), encode(&out)));
    let mut resources = inherited(doc, page_id, b"Resources")
        .and_then(|r| resolve_dict(doc, r))
        .cloned()
        .unwrap_or_default();
    if !new_xobjects.is_empty() {
        let mut xobjects = resources
            .get(b"XObject")
            .ok()
            .and_then(|x| resolve_dict(doc, x))
            .cloned()
            .unwrap_or_default();
        for (name, id) in new_xobjects {
            xobjects.set(name, id);
        }
        resources.set("XObject", xobjects);
    }

    // Annotations (comments, links, form fields) touching a box go too.
    let annots: Vec<Object> = doc
        .get_dictionary(page_id)?
        .get(b"Annots")
        .ok()
        .and_then(|a| match a {
            Object::Reference(id) => doc.get_object(*id).ok(),
            other => Some(other),
        })
        .and_then(|a| a.as_array().ok())
        .cloned()
        .unwrap_or_default();
    let mut gone: HashSet<ObjectId> = HashSet::new();
    let kept: Vec<Object> = annots
        .into_iter()
        .filter(|a| {
            let rect = match a {
                Object::Reference(id) => doc.get_dictionary(*id).ok(),
                Object::Dictionary(d) => Some(d),
                _ => None,
            }
            .and_then(|d| d.get(b"Rect").ok())
            .and_then(|r| r.as_array().ok())
            .map(|r| r.iter().filter_map(number).collect::<Vec<_>>());
            match rect.as_deref() {
                Some(&[a0, b0, c0, d0]) if hits(&normalize([a0, b0, c0, d0])) => {
                    report.annotations += 1;
                    gone.extend(a.as_reference().ok());
                    false
                }
                _ => true,
            }
        })
        .collect();

    let page = doc.get_dictionary_mut(page_id)?;
    page.set("Contents", content);
    page.set("Resources", resources);
    page.set("Annots", kept);
    // Removed form fields must also leave the document's form, or they'd stay reachable.
    if !gone.is_empty() {
        let root = doc.trailer.get(b"Root")?.as_reference()?;
        let form = doc.get_dictionary(root)?.get(b"AcroForm").ok().cloned();
        let form_id = form.as_ref().and_then(|f| f.as_reference().ok());
        let mut form_dict = match (&form, form_id) {
            (_, Some(id)) => doc.get_dictionary(id).ok().cloned(),
            (Some(Object::Dictionary(d)), None) => Some(d.clone()),
            _ => None,
        };
        if let Some(dict) = &mut form_dict {
            if let Ok(fields) = dict.get(b"Fields").and_then(Object::as_array) {
                let fields: Vec<Object> = fields
                    .iter()
                    .filter(|f| !f.as_reference().is_ok_and(|id| gone.contains(&id)))
                    .cloned()
                    .collect();
                dict.set("Fields", fields);
            }
            match form_id {
                Some(id) => {
                    doc.objects.insert(id, Object::Dictionary(dict.clone()));
                }
                None => doc.get_dictionary_mut(root)?.set("AcroForm", dict.clone()),
            }
        }
    }
    Ok(())
}

/// Rebuilds a text-showing operator without the cut glyphs, keeping the
/// positions of the others.
fn rewrite_run(op: &Operation, run: &Run, cut: &[bool]) -> Vec<Operation> {
    let mut prefix = Vec::new();
    match op.operator.as_str() {
        "'" => prefix.push(Operation::new("T*", vec![])),
        "\"" => {
            prefix.push(Operation::new("Tw", vec![op.operands[0].clone()]));
            prefix.push(Operation::new("Tc", vec![op.operands[1].clone()]));
            prefix.push(Operation::new("T*", vec![]));
        }
        _ => {}
    }
    let mut parts: Vec<Object> = Vec::new();
    let mut pending: Vec<u8> = Vec::new();
    let flush = |pending: &mut Vec<u8>, parts: &mut Vec<Object>| {
        if !pending.is_empty() {
            parts.push(Object::String(std::mem::take(pending), StringFormat::Hexadecimal));
        }
    };
    for piece in &run.pieces {
        match *piece {
            Piece::Glyph(i) if !cut[i] => pending.extend_from_slice(&run.glyphs[i].code),
            Piece::Glyph(i) => {
                flush(&mut pending, &mut parts);
                parts.push(Object::Real(-(run.glyphs[i].advance * 1000.0 / run.scale) as f32));
            }
            Piece::Adjust(n) => {
                flush(&mut pending, &mut parts);
                parts.push(Object::Real(n as f32));
            }
        }
    }
    flush(&mut pending, &mut parts);
    prefix.push(Operation::new("TJ", vec![Object::Array(parts)]));
    prefix
}

/// A copy of the image with the parts under `rects` painted black, or `None`
/// if the image is in a format Hush can't safely decode and re-encode.
fn burn_image(doc: &Document, id: ObjectId, ctm: &Matrix, rects: &[&[f64; 4]]) -> Option<Stream> {
    let stream = doc.get_object(id).ok()?.as_stream().ok()?;
    let dict = &stream.dict;
    let get_int = |key: &[u8]| dict.get(key).ok().and_then(|o| o.as_i64().ok());
    let (w, h) = (get_int(b"Width")? as u32, get_int(b"Height")? as u32);
    let bpc = get_int(b"BitsPerComponent").unwrap_or(8);
    let filters: Vec<&[u8]> = match dict.get(b"Filter") {
        Ok(Object::Name(n)) => vec![n.as_slice()],
        Ok(Object::Array(a)) => a.iter().filter_map(|f| f.as_name().ok()).collect(),
        _ => vec![],
    };
    let space = dict.get(b"ColorSpace").ok().map(|s| match s {
        Object::Reference(id) => doc.get_object(*id).ok().cloned().unwrap_or(Object::Null),
        other => other.clone(),
    });
    let gray = matches!(&space, Some(Object::Name(n)) if n == b"DeviceGray");
    let rgb = matches!(&space, Some(Object::Name(n)) if n == b"DeviceRGB");
    let mask = dict.get(b"ImageMask").and_then(Object::as_bool).unwrap_or(false);

    // Image pixels the boxes cover. The unit square maps onto the image upside down.
    let inverse = invert(ctm)?;
    let cover: Vec<[u32; 4]> = rects
        .iter()
        .map(|r| {
            let u = transform_rect(r, &inverse);
            let x0 = (u[0].clamp(0.0, 1.0) * w as f64).floor() as u32;
            let x1 = (u[2].clamp(0.0, 1.0) * w as f64).ceil() as u32;
            let y0 = ((1.0 - u[3].clamp(0.0, 1.0)) * h as f64).floor() as u32;
            let y1 = ((1.0 - u[1].clamp(0.0, 1.0)) * h as f64).ceil() as u32;
            [x0, y0, x1.min(w), y1.min(h)]
        })
        .collect();
    let covered = |x: u32, y: u32| cover.iter().any(|c| x >= c[0] && x < c[2] && y >= c[1] && y < c[3]);

    let mut dict = dict.clone();
    let content = match filters.as_slice() {
        [b"DCTDecode"] if !mask => {
            let img = image::load_from_memory_with_format(&stream.content, image::ImageFormat::Jpeg).ok()?;
            let img = match img {
                DynamicImage::ImageLuma8(mut g) => {
                    g.enumerate_pixels_mut().filter(|(x, y, _)| covered(*x, *y)).for_each(|(_, _, p)| p.0 = [0]);
                    DynamicImage::ImageLuma8(g)
                }
                other => {
                    // CMYK comes back as RGB; it's stored as RGB from now on.
                    let mut rgb = other.to_rgb8();
                    rgb.enumerate_pixels_mut().filter(|(x, y, _)| covered(*x, *y)).for_each(|(_, _, p)| p.0 = [0; 3]);
                    dict.set("ColorSpace", "DeviceRGB");
                    dict.remove(b"Decode");
                    DynamicImage::ImageRgb8(rgb)
                }
            };
            let mut jpeg = Vec::new();
            img.write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 92)).ok()?;
            jpeg
        }
        f if f.iter().all(|f| matches!(*f, b"FlateDecode" | b"LZWDecode")) && !dict.has(b"Decode") => {
            let mut data = stream.get_plain_content().ok()?;
            match (bpc, gray || mask, rgb) {
                (8, true, _) => {
                    let mut g = GrayImage::from_raw(w, h, data.get(..(w * h) as usize)?.to_vec())?;
                    g.enumerate_pixels_mut().filter(|(x, y, _)| covered(*x, *y)).for_each(|(_, _, p)| p.0 = [0]);
                    data = g.into_raw();
                }
                (8, _, true) => {
                    let mut c = RgbImage::from_raw(w, h, data.get(..(w * h * 3) as usize)?.to_vec())?;
                    c.enumerate_pixels_mut().filter(|(x, y, _)| covered(*x, *y)).for_each(|(_, _, p)| p.0 = [0; 3]);
                    data = c.into_raw();
                }
                (1, true, _) => {
                    // A 0 bit is black in a gray image and painted in an image
                    // mask; either way, what was there is gone.
                    let row = w.div_ceil(8) as usize;
                    data.get(..row * h as usize)?;
                    for y in 0..h {
                        for x in 0..w {
                            if covered(x, y) {
                                data[y as usize * row + x as usize / 8] &= !(0x80 >> (x % 8));
                            }
                        }
                    }
                }
                _ => return None,
            }
            dict.remove(b"DecodeParms");
            dict.remove(b"Filter");
            data
        }
        _ => return None,
    };
    let mut copy = Stream::new(dict, content);
    if filters.as_slice() != [b"DCTDecode".as_slice()] {
        copy.compress().ok()?;
    } else {
        copy.allows_compression = false;
    }
    Some(copy)
}

/// What's known about a font, for measuring and reading its glyphs.
struct Font<'a> {
    two_byte: bool,
    /// Glyph widths in thousandths of the font size, by character code.
    widths: HashMap<u32, f64>,
    default_width: f64,
    /// Whether `widths` actually comes from the file (otherwise it's a guess).
    measured: bool,
    encoding: Option<Encoding<'a>>,
}

impl<'a> Font<'a> {
    fn load(doc: &'a Document, dict: &'a Dictionary) -> Font<'a> {
        let encoding = dict.get_font_encoding(doc).ok();
        let subtype = dict.get(b"Subtype").and_then(Object::as_name).unwrap_or(b"");
        let deref = |o: &'a Object| match o {
            Object::Reference(id) => doc.get_object(*id).ok(),
            other => Some(other),
        };
        if subtype == b"Type0" {
            let identity = matches!(dict.get(b"Encoding").and_then(Object::as_name), Ok(b"Identity-H"));
            let descendant = dict
                .get(b"DescendantFonts")
                .ok()
                .and_then(deref)
                .and_then(|d| d.as_array().ok())
                .and_then(|a| a.first())
                .and_then(deref)
                .and_then(|d| d.as_dict().ok());
            let mut widths = HashMap::new();
            if let Some(w) = descendant.and_then(|d| d.get(b"W").ok()).and_then(deref).and_then(|w| w.as_array().ok()) {
                let mut i = 0;
                while i + 1 < w.len() {
                    let first = number(&w[i]).unwrap_or(0.0) as u32;
                    match deref(&w[i + 1]) {
                        Some(Object::Array(list)) => {
                            for (k, v) in list.iter().enumerate() {
                                widths.insert(first + k as u32, number(v).unwrap_or(0.0));
                            }
                            i += 2;
                        }
                        Some(last) if i + 2 < w.len() => {
                            let last = number(last).unwrap_or(0.0) as u32;
                            let width = number(&w[i + 2]).unwrap_or(0.0);
                            for c in first..=last.min(first + 65535) {
                                widths.insert(c, width);
                            }
                            i += 3;
                        }
                        _ => break,
                    }
                }
            }
            let default_width = descendant.and_then(|d| d.get(b"DW").ok()).and_then(number).unwrap_or(1000.0);
            return Font { two_byte: true, widths, default_width, measured: identity, encoding };
        }

        let first = dict.get(b"FirstChar").ok().and_then(number).unwrap_or(0.0) as u32;
        let list = dict.get(b"Widths").ok().and_then(deref).and_then(|w| w.as_array().ok());
        let base = dict.get(b"BaseFont").and_then(Object::as_name).unwrap_or(b"");
        let standard = crate::std_widths::widths(base).filter(|_| list.is_none());
        let widths: HashMap<u32, f64> = match (list, standard) {
            (Some(l), _) => l.iter().enumerate().map(|(i, v)| (first + i as u32, deref(v).and_then(number).unwrap_or(0.0))).collect(),
            // A standard font used without widths: its metrics are well known.
            (None, Some(table)) => (32..).zip(table.iter()).filter(|(_, w)| **w > 0).map(|(c, w)| (c, *w as f64)).collect(),
            (None, None) => HashMap::new(),
        };
        let missing = dict
            .get(b"FontDescriptor")
            .ok()
            .and_then(deref)
            .and_then(|d| d.as_dict().ok())
            .and_then(|d| d.get(b"MissingWidth").ok())
            .and_then(number);
        let courier = base.starts_with(b"Courier");
        Font {
            two_byte: false,
            measured: list.is_some() || standard.is_some() || courier,
            default_width: missing.unwrap_or(if courier { 600.0 } else { 556.0 }),
            widths,
            encoding,
        }
    }

    fn codes<'b>(&self, bytes: &'b [u8]) -> Vec<&'b [u8]> {
        if self.two_byte { bytes.chunks(2).collect() } else { bytes.chunks(1).collect() }
    }

    fn width(&self, code: &[u8]) -> f64 {
        let c = code.iter().fold(0u32, |acc, b| acc << 8 | *b as u32);
        self.widths.get(&c).copied().unwrap_or(self.default_width)
    }

    fn text(&self, code: &[u8]) -> String {
        match &self.encoding {
            Some(e) => e.bytes_to_string(code).unwrap_or_default(),
            None if !self.two_byte => (code[0] as char).to_string(),
            None => String::new(),
        }
    }
}

struct Glyph {
    code: Vec<u8>,
    text: String,
    /// Where it lands on the page.
    bbox: [f64; 4],
    /// How far it moves the text position, in text space.
    advance: f64,
}

enum Piece {
    Glyph(usize),
    /// A TJ positioning number.
    Adjust(f64),
}

/// The glyphs drawn by one text-showing operator.
struct Run {
    op: usize,
    glyphs: Vec<Glyph>,
    pieces: Vec<Piece>,
    /// Font size × horizontal scaling: converts TJ numbers to text space.
    scale: f64,
    /// Whether the glyph boxes come from real font metrics.
    measurable: bool,
    /// A generous box around the whole run, for runs that can't be measured.
    extent: [f64; 4],
}

enum DrawKind {
    Image(ObjectId),
    Form,
    Inline,
}

struct Draw {
    op: usize,
    kind: DrawKind,
    ctm: Matrix,
}

/// Everything a page's top-level content draws that redaction cares about.
struct Walk {
    operations: Vec<Operation>,
    runs: Vec<Run>,
    draws: Vec<Draw>,
}

#[derive(Clone)]
struct TextState {
    font: Option<Vec<u8>>,
    size: f64,
    char_spacing: f64,
    word_spacing: f64,
    scaling: f64,
    leading: f64,
    rise: f64,
}

impl Walk {
    fn page(doc: &Document, page_id: ObjectId) -> Option<Walk> {
        let resources = inherited(doc, page_id, b"Resources").and_then(|r| resolve_dict(doc, r));
        let content = Content::decode(&doc.get_page_content(page_id).ok()?).ok()?;
        let sub = |key: &[u8]| resources.and_then(|r| r.get(key).ok()).and_then(|x| resolve_dict(doc, x));
        let (font_dicts, xobjects) = (sub(b"Font"), sub(b"XObject"));
        let mut fonts: HashMap<Vec<u8>, Font> = HashMap::new();

        let mut walk = Walk { operations: Vec::new(), runs: Vec::new(), draws: Vec::new() };
        let mut ctm = IDENTITY;
        let mut ts = TextState { font: None, size: 0.0, char_spacing: 0.0, word_spacing: 0.0, scaling: 1.0, leading: 0.0, rise: 0.0 };
        let mut stack: Vec<(Matrix, TextState)> = Vec::new();
        let (mut tm, mut tlm) = (IDENTITY, IDENTITY);

        for (ix, op) in content.operations.iter().enumerate() {
            let nums: Vec<f64> = op.operands.iter().filter_map(number).collect();
            match (op.operator.as_str(), nums.as_slice()) {
                ("q", _) => stack.push((ctm, ts.clone())),
                ("Q", _) => {
                    if let Some((c, t)) = stack.pop() {
                        (ctm, ts) = (c, t);
                    }
                }
                ("cm", &[a, b, c, d, e, f]) => ctm = multiply(&[a, b, c, d, e, f], &ctm),
                ("BT", _) => (tm, tlm) = (IDENTITY, IDENTITY),
                ("Tf", &[size]) => {
                    ts.font = op.operands.first().and_then(|n| n.as_name().ok()).map(<[u8]>::to_vec);
                    ts.size = size;
                }
                ("Tc", &[v]) => ts.char_spacing = v,
                ("Tw", &[v]) => ts.word_spacing = v,
                ("Tz", &[v]) => ts.scaling = v / 100.0,
                ("TL", &[v]) => ts.leading = v,
                ("Ts", &[v]) => ts.rise = v,
                ("Td", &[x, y]) => {
                    tlm = multiply(&[1.0, 0.0, 0.0, 1.0, x, y], &tlm);
                    tm = tlm;
                }
                ("TD", &[x, y]) => {
                    ts.leading = -y;
                    tlm = multiply(&[1.0, 0.0, 0.0, 1.0, x, y], &tlm);
                    tm = tlm;
                }
                ("Tm", &[a, b, c, d, e, f]) => (tm, tlm) = ([a, b, c, d, e, f], [a, b, c, d, e, f]),
                ("T*", _) => {
                    tlm = multiply(&[1.0, 0.0, 0.0, 1.0, 0.0, -ts.leading], &tlm);
                    tm = tlm;
                }
                ("Tj" | "TJ" | "'" | "\"", _) => {
                    if op.operator == "\"" && nums.len() >= 2 {
                        ts.word_spacing = nums[0];
                        ts.char_spacing = nums[1];
                    }
                    if matches!(op.operator.as_str(), "'" | "\"") {
                        tlm = multiply(&[1.0, 0.0, 0.0, 1.0, 0.0, -ts.leading], &tlm);
                        tm = tlm;
                    }
                    let font_name = ts.font.clone().unwrap_or_default();
                    if !fonts.contains_key(&font_name)
                        && let Some(dict) = font_dicts.and_then(|f| f.get(&font_name).ok()).and_then(|f| resolve_dict(doc, f))
                    {
                        fonts.insert(font_name.clone(), Font::load(doc, dict));
                    }
                    let font = fonts.get(&font_name);
                    let elements: Vec<&Object> = match op.operator.as_str() {
                        "TJ" => op.operands.first().and_then(|a| a.as_array().ok()).map(|a| a.iter().collect()).unwrap_or_default(),
                        _ => op.operands.last().into_iter().collect(),
                    };
                    let run = show(&elements, font, &ts, &mut tm, &ctm, ix);
                    walk.runs.push(run);
                }
                ("Do", _) => {
                    let target = op
                        .operands
                        .first()
                        .and_then(|n| n.as_name().ok())
                        .and_then(|name| xobjects?.get(name).ok()?.as_reference().ok())
                        .and_then(|id| Some((id, doc.get_object(id).ok()?.as_stream().ok()?)));
                    if let Some((id, stream)) = target {
                        let kind = match stream.dict.get(b"Subtype").and_then(Object::as_name) {
                            Ok(b"Image") => DrawKind::Image(id),
                            _ => DrawKind::Form,
                        };
                        let ctm = match kind {
                            // A form's reach is its bounding box, not the unit square.
                            DrawKind::Form => form_box_matrix(stream, &ctm),
                            _ => ctm,
                        };
                        walk.draws.push(Draw { op: ix, kind, ctm });
                    }
                }
                ("BI", _) => walk.draws.push(Draw { op: ix, kind: DrawKind::Inline, ctm }),
                _ => {}
            }
        }
        walk.operations = content.operations;
        Some(walk)
    }
}

/// Lays out the glyphs of one text-showing operator, advancing the text matrix.
fn show(elements: &[&Object], font: Option<&Font>, ts: &TextState, tm: &mut Matrix, ctm: &Matrix, op: usize) -> Run {
    let scale = ts.size * ts.scaling;
    let mut run = Run { op, glyphs: Vec::new(), pieces: Vec::new(), scale, measurable: font.is_some_and(|f| f.measured) && scale != 0.0, extent: [0.0; 4] };
    let start = *tm;
    let mut total = 0.0;
    for element in elements {
        match element {
            Object::String(bytes, _) => {
                let codes: Vec<&[u8]> = match font {
                    Some(f) => f.codes(bytes),
                    None => bytes.chunks(1).collect(),
                };
                for code in codes {
                    let w0 = font.map_or(556.0, |f| f.width(code)) / 1000.0;
                    let space = code == [32] && !font.is_some_and(|f| f.two_byte);
                    let advance = (w0 * ts.size + ts.char_spacing + if space { ts.word_spacing } else { 0.0 }) * ts.scaling;
                    let trm = multiply(&multiply(&[scale, 0.0, 0.0, ts.size, 0.0, ts.rise], tm), ctm);
                    let bbox = transform_rect(&[0.0, -DESCENT, w0.max(0.05), ASCENT], &trm);
                    run.pieces.push(Piece::Glyph(run.glyphs.len()));
                    run.glyphs.push(Glyph {
                        code: code.to_vec(),
                        text: font.map(|f| f.text(code)).unwrap_or_default(),
                        bbox,
                        advance,
                    });
                    *tm = multiply(&[1.0, 0.0, 0.0, 1.0, advance, 0.0], tm);
                    total += advance;
                }
            }
            other => {
                if let Some(n) = number(other) {
                    let tx = -n / 1000.0 * scale;
                    run.pieces.push(Piece::Adjust(n));
                    *tm = multiply(&[1.0, 0.0, 0.0, 1.0, tx, 0.0], tm);
                    total += tx;
                }
            }
        }
    }
    // For unmeasurable runs, be generous: guessed widths may be off by half.
    let trm = multiply(&multiply(&[1.0, 0.0, 0.0, 1.0, 0.0, ts.rise], &start), ctm);
    let reach = total.abs() * 1.5 + ts.size;
    run.extent = transform_rect(&[-ts.size, -DESCENT * ts.size * 2.0, reach, ASCENT * ts.size * 2.0], &trm);
    run
}

fn form_box_matrix(form: &Stream, ctm: &Matrix) -> Matrix {
    let nums = |key: &[u8]| -> Option<Vec<f64>> {
        Some(form.dict.get(key).ok()?.as_array().ok()?.iter().filter_map(number).collect())
    };
    let matrix = match nums(b"Matrix").as_deref() {
        Some(&[a, b, c, d, e, f]) => [a, b, c, d, e, f],
        _ => IDENTITY,
    };
    let [x0, y0, x1, y1] = match nums(b"BBox").as_deref() {
        Some(&[a, b, c, d]) => normalize([a, b, c, d]),
        // No box: assume it could be anywhere.
        _ => [-1e6, -1e6, 1e6, 1e6],
    };
    // Map the unit square onto the bounding box, then through the form matrix and CTM.
    multiply(&multiply(&[x1 - x0, 0.0, 0.0, y1 - y0, x0, y0], &matrix), ctm)
}

/// Writes content operations back out, including inline images (which lopdf's encoder can't).
fn encode(ops: &[Operation]) -> Vec<u8> {
    let mut out = Vec::new();
    for op in ops {
        if op.operator == "BI"
            && let Some(Object::Stream(image)) = op.operands.first()
        {
            out.extend_from_slice(b"BI");
            for (key, value) in image.dict.iter() {
                out.push(b' ');
                out.extend(Content { operations: vec![Operation::new("", vec![Object::Name(key.clone()), value.clone()])] }.encode().unwrap_or_default());
            }
            out.extend_from_slice(b" ID ");
            out.extend_from_slice(&image.content);
            out.extend_from_slice(b"\nEI\n");
            continue;
        }
        out.extend(Content { operations: vec![op.clone()] }.encode().unwrap_or_default());
        out.push(b'\n');
    }
    out
}

/// What metadata a document carries, in plain words.
pub fn describe_metadata(doc: &Document) -> Vec<String> {
    let mut found = Vec::new();
    if let Some(info) = doc.trailer.get(b"Info").ok().and_then(|i| resolve_dict(doc, i)) {
        for (key, label) in [
            (b"Author".as_slice(), "author"),
            (b"Title", "title"),
            (b"Subject", "subject"),
            (b"Keywords", "keywords"),
            (b"Creator", "app that made it"),
            (b"Producer", "PDF producer"),
            (b"CreationDate", "creation date"),
            (b"ModDate", "last edited date"),
        ] {
            if let Ok(value) = info.get(key).and_then(Object::as_str) {
                let value = lopdf::decode_text_string(&Object::String(value.to_vec(), StringFormat::Literal)).unwrap_or_default();
                if !value.trim().is_empty() {
                    found.push(format!("{label}: {}", value.trim()));
                }
            }
        }
    }
    let xmp = doc.objects.values().filter(|o| o.as_stream().is_ok_and(|s| s.dict.has_type(b"Metadata"))).count();
    if xmp > 0 {
        found.push(format!("{xmp} XMP metadata block{}", if xmp == 1 { "" } else { "s" }));
    }
    let exif = doc
        .objects
        .values()
        .filter_map(|o| o.as_stream().ok())
        .filter(|s| is_jpeg(s) && !jpeg_without_metadata(&s.content).is_none_or(|clean| clean.len() == s.content.len()))
        .count();
    if exif > 0 {
        found.push(format!("camera/location data (EXIF) in {exif} image{}", if exif == 1 { "" } else { "s" }));
    }
    let notes = doc
        .objects
        .values()
        .filter_map(|o| o.as_dict().ok())
        .filter(|d| d.has_type(b"Annot") && d.has(b"T"))
        .count();
    if notes > 0 {
        found.push(format!("author names on {notes} comment{}", if notes == 1 { "" } else { "s" }));
    }
    found
}

/// Removes document info, XMP metadata, private app data, EXIF/XMP inside
/// JPEG images and author names on comments. Returns how many things were removed.
pub fn strip_metadata(doc: &mut Document) -> usize {
    let mut removed = 0;
    if doc.trailer.remove(b"Info").is_some() {
        removed += 1;
    }
    // A fresh file ID: the old one can link this copy to the original.
    doc.trailer.remove(b"ID");
    for obj in doc.objects.values_mut() {
        let dict = match obj {
            Object::Dictionary(d) => d,
            Object::Stream(s) => {
                if is_jpeg(s)
                    && let Some(clean) = jpeg_without_metadata(&s.content)
                    && clean.len() != s.content.len()
                {
                    s.set_content(clean);
                    removed += 1;
                }
                &mut s.dict
            }
            _ => continue,
        };
        for key in [b"Metadata".as_slice(), b"PieceInfo", b"LastModified"] {
            if dict.remove(key).is_some() {
                removed += 1;
            }
        }
        if dict.has_type(b"Annot") {
            for key in [b"T".as_slice(), b"M", b"CreationDate"] {
                if dict.remove(key).is_some() {
                    removed += 1;
                }
            }
        }
    }
    pages::prune_unreachable(doc);
    removed
}

fn is_jpeg(s: &Stream) -> bool {
    matches!(s.dict.get(b"Filter"), Ok(Object::Name(n)) if n == b"DCTDecode")
        || s.dict.get(b"Filter").and_then(Object::as_array).is_ok_and(|a| a.len() == 1 && a[0].as_name().ok() == Some(b"DCTDecode"))
}

/// The JPEG without its APP1–APP13 and comment segments (EXIF, XMP, IPTC, …),
/// losslessly. APP0 (JFIF) and APP14 (Adobe colour info) stay. `None` if it
/// doesn't look like a JPEG.
fn jpeg_without_metadata(data: &[u8]) -> Option<Vec<u8>> {
    if data.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut out = vec![0xFF, 0xD8];
    let mut i = 2;
    while i + 4 <= data.len() {
        if data[i] != 0xFF {
            return None;
        }
        let marker = data[i + 1];
        if marker == 0xDA {
            // Start of scan: the rest is image data.
            out.extend_from_slice(&data[i..]);
            return Some(out);
        }
        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        let end = i + 2 + len;
        if end > data.len() {
            return None;
        }
        let metadata = (0xE1..=0xED).contains(&marker) || marker == 0xFE;
        if !metadata {
            out.extend_from_slice(&data[i..end]);
        }
        i = end;
    }
    None
}

fn normalize(r: [f64; 4]) -> [f64; 4] {
    [r[0].min(r[2]), r[1].min(r[3]), r[0].max(r[2]), r[1].max(r[3])]
}

fn intersects(a: &[f64; 4], b: &[f64; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

fn union(a: &[f64; 4], b: &[f64; 4]) -> [f64; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

fn overlaps_vertically(a: &[f64; 4], b: &[f64; 4]) -> bool {
    a[1] < b[3] && b[1] < a[3]
}

/// The bounding box of a rectangle after a transformation.
pub fn transform_rect(r: &[f64; 4], m: &Matrix) -> [f64; 4] {
    let corners = [(r[0], r[1]), (r[2], r[1]), (r[0], r[3]), (r[2], r[3])]
        .map(|(x, y)| (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]));
    let xs = corners.map(|c| c.0);
    let ys = corners.map(|c| c.1);
    [
        xs.iter().copied().fold(f64::MAX, f64::min),
        ys.iter().copied().fold(f64::MAX, f64::min),
        xs.iter().copied().fold(f64::MIN, f64::max),
        ys.iter().copied().fold(f64::MIN, f64::max),
    ]
}

pub fn invert(m: &Matrix) -> Option<Matrix> {
    let det = m[0] * m[3] - m[1] * m[2];
    if det.abs() < 1e-12 {
        return None;
    }
    let (a, b, c, d) = (m[3] / det, -m[1] / det, -m[2] / det, m[0] / det);
    Some([a, b, c, d, -(m[4] * a + m[5] * c), -(m[4] * b + m[5] * d)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    /// A page with two lines in Helvetica *with* a Widths array (so glyphs can
    /// be cut precisely), plus a JPEG photo with EXIF, plus metadata.
    fn report_pdf() -> Document {
        let mut doc = Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        // Helvetica widths for 32..=126.
        let widths: Vec<Object> = [
            278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556, 556, 556,
            556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667, 611, 778, 722, 278,
            500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469,
            556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500,
            278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
        ]
        .into_iter()
        .map(Object::from)
        .collect();
        let font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding", "FirstChar" => 32, "LastChar" => 126, "Widths" => widths,
        });
        let photo = {
            let img = RgbImage::from_fn(200, 100, |x, _| image::Rgb([200, (x % 255) as u8, 90]));
            let mut jpeg = Vec::new();
            img.write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)).unwrap();
            // Splice in a fake EXIF segment after SOI.
            let exif = b"Exif\0\0GPS 47.3769 N 8.5417 E";
            let mut with_exif = vec![0xFF, 0xD8, 0xFF, 0xE1];
            with_exif.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
            with_exif.extend_from_slice(exif);
            with_exif.extend_from_slice(&jpeg[2..]);
            doc.add_object(Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Image", "Width" => 200, "Height" => 100,
                    "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8, "Filter" => "DCTDecode",
                },
                with_exif,
            ))
        };
        let content = b"BT /F1 12 Tf 72 700 Td (Customer: Jane Example, account 4711) Tj 0 -20 Td [(Total due) -250 (CHF 99)] TJ ET\n\
            q 200 0 0 100 72 400 cm /Im0 Do Q"
            .to_vec();
        let content = doc.add_object(Stream::new(dictionary! {}, content));
        let note = doc.add_object(dictionary! {
            "Type" => "Annot", "Subtype" => "Text", "T" => Object::string_literal("Jane Example"),
            "Rect" => vec![140.into(), 695.into(), 160.into(), 715.into()],
            "Contents" => Object::string_literal("call her"),
        });
        let page = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content, "Annots" => vec![note.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font },
                "XObject" => dictionary! { "Im0" => photo },
            },
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1,
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            }),
        );
        let xmp = doc.add_object(Stream::new(dictionary! { "Type" => "Metadata", "Subtype" => "XML" }, b"<x:xmpmeta>Jane</x:xmpmeta>".to_vec()));
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id, "Metadata" => xmp });
        let info = doc.add_object(dictionary! { "Author" => Object::string_literal("Jane Example"), "Title" => Object::string_literal("Invoice") });
        doc.trailer.set("Root", catalog);
        doc.trailer.set("Info", info);
        doc
    }

    /// Saves and reloads, returning everything readable in the file: either
    /// just what the pages draw, or everything but that.
    fn saved_text(doc: &mut Document, page_content: bool) -> (String, Document) {
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        let reloaded = Document::load_mem(&bytes).unwrap();
        let contents: HashSet<ObjectId> =
            reloaded.get_pages().values().flat_map(|&p| reloaded.get_page_contents(p)).collect();
        let mut plain = String::new();
        for (id, obj) in &reloaded.objects {
            if contents.contains(id) != page_content {
                continue;
            }
            match obj {
                Object::Stream(s) => {
                    plain.push_str(&String::from_utf8_lossy(&s.get_plain_content().unwrap_or_default()));
                    plain.push_str(&format!("{:?}", s.dict));
                }
                other => plain.push_str(&format!("{other:?}")),
            }
        }
        (plain, reloaded)
    }

    #[test]
    fn finds_text() {
        let doc = report_pdf();
        let hits = find_text(&doc, "jane example");
        assert_eq!(hits.len(), 1);
        let r = hits[0].rect;
        // "Customer: " is about 57pt wide at 12pt; the name is about 75pt wide.
        assert!(r[0] > 72.0 + 50.0 && r[0] < 72.0 + 65.0, "{r:?}");
        assert!(r[2] - r[0] > 60.0 && r[2] - r[0] < 90.0, "{r:?}");
        assert!(r[1] < 700.0 && r[3] > 700.0 + 8.0, "{r:?}");
        assert!(find_text(&doc, "nobody").is_empty());
        // Split across the TJ positioning: still one phrase.
        assert_eq!(find_text(&doc, "total due").len(), 1);
    }

    #[test]
    fn redacts_text_without_moving_the_rest() {
        let mut doc = report_pdf();
        let areas = find_text(&doc, "Jane Example");
        let report = redact(&mut doc, &areas).unwrap();
        assert_eq!(report.glyphs, "JaneExample".len() + 1);
        assert!(report.removed_whole.is_empty(), "{:?}", report.removed_whole);
        let (text, reloaded) = saved_text(&mut doc, true);
        assert!(!text.contains("Jane"), "the name is still on the page");
        assert!(!text.contains("Example"));
        // Neighbouring text survives, at the same place.
        let after = find_text(&reloaded, "account 4711");
        let before = find_text(&report_pdf(), "account 4711");
        assert_eq!(after.len(), 1);
        assert!((after[0].rect[0] - before[0].rect[0]).abs() < 0.01, "{after:?} vs {before:?}");
        assert_eq!(find_text(&reloaded, "Customer:").len(), 1);
        // The note on top of the name is gone too.
        assert_eq!(report.annotations, 1);
    }

    #[test]
    fn burns_images() {
        let mut doc = report_pdf();
        // The left half of the photo (drawn at 72..272 × 400..500).
        let report = redact(&mut doc, &[Area { page: 1, rect: [60.0, 390.0, 172.0, 510.0] }]).unwrap();
        assert_eq!(report.images, 1);
        assert_eq!(report.glyphs, 0);
        let page = doc.get_pages()[&1];
        let mut drawn = Vec::new();
        crate::content::page_images(&doc, page, &mut |id, _| drawn.push(id));
        let stream = doc.get_object(drawn[0]).unwrap().as_stream().unwrap();
        let img = image::load_from_memory(&stream.content).unwrap().to_rgb8();
        let dark = |x: u32| img.get_pixel(x, 50).0.iter().all(|&c| c < 40);
        assert!(dark(10) && dark(90), "left half should be black");
        assert!(!dark(150), "right half should be untouched");
    }

    #[test]
    fn strips_metadata() {
        let mut doc = report_pdf();
        let found = describe_metadata(&doc);
        assert!(found.iter().any(|f| f == "author: Jane Example"), "{found:?}");
        assert!(found.iter().any(|f| f.contains("EXIF")), "{found:?}");
        assert!(found.iter().any(|f| f.contains("XMP")), "{found:?}");
        assert!(found.iter().any(|f| f.contains("comment")), "{found:?}");
        assert!(strip_metadata(&mut doc) >= 4);
        assert!(describe_metadata(&doc).is_empty(), "{:?}", describe_metadata(&doc));
        let (text, reloaded) = saved_text(&mut doc, false);
        assert!(!text.contains("Jane"), "a name is still in the file's metadata");
        assert!(!text.contains("GPS"));
        // The photo still decodes.
        let page = reloaded.get_pages()[&1];
        let mut ok = false;
        crate::content::page_images(&reloaded, page, &mut |id, _| {
            ok = image::load_from_memory(&reloaded.get_object(id).unwrap().as_stream().unwrap().content).is_ok()
        });
        assert!(ok);
    }

    #[test]
    fn standard_fonts_without_widths() {
        // The fixture uses plain Helvetica with no Widths array; Hush knows its metrics.
        let mut doc = crate::pages::tests::text_pdf(&["secret plans"]);
        let hits = find_text(&doc, "plans");
        let report = redact(&mut doc, &hits).unwrap();
        assert!(report.removed_whole.is_empty());
        assert_eq!(report.glyphs, 5);
        let (text, reloaded) = saved_text(&mut doc, true);
        assert!(!text.contains("plans"));
        assert_eq!(find_text(&reloaded, "secret").len(), 1);
    }

    #[test]
    fn unmeasurable_text_goes_whole() {
        // An unknown font with no Widths: Hush can't cut glyph by glyph.
        let mut doc = crate::pages::tests::text_pdf(&["secret plans"]);
        for obj in doc.objects.values_mut() {
            if let Ok(d) = obj.as_dict_mut()
                && d.has_type(b"Font")
            {
                d.set("BaseFont", "MysteryGrotesk");
            }
        }
        let hits = find_text(&doc, "plans");
        assert_eq!(hits.len(), 1);
        let report = redact(&mut doc, &hits).unwrap();
        assert_eq!(report.removed_whole.len(), 1);
        let (text, _) = saved_text(&mut doc, true);
        assert!(!text.contains("plans") && !text.contains("secret"));
    }

    #[test]
    fn jpeg_metadata_segments() {
        assert!(jpeg_without_metadata(b"not a jpeg").is_none());
        let clean = jpeg_without_metadata(&[0xFF, 0xD8, 0xFF, 0xE1, 0, 4, 1, 2, 0xFF, 0xDA, 0, 2, 9, 9]).unwrap();
        assert_eq!(clean, [0xFF, 0xD8, 0xFF, 0xDA, 0, 2, 9, 9]);
    }

    #[test]
    fn matrices() {
        let m = [2.0, 0.0, 0.0, 3.0, 10.0, 20.0];
        let back = multiply(&m, &invert(&m).unwrap());
        for (a, b) in back.iter().zip(IDENTITY) {
            assert!((a - b).abs() < 1e-9);
        }
        assert_eq!(transform_rect(&[0.0, 0.0, 1.0, 1.0], &m), [10.0, 20.0, 12.0, 23.0]);
    }
}
