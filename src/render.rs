//! Drawing PDF pages to bitmaps, for thumbnails.
//!
//! This uses hayro, a pure-Rust PDF rasteriser, so text and vector pages get a
//! preview too, not just scans.

use std::sync::Arc;

use hayro::{
    PixmapSettings, RenderCache, RenderSettings,
    hayro_interpret::{InterpreterSettings, util::TransformExt},
    hayro_syntax::Pdf,
    vello_cpu::color::palette::css::WHITE,
};
use image::RgbaImage;
use rayon::prelude::*;

/// Renders every page so that its longer side is `max_side` pixels, all pages in
/// parallel. A page that can't be drawn comes back as `None`.
pub fn thumbnails(data: Arc<Vec<u8>>, max_side: f32) -> Vec<Option<RgbaImage>> {
    let Ok(pdf) = Pdf::new(data.clone()) else { return Vec::new() };
    let count = pdf.pages().len();
    drop(pdf);

    (0..count)
        .into_par_iter()
        // hayro's documents aren't shareable across threads, so each worker opens its own.
        .map_init(|| Pdf::new(data.clone()).ok(), |pdf, ix| render_page(pdf.as_ref()?, ix, max_side))
        .collect()
}

/// For every page: the matrix that maps the page's own coordinates (PDF points,
/// y up, before /Rotate) onto a rendering at scale 1 (y down, rotated), and the
/// size of that rendering.
pub fn page_geometry(data: Arc<Vec<u8>>) -> Vec<Option<([f64; 6], (f32, f32))>> {
    let Ok(pdf) = Pdf::new(data) else { return Vec::new() };
    pdf.pages()
        .iter()
        .map(|page| Some((page.initial_transform(true).to_kurbo().as_coeffs(), page.render_dimensions())))
        .collect()
}

fn render_page(pdf: &Pdf, ix: usize, max_side: f32) -> Option<RgbaImage> {
    let page = pdf.pages().get(ix)?;
    let (w, h) = page.render_dimensions();
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let scale = max_side / w.max(h);
    let pixmap = hayro::render(
        page,
        &RenderCache::new(),
        &InterpreterSettings::default(),
        &RenderSettings::default(),
        &PixmapSettings { x_scale: scale, y_scale: scale, bg_color: WHITE },
    );
    // On an opaque white background, premultiplied and straight alpha are the same.
    RgbaImage::from_raw(
        pixmap.width() as u32,
        pixmap.height() as u32,
        pixmap.data_as_u8_slice().to_vec(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::tests::text_pdf;

    #[test]
    fn draws_text_pages() {
        let mut doc = text_pdf(&["hello", "world"]);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        let thumbs = thumbnails(Arc::new(bytes), 200.0);
        assert_eq!(thumbs.len(), 2);
        let thumb = thumbs[0].as_ref().unwrap();
        // Letter-sized page: 612 × 792 points.
        assert!((199..=200).contains(&thumb.height()), "{}", thumb.height());
        assert!((150..=156).contains(&thumb.width()), "{}", thumb.width());
        // White page with some dark ink on it.
        assert!(thumb.pixels().any(|p| p.0[0] < 100));
        assert!(thumb.pixels().filter(|p| p.0[0] > 250).count() > thumb.len() / 4 * 9 / 10);
    }
}
