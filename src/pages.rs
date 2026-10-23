//! Rebuilding a document's page tree, for the monsters that move pages around.

use std::collections::HashSet;

use anyhow::{Context as _, Result};
use lopdf::{Dictionary, Document, Object, ObjectId, dictionary};

use crate::pdf::inherited;

/// Page attributes a page can inherit from its ancestors in the page tree.
const INHERITABLE: [&str; 4] = ["Resources", "MediaBox", "CropBox", "Rotate"];

/// Makes `pages` the document's pages, in this order, under one flat page tree.
///
/// Pages that aren't listed are dropped for real: links and bookmarks pointing
/// at them are cut, and every object only they used is removed from the file.
pub fn set_pages(doc: &mut Document, pages: &[ObjectId]) -> Result<()> {
    anyhow::ensure!(!pages.is_empty(), "a PDF needs at least one page");
    let kept: HashSet<ObjectId> = pages.iter().copied().collect();
    anyhow::ensure!(kept.len() == pages.len(), "a page can only appear once");
    let dropped: HashSet<ObjectId> = doc
        .get_pages()
        .into_values()
        .filter(|id| !kept.contains(id))
        .collect();

    let tree = doc.new_object_id();
    for &id in pages {
        // Copy inherited attributes down before the page leaves its old tree.
        let copied: Vec<(&str, Object)> = INHERITABLE
            .iter()
            .filter_map(|&key| Some((key, inherited(doc, id, key.as_bytes())?.clone())))
            .collect();
        let page = doc.get_dictionary_mut(id).context("page is not a dictionary")?;
        for (key, value) in copied {
            page.set(key, value);
        }
        page.set("Parent", tree);
    }
    doc.objects.insert(
        tree,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => pages.iter().map(|&id| Object::Reference(id)).collect::<Vec<_>>(),
            "Count" => pages.len() as i64,
        }),
    );

    let root = match doc.trailer.get(b"Root").and_then(Object::as_reference) {
        Ok(root) => root,
        Err(_) => {
            let root = doc.add_object(dictionary! { "Type" => "Catalog" });
            doc.trailer.set("Root", root);
            root
        }
    };
    doc.get_dictionary_mut(root)
        .context("document catalog is not a dictionary")?
        .set("Pages", tree);

    prune(doc, &dropped);
    Ok(())
}

/// Removes every object that is no longer reachable from the trailer, after
/// replacing references to `dropped` objects with null.
fn prune(doc: &mut Document, dropped: &HashSet<ObjectId>) {
    fn visit(obj: &mut Object, dropped: &HashSet<ObjectId>, todo: &mut Vec<ObjectId>) {
        match obj {
            Object::Reference(id) if dropped.contains(id) => *obj = Object::Null,
            Object::Reference(id) => todo.push(*id),
            Object::Array(items) => items.iter_mut().for_each(|o| visit(o, dropped, todo)),
            Object::Dictionary(dict) => visit_dict(dict, dropped, todo),
            Object::Stream(stream) => visit_dict(&mut stream.dict, dropped, todo),
            _ => {}
        }
    }
    fn visit_dict(dict: &mut Dictionary, dropped: &HashSet<ObjectId>, todo: &mut Vec<ObjectId>) {
        for (_, value) in dict.iter_mut() {
            visit(value, dropped, todo);
        }
    }

    let mut todo = Vec::new();
    visit_dict(&mut doc.trailer, dropped, &mut todo);
    let mut seen = HashSet::new();
    while let Some(id) = todo.pop() {
        if seen.insert(id)
            && let Some(obj) = doc.objects.get_mut(&id)
        {
            visit(obj, dropped, &mut todo);
        }
    }
    doc.objects.retain(|id, _| seen.contains(id));
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use lopdf::{Stream, content::Content, content::Operation};

    /// Builds a PDF with one page of text per label, nested two levels deep so
    /// pages inherit their size and font from the tree.
    pub fn text_pdf(labels: &[&str]) -> Document {
        let mut doc = Document::with_version("1.7");
        let root_tree = doc.new_object_id();
        let mid_tree = doc.new_object_id();
        let font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        });
        let kids: Vec<Object> = labels
            .iter()
            .map(|label| {
                let content = Content {
                    operations: vec![
                        Operation::new("BT", vec![]),
                        Operation::new("Tf", vec!["F1".into(), 24.into()]),
                        Operation::new("Td", vec![72.into(), 720.into()]),
                        Operation::new("Tj", vec![Object::string_literal(*label)]),
                        Operation::new("ET", vec![]),
                    ],
                };
                let stream = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
                doc.add_object(dictionary! { "Type" => "Page", "Parent" => mid_tree, "Contents" => stream })
                    .into()
            })
            .collect();
        doc.objects.insert(
            mid_tree,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Parent" => root_tree, "Count" => kids.len() as i64, "Kids" => kids,
            }),
        );
        doc.objects.insert(
            root_tree,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => vec![mid_tree.into()], "Count" => labels.len() as i64,
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
            }),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => root_tree });
        doc.trailer.set("Root", catalog);
        doc
    }

    /// The text on each page, in order.
    pub fn page_texts(doc: &Document) -> Vec<String> {
        doc.get_pages()
            .keys()
            .map(|&n| doc.extract_text(&[n]).unwrap().trim().to_string())
            .collect()
    }

    #[test]
    fn reorders_and_drops_pages() {
        let mut doc = text_pdf(&["one", "two", "three"]);
        let pages = doc.get_pages();
        let (one, two, three) = (pages[&1], pages[&2], pages[&3]);
        // A link on page one that points at page two, which is about to go.
        let link = doc.add_object(dictionary! {
            "Type" => "Annot", "Subtype" => "Link",
            "Dest" => vec![two.into(), "Fit".into()],
        });
        doc.get_dictionary_mut(one).unwrap().set("Annots", vec![link.into()]);

        set_pages(&mut doc, &[three, one]).unwrap();

        assert_eq!(page_texts(&doc), ["three", "one"]);
        assert!(!doc.objects.contains_key(&two), "dropped page is still in the file");
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        let saved = Document::load_mem(&bytes).unwrap();
        assert_eq!(page_texts(&saved), ["three", "one"]);
        // Size and fonts were inherited from the old tree; they must survive.
        let first = saved.get_pages()[&1];
        assert!(saved.get_dictionary(first).unwrap().has(b"MediaBox"));
        assert!(saved.get_dictionary(first).unwrap().has(b"Resources"));
    }

    #[test]
    fn refuses_nonsense() {
        let mut doc = text_pdf(&["one"]);
        let one = doc.get_pages()[&1];
        assert!(set_pages(&mut doc, &[]).is_err());
        assert!(set_pages(&mut doc, &[one, one]).is_err());
    }
}
