//! Snip: cuts pages out of a PDF, or splits it into several.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use lopdf::{Document, ObjectId};

use crate::pages;

/// Parses a page list like `1-3,5,9-` (1-based, `9-` means "9 to the end",
/// `-3` means "1 to 3", and `9-4` counts backwards) into page numbers.
pub fn parse_pages(spec: &str, count: u32) -> Result<Vec<u32>> {
    let mut pages = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let number = |s: &str, default: u32| -> Result<u32> {
            let s = s.trim();
            if s.is_empty() {
                return Ok(default);
            }
            let n = if s.eq_ignore_ascii_case("end") || s.eq_ignore_ascii_case("last") {
                count
            } else {
                s.parse().with_context(|| format!("{s:?} is not a page number"))?
            };
            if n == 0 || n > count {
                bail!("there is no page {n} (the document has {count})");
            }
            Ok(n)
        };
        match part.split_once('-') {
            Some((a, b)) => {
                let (a, b) = (number(a, 1)?, number(b, count)?);
                if a <= b {
                    pages.extend(a..=b);
                } else {
                    pages.extend((b..=a).rev());
                }
            }
            None => pages.push(number(part, 1)?),
        }
    }
    if pages.is_empty() {
        bail!("no pages selected");
    }
    Ok(pages)
}

/// A new document with just these pages (1-based), in this order. Everything
/// only the other pages used is left behind.
pub fn extract(doc: &Document, numbers: &[u32]) -> Result<Document> {
    let all = doc.get_pages();
    let mut seen = std::collections::HashSet::new();
    let ids: Vec<ObjectId> = numbers
        .iter()
        .map(|n| {
            if !seen.insert(n) {
                bail!("page {n} is listed twice");
            }
            all.get(n).copied().with_context(|| format!("there is no page {n}"))
        })
        .collect::<Result<_>>()?;
    let mut out = doc.clone();
    pages::set_pages(&mut out, &ids)?;
    Ok(out)
}

/// Splits into chunks of `every` pages, returning each chunk's page numbers.
pub fn chunks(count: u32, every: u32) -> Vec<Vec<u32>> {
    let every = every.max(1);
    (1..=count)
        .collect::<Vec<_>>()
        .chunks(every as usize)
        .map(<[u32]>::to_vec)
        .collect()
}

/// `scan.pdf` + pages 4–9 → `scan.p4-9.pdf`.
pub fn output_for(input: &Path, numbers: &[u32]) -> PathBuf {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("document");
    input.with_file_name(format!("{stem}.{}.pdf", label(numbers)))
}

/// A short name for a set of pages: `p4`, `p4-9`, or `p4-9+3` when they aren't one run.
fn label(numbers: &[u32]) -> String {
    let (first, last) = (numbers[0], numbers[numbers.len() - 1]);
    let is_run = numbers.windows(2).all(|w| w[1] == w[0] + 1);
    match (numbers.len(), is_run) {
        (1, _) => format!("p{first}"),
        (_, true) => format!("p{first}-{last}"),
        (n, false) => format!("p{first}+{}", n - 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::tests::{page_texts, text_pdf};

    #[test]
    fn parses_page_lists() {
        assert_eq!(parse_pages("1-3,5,9-", 10).unwrap(), [1, 2, 3, 5, 9, 10]);
        assert_eq!(parse_pages("-2, last", 4).unwrap(), [1, 2, 4]);
        assert_eq!(parse_pages("3-1", 4).unwrap(), [3, 2, 1]);
        assert_eq!(parse_pages("2-end", 3).unwrap(), [2, 3]);
        assert!(parse_pages("0", 3).is_err());
        assert!(parse_pages("4", 3).is_err());
        assert!(parse_pages("two", 3).is_err());
        assert!(parse_pages(" , ", 3).is_err());
    }

    #[test]
    fn extracts_pages_in_order() {
        let doc = text_pdf(&["one", "two", "three", "four"]);
        let snipped = extract(&doc, &[4, 2]).unwrap();
        assert_eq!(page_texts(&snipped), ["four", "two"]);
        // The original is untouched.
        assert_eq!(doc.get_pages().len(), 4);
        assert!(extract(&doc, &[1, 1]).is_err());
        assert!(extract(&doc, &[5]).is_err());
    }

    #[test]
    fn leaves_the_rest_behind() {
        let doc = text_pdf(&["keep", "secret"]);
        let mut snipped = extract(&doc, &[1]).unwrap();
        let mut bytes = Vec::new();
        snipped.save_to(&mut bytes).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("(keep)"));
        assert!(!text.contains("secret"), "the dropped page's content is still in the file");
    }

    #[test]
    fn splits_and_names() {
        assert_eq!(chunks(5, 2), [vec![1, 2], vec![3, 4], vec![5]]);
        assert_eq!(chunks(2, 0), [vec![1], vec![2]]);
        let input = Path::new("/tmp/scan.pdf");
        assert_eq!(output_for(input, &[4]), Path::new("/tmp/scan.p4.pdf"));
        assert_eq!(output_for(input, &[4, 5, 6]), Path::new("/tmp/scan.p4-6.pdf"));
        assert_eq!(output_for(input, &[4, 9]), Path::new("/tmp/scan.p4+1.pdf"));
    }
}
