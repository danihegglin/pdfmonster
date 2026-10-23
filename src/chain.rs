//! Chains: several monsters working on one PDF, one after the other.
//!
//! "Munch straightens a scan, Peek makes it searchable, Squish shrinks it for
//! email. One drop, three jobs." Only monsters that need no decisions from you
//! can be chained: Munch, Peek, Squish (at a fixed level) and Hush (which, in
//! a chain, wipes metadata).

use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Result;
use lopdf::Document;

use crate::{hush, pdf, peek, squish};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Munch,
    Peek,
    Squish(squish::Level),
    Hush,
}

impl Step {
    /// Every step, in the order they're offered.
    pub const ALL: [Step; 4] = [Step::Munch, Step::Peek, Step::Squish(squish::Level::Medium), Step::Hush];

    /// `munch`, `peek`, `squish`, `squish:strong`, `hush`.
    pub fn parse(s: &str) -> Option<Step> {
        let s = s.trim().to_ascii_lowercase();
        let (name, option) = s.split_once(':').map_or((s.as_str(), None), |(n, o)| (n, Some(o)));
        match (name, option) {
            ("munch", None) => Some(Step::Munch),
            ("peek", None) => Some(Step::Peek),
            ("hush", None) => Some(Step::Hush),
            ("squish", None) => Some(Step::Squish(squish::Level::default())),
            ("squish", Some(level)) => squish::Level::parse(level).map(Step::Squish),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Step::Munch => "Munch",
            Step::Peek => "Peek",
            Step::Squish(_) => "Squish",
            Step::Hush => "Hush",
        }
    }

    /// What the step does, in a few words.
    pub fn job(self) -> String {
        match self {
            Step::Munch => "straightens".into(),
            Step::Peek => "makes it searchable".into(),
            Step::Squish(level) => format!("shrinks it ({})", level.label()),
            Step::Hush => "wipes metadata".into(),
        }
    }
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Step::Squish(level) if *level != squish::Level::default() => write!(f, "squish:{}", level.label()),
            step => f.write_str(&step.name().to_ascii_lowercase()),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Chain(pub Vec<Step>);

impl Chain {
    /// The one from the landing page.
    pub fn classic() -> Chain {
        Chain(vec![Step::Munch, Step::Peek, Step::Squish(squish::Level::default())])
    }

    /// `munch > peek > squish`, also with `→`, commas or spaces between steps.
    pub fn parse(s: &str) -> Result<Chain> {
        let steps = s
            .split(|c: char| c == '>' || c == '→' || c == ',' || c.is_whitespace())
            .filter(|s| !s.is_empty() && *s != "-")
            .map(|s| {
                Step::parse(s).ok_or_else(|| {
                    anyhow::anyhow!("{s:?} can't be chained; use munch, peek, squish[:light|:strong] or hush")
                })
            })
            .collect::<Result<Vec<_>>>()?;
        anyhow::ensure!(!steps.is_empty(), "a chain needs at least one monster");
        Ok(Chain(steps))
    }
}

impl fmt::Display for Chain {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let steps: Vec<String> = self.0.iter().map(Step::to_string).collect();
        f.write_str(&steps.join(" > "))
    }
}

pub fn default_output(input: &Path) -> PathBuf {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("document");
    input.with_file_name(format!("{stem}.chained.pdf"))
}

/// `path`, or `path` with ` 2`, ` 3`… added to its name if that file exists.
pub fn unused(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("document").to_string();
    (2..)
        .map(|n| path.with_file_name(format!("{stem} {n}.pdf")))
        .find(|p| !p.exists())
        .expect("some name is free")
}

/// Runs every step of `chain` on `doc` and returns the finished file, plus one
/// line per step saying what it did. `on_step` is called before each step.
pub fn run(mut doc: Document, chain: &Chain, on_step: &dyn Fn(usize, Step)) -> Result<(Vec<u8>, Vec<String>)> {
    let mut log = Vec::new();
    for (ix, &step) in chain.0.iter().enumerate() {
        on_step(ix, step);
        let line = match step {
            Step::Munch => {
                let pages = pdf::analyze(&doc);
                let corrections: Vec<_> = pages.iter().map(|p| (p.id, p.suggested_angle())).collect();
                let changed = pdf::straighten(&mut doc, &corrections)?;
                format!("Munch straightened {changed} of {} pages", pages.len())
            }
            Step::Peek => {
                let mut data = Vec::new();
                doc.save_to(&mut data)?;
                // Read the saved copy, so pages and bytes are sure to match.
                let data = Arc::new(data);
                doc = Document::load_mem(&data)?;
                let pages = peek::read(&doc, data, &|| {})?;
                let report = peek::add_text(&mut doc, &pages)?;
                format!(
                    "Peek read {} page{} ({} words, {} already had text)",
                    report.read,
                    if report.read == 1 { "" } else { "s" },
                    report.words,
                    report.had_text
                )
            }
            Step::Squish(level) => {
                let report = squish::squish(&mut doc, level);
                format!("Squish shrank {} of {} images ({})", report.shrunk, report.images, level.label())
            }
            Step::Hush => {
                let wiped = hush::strip_metadata(&mut doc);
                format!("Hush wiped {wiped} bit{} of metadata", if wiped == 1 { "" } else { "s" })
            }
        };
        log.push(line);
    }
    let bytes = if chain.0.iter().any(|s| matches!(s, Step::Squish(_))) {
        squish::to_bytes(&mut doc)?
    } else {
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes)?;
        bytes
    };
    Ok((bytes, log))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{pages::tests::text_pdf, squish::Level};

    #[test]
    fn parses_and_prints() {
        let chain = Chain::parse("munch > peek > squish:strong").unwrap();
        assert_eq!(chain.0, [Step::Munch, Step::Peek, Step::Squish(Level::Strong)]);
        assert_eq!(chain.to_string(), "munch > peek > squish:strong");
        assert_eq!(Chain::parse("Munch → Peek, squish").unwrap(), Chain::classic());
        assert_eq!(Chain::parse(&Chain::classic().to_string()).unwrap(), Chain::classic());
        assert!(Chain::parse("munch > stitch").is_err());
        assert!(Chain::parse("squish:huge").is_err());
        assert!(Chain::parse(" > ").is_err());
    }

    #[test]
    fn runs_every_step() {
        let mut doc = text_pdf(&["one", "two"]);
        doc.trailer.set(
            "Info",
            lopdf::dictionary! { "Author" => lopdf::Object::string_literal("Jane Example") },
        );
        let chain = Chain::parse("munch > peek > squish > hush").unwrap();
        let seen = std::sync::Mutex::new(Vec::new());
        let (bytes, log) = run(doc, &chain, &|ix, step| seen.lock().unwrap().push((ix, step))).unwrap();
        assert_eq!(seen.into_inner().unwrap().len(), 4);
        assert_eq!(log.len(), 4);
        assert!(log[1].contains("2 already had text"), "{log:?}");
        let saved = Document::load_mem(&bytes).unwrap();
        assert_eq!(saved.get_pages().len(), 2);
        assert!(saved.trailer.get(b"Info").is_err(), "metadata survived");
    }
}
