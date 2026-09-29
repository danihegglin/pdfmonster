//! Skew detection for scanned pages.
//!
//! Uses the projection-profile method: the page is binarised, and for every
//! candidate angle the dark pixels are projected onto the vertical axis of a
//! rotated frame. When the angle matches the text lines, the histogram turns
//! into sharp peaks (lines) and valleys (gaps), which maximises the sum of
//! squared differences between neighbouring bins. A coarse sweep followed by
//! a fine one keeps it to a few milliseconds per page.

use image::GrayImage;

/// Largest skew (in degrees) we look for. Real scans are almost always within this.
pub const MAX_ANGLE: f32 = 15.0;
/// Pages analysed at roughly this size on their longest side. Plenty for sub-0.1° accuracy.
const WORK_SIZE: u32 = 1400;
/// Upper bound on the number of dark pixels scored per angle.
const MAX_POINTS: usize = 150_000;

/// Skew of a page's content, in degrees.
///
/// Positive means the lines run downhill to the right (content turned clockwise);
/// rotating the page counter-clockwise by this angle straightens it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Skew {
    pub degrees: f32,
    /// How much sharper the best profile is than the flattest one (≥ 1). Near 1 = no structure.
    pub confidence: f32,
}

/// Detects the skew of a grayscale page. `quarter_turn` analyses the page as if
/// rotated by 90°, for pages whose text runs vertically in the stored image.
pub fn detect(img: &GrayImage, quarter_turn: bool) -> Skew {
    let small = downscale(img, WORK_SIZE);
    let mut points = dark_points(&small);
    if quarter_turn {
        // A proper rotation keeps the clockwise sense, so the angle needs no fix-up.
        let h = small.height() as f32;
        for p in &mut points {
            *p = (h - p.1, p.0);
        }
    }
    if points.len() < 50 {
        return Skew { degrees: 0.0, confidence: 1.0 };
    }

    let score = |deg: f32| profile_score(&points, deg);

    // Coarse sweep.
    let coarse_step = 0.25;
    let steps = (MAX_ANGLE / coarse_step) as i32;
    let mut best = (0.0f32, f64::MIN);
    let mut worst = f64::MAX;
    for i in -steps..=steps {
        let deg = i as f32 * coarse_step;
        let s = score(deg);
        worst = worst.min(s);
        if s > best.1 {
            best = (deg, s);
        }
    }

    // Fine sweep around the coarse winner.
    let fine_step = 0.02;
    let center = best.0;
    let n = (coarse_step / fine_step).ceil() as i32;
    for i in -n..=n {
        let deg = center + i as f32 * fine_step;
        let s = score(deg);
        if s > best.1 {
            best = (deg, s);
        }
    }

    let confidence = if worst > 0.0 { (best.1 / worst) as f32 } else { 1.0 };
    Skew { degrees: best.0, confidence }
}

/// Sum of squared differences between neighbouring bins of the rotated row histogram.
fn profile_score(points: &[(f32, f32)], deg: f32) -> f64 {
    let (s, c) = deg.to_radians().sin_cos();
    let mut min = f32::MAX;
    let mut max = f32::MIN;
    for &(x, y) in points {
        let r = y * c - x * s;
        min = min.min(r);
        max = max.max(r);
    }
    let mut bins = vec![0u32; (max - min) as usize + 2];
    for &(x, y) in points {
        bins[(y * c - x * s - min) as usize] += 1;
    }
    bins.windows(2)
        .map(|w| {
            let d = w[1] as f64 - w[0] as f64;
            d * d
        })
        .sum()
}

/// Coordinates of "ink" pixels, found with an Otsu threshold.
/// A thin margin is ignored so scanner borders and punch holes don't dominate.
fn dark_points(img: &GrayImage) -> Vec<(f32, f32)> {
    let threshold = otsu(img);
    let (w, h) = img.dimensions();
    let mx = w / 25;
    let my = h / 25;
    let mut points = Vec::new();
    for y in my..h.saturating_sub(my) {
        for x in mx..w.saturating_sub(mx) {
            if img.get_pixel(x, y).0[0] < threshold {
                points.push((x as f32, y as f32));
            }
        }
    }
    // Mostly-dark pages (photos, dark backgrounds): nothing text-like to lock onto.
    if points.len() > (w * h / 2) as usize {
        return Vec::new();
    }
    if points.len() > MAX_POINTS {
        let stride = points.len().div_ceil(MAX_POINTS);
        points = points.into_iter().step_by(stride).collect();
    }
    points
}

fn otsu(img: &GrayImage) -> u8 {
    let mut hist = [0u64; 256];
    for p in img.pixels() {
        hist[p.0[0] as usize] += 1;
    }
    let total: u64 = hist.iter().sum();
    let sum_all: f64 = hist.iter().enumerate().map(|(i, &n)| i as f64 * n as f64).sum();
    let (mut w_bg, mut sum_bg) = (0u64, 0f64);
    let (mut best_t, mut best_var) = (128u8, 0f64);
    for t in 0..256 {
        w_bg += hist[t];
        if w_bg == 0 {
            continue;
        }
        let w_fg = total - w_bg;
        if w_fg == 0 {
            break;
        }
        sum_bg += t as f64 * hist[t] as f64;
        let m_bg = sum_bg / w_bg as f64;
        let m_fg = (sum_all - sum_bg) / w_fg as f64;
        let var = w_bg as f64 * w_fg as f64 * (m_bg - m_fg).powi(2);
        if var > best_var {
            best_var = var;
            best_t = t as u8;
        }
    }
    best_t.saturating_add(1)
}

/// Fast box-filter downscale so the longest side is at most `max_side`.
pub fn downscale(img: &GrayImage, max_side: u32) -> GrayImage {
    let (w, h) = img.dimensions();
    let factor = w.max(h).div_ceil(max_side).max(1);
    if factor == 1 {
        return img.clone();
    }
    let (nw, nh) = (w / factor, h / factor);
    let src = img.as_raw();
    let mut out = GrayImage::new(nw, nh);
    let area = factor * factor;
    for oy in 0..nh {
        for ox in 0..nw {
            let mut acc = 0u32;
            for dy in 0..factor {
                let row = ((oy * factor + dy) * w + ox * factor) as usize;
                acc += src[row..row + factor as usize].iter().map(|&v| v as u32).sum::<u32>();
            }
            out.put_pixel(ox, oy, image::Luma([(acc / area) as u8]));
        }
    }
    out
}

/// Rotates an image counter-clockwise by `degrees` around its centre (bilinear, white fill).
/// This is the preview of what straightening a page with that skew looks like.
pub fn straighten(img: &GrayImage, degrees: f32) -> GrayImage {
    let (w, h) = img.dimensions();
    let (s, c) = degrees.to_radians().sin_cos();
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let src = img.as_raw();
    let sample = |x: i32, y: i32| -> f32 {
        if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
            255.0
        } else {
            src[y as usize * w as usize + x as usize] as f32
        }
    };
    GrayImage::from_fn(w, h, |x, y| {
        let dx = x as f32 + 0.5 - cx;
        let dy = y as f32 + 0.5 - cy;
        let sx = c * dx - s * dy + cx - 0.5;
        let sy = s * dx + c * dy + cy - 0.5;
        let (x0, y0) = (sx.floor(), sy.floor());
        let (fx, fy) = (sx - x0, sy - y0);
        let (x0, y0) = (x0 as i32, y0 as i32);
        let top = sample(x0, y0) * (1.0 - fx) + sample(x0 + 1, y0) * fx;
        let bottom = sample(x0, y0 + 1) * (1.0 - fx) + sample(x0 + 1, y0 + 1) * fx;
        image::Luma([(top * (1.0 - fy) + bottom * fy).round() as u8])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> GrayImage {
        image::open(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/skewed-scan.png"))
            .unwrap()
            .to_luma8()
    }

    #[test]
    fn detects_real_scan() {
        let skew = detect(&fixture(), false);
        assert!(skew.degrees > 0.5 && skew.degrees < 1.5, "{skew:?}");
        assert!(skew.confidence > 1.5, "{skew:?}");
    }

    #[test]
    fn recovers_synthetic_rotation() {
        let upright = straighten(&fixture(), detect(&fixture(), false).degrees);
        for want in [-7.0f32, -2.5, 0.0, 3.3] {
            // Rotating by -want introduces a skew of `want`.
            let skewed = straighten(&upright, -want);
            let got = detect(&skewed, false).degrees;
            assert!((got - want).abs() < 0.15, "want {want}, got {got}");
        }
    }

    #[test]
    fn quarter_turned_page() {
        let img = fixture();
        let turned = image::imageops::rotate90(&img);
        let a = detect(&img, false).degrees;
        let b = detect(&turned, true).degrees;
        assert!((a - b).abs() < 0.15, "{a} vs {b}");
    }
}
