//! Walking a page's drawing instructions while keeping track of where things land.

use lopdf::{Dictionary, Document, Object, ObjectId, content::Content};

use crate::pdf::inherited;

/// A PDF transformation matrix `[a b c d e f]`.
pub type Matrix = [f64; 6];

pub const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `m` applied first, then `n` (PDF's `m × n`).
pub fn multiply(m: &Matrix, n: &Matrix) -> Matrix {
    [
        m[0] * n[0] + m[1] * n[2],
        m[0] * n[1] + m[1] * n[3],
        m[2] * n[0] + m[3] * n[2],
        m[2] * n[1] + m[3] * n[3],
        m[4] * n[0] + m[5] * n[2] + n[4],
        m[4] * n[1] + m[5] * n[3] + n[5],
    ]
}

/// Form XObjects nested deeper than this are ignored.
const MAX_DEPTH: usize = 8;

/// Calls `on_image` for every image XObject the page draws, with the matrix that
/// maps the image's unit square onto the page. Images inside form XObjects are
/// included.
pub fn page_images(doc: &Document, page_id: ObjectId, on_image: &mut dyn FnMut(ObjectId, Matrix)) {
    let resources = inherited(doc, page_id, b"Resources").and_then(|r| resolve_dict(doc, r));
    let Ok(content) = doc.get_page_content(page_id) else { return };
    walk(doc, &content, resources, IDENTITY, 0, on_image);
}

fn walk(
    doc: &Document,
    content: &[u8],
    resources: Option<&Dictionary>,
    base: Matrix,
    depth: usize,
    on_image: &mut dyn FnMut(ObjectId, Matrix),
) {
    let Ok(content) = Content::decode(content) else { return };
    let xobjects = resources
        .and_then(|r| r.get(b"XObject").ok())
        .and_then(|x| resolve_dict(doc, x));
    let mut ctm = base;
    let mut stack = Vec::new();
    for op in &content.operations {
        match op.operator.as_str() {
            "q" => stack.push(ctm),
            "Q" => ctm = stack.pop().unwrap_or(base),
            "cm" => {
                let n: Vec<f64> = op.operands.iter().filter_map(number).collect();
                if let [a, b, c, d, e, f] = n[..] {
                    ctm = multiply(&[a, b, c, d, e, f], &ctm);
                }
            }
            "Do" => {
                let Some(id) = op
                    .operands
                    .first()
                    .and_then(|n| n.as_name().ok())
                    .and_then(|name| xobjects?.get(name).ok()?.as_reference().ok())
                else {
                    continue;
                };
                let Ok(Object::Stream(stream)) = doc.get_object(id) else { continue };
                match stream.dict.get(b"Subtype").and_then(Object::as_name) {
                    Ok(b"Image") => on_image(id, ctm),
                    Ok(b"Form") if depth < MAX_DEPTH => {
                        let matrix = stream
                            .dict
                            .get(b"Matrix")
                            .and_then(Object::as_array)
                            .ok()
                            .map(|m| m.iter().filter_map(number).collect::<Vec<_>>());
                        let form_ctm = match matrix.as_deref() {
                            Some(&[a, b, c, d, e, f]) => multiply(&[a, b, c, d, e, f], &ctm),
                            _ => ctm,
                        };
                        let form_resources = stream
                            .dict
                            .get(b"Resources")
                            .ok()
                            .and_then(|r| resolve_dict(doc, r))
                            .or(resources);
                        if let Ok(data) = stream.get_plain_content() {
                            walk(doc, &data, form_resources, form_ctm, depth + 1, on_image);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

pub fn number(obj: &Object) -> Option<f64> {
    match obj {
        Object::Integer(i) => Some(*i as f64),
        Object::Real(r) => Some(*r as f64),
        _ => None,
    }
}

pub fn resolve_dict<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a Dictionary> {
    match obj {
        Object::Reference(id) => doc.get_dictionary(*id).ok(),
        Object::Dictionary(dict) => Some(dict),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_scaled_images() {
        let doc = crate::pdf::tests::fixture_pdf();
        let page = doc.get_pages()[&1];
        let mut seen = Vec::new();
        page_images(&doc, page, &mut |id, m| seen.push((id, m)));
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].1, [595.0, 0.0, 0.0, 842.0, 0.0, 0.0]);
    }

    #[test]
    fn multiplies_in_pdf_order() {
        let scale = [2.0, 0.0, 0.0, 2.0, 0.0, 0.0];
        let shift = [1.0, 0.0, 0.0, 1.0, 10.0, 0.0];
        // Scale first, then shift: the shift is not scaled.
        assert_eq!(multiply(&scale, &shift)[4], 10.0);
        assert_eq!(multiply(&shift, &scale)[4], 20.0);
    }
}
