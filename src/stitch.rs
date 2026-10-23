//! Stitch: sews several PDFs into one, in the order given.

use std::path::{Path, PathBuf};

use anyhow::Result;
use lopdf::{Document, ObjectId, dictionary};

use crate::pages;

/// Merges the documents into a new one. Pages keep their content, size and
/// rotation; bookmarks and forms of the inputs are not carried over.
pub fn merge(docs: impl IntoIterator<Item = Document>) -> Result<Document> {
    let mut out = Document::with_version("1.7");
    let mut next_id = 1;
    let mut page_ids: Vec<ObjectId> = Vec::new();
    for mut doc in docs {
        doc.renumber_objects_with(next_id);
        next_id = doc.max_id + 1;
        page_ids.extend(doc.get_pages().into_values());
        out.objects.extend(doc.objects);
    }
    anyhow::ensure!(!page_ids.is_empty(), "nothing to stitch: no pages");
    out.max_id = next_id - 1;

    let catalog = out.add_object(dictionary! { "Type" => "Catalog" });
    out.trailer.set("Root", catalog);
    pages::set_pages(&mut out, &page_ids)?;
    Ok(out)
}

pub fn default_output(first: &Path) -> PathBuf {
    let stem = first.file_stem().and_then(|s| s.to_str()).unwrap_or("document");
    first.with_file_name(format!("{stem}.stitched.pdf"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::tests::{page_texts, text_pdf};

    #[test]
    fn merges_in_order() {
        let a = text_pdf(&["a1", "a2"]);
        let b = text_pdf(&["b1"]);
        let c = text_pdf(&["c1", "c2"]);
        let mut merged = merge([b, a, c]).unwrap();
        assert_eq!(page_texts(&merged), ["b1", "a1", "a2", "c1", "c2"]);

        // The three old catalogs and page trees are gone.
        let catalogs = merged
            .objects
            .values()
            .filter(|o| o.type_name().ok() == Some(b"Catalog".as_slice()))
            .count();
        assert_eq!(catalogs, 1);

        let mut bytes = Vec::new();
        merged.save_to(&mut bytes).unwrap();
        let saved = Document::load_mem(&bytes).unwrap();
        assert_eq!(page_texts(&saved), ["b1", "a1", "a2", "c1", "c2"]);
    }

    #[test]
    fn needs_pages() {
        assert!(merge([]).is_err());
    }
}
