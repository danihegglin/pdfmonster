//! Watched folders: every PDF dropped into one goes through a chain on its own.
//!
//! A watched folder works like a hot folder:
//! - a PDF that lands in it is processed once it has stopped growing (so
//!   files still being copied or scanned in are left alone until they're done);
//! - the result goes to `done/` inside the folder, under the same name;
//! - the original moves to `originals/`, so the folder empties as it works and
//!   nothing is processed twice;
//! - a file that can't be processed moves to `failed/`, with a note saying why.
//!
//! Folders are polled every couple of seconds rather than watched with OS
//! notifications: polling works the same everywhere, including network drives
//! and synced folders, and costs next to nothing.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Instant, SystemTime},
};

use anyhow::{Context as _, Result};

use crate::{
    chain::{self, Chain},
    pdf,
};

pub const DONE: &str = "done";
pub const ORIGINALS: &str = "originals";
pub const FAILED: &str = "failed";

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Watch {
    pub folder: PathBuf,
    pub chain: Chain,
}

pub struct Done {
    pub output: PathBuf,
    pub before: u64,
    pub after: u64,
    pub log: Vec<String>,
    pub millis: u128,
}

/// Keeps track of which files in a folder are ready.
#[derive(Default)]
pub struct Watcher {
    /// Size and modification time of each PDF when last seen.
    seen: HashMap<PathBuf, (u64, Option<SystemTime>)>,
}

impl Watcher {
    /// PDFs in `folder` that haven't changed since the last call.
    pub fn ready(&mut self, folder: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(folder) else { return Vec::new() };
        let mut now = HashMap::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let hidden = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'));
            let is_pdf = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"));
            let Ok(meta) = entry.metadata() else { continue };
            if hidden || !is_pdf || !meta.is_file() {
                continue;
            }
            now.insert(path, (meta.len(), meta.modified().ok()));
        }
        let mut ready: Vec<PathBuf> = now
            .iter()
            .filter(|(path, state)| state.0 > 0 && self.seen.get(*path) == Some(state))
            .map(|(path, _)| path.clone())
            .collect();
        ready.sort();
        self.seen = now;
        ready
    }
}

/// Runs `chain` on `input`, writes the result to `done/` and moves the input to
/// `originals/` (or `failed/`).
pub fn process(input: &Path, chain: &Chain) -> Result<Done> {
    let folder = input.parent().unwrap_or(Path::new("."));
    let name = input.file_name().unwrap_or_default();
    let result = (|| -> Result<Done> {
        let start = Instant::now();
        let before = std::fs::metadata(input)?.len();
        let (bytes, log) = chain::run(pdf::open(input)?, chain, &|_, _| {})?;
        let done = folder.join(DONE);
        std::fs::create_dir_all(&done).with_context(|| format!("could not create {}", done.display()))?;
        let output = chain::unused(done.join(name));
        std::fs::write(&output, &bytes).with_context(|| format!("could not write {}", output.display()))?;
        move_into(input, &folder.join(ORIGINALS))?;
        Ok(Done { output, before, after: bytes.len() as u64, log, millis: start.elapsed().as_millis() })
    })();
    if let Err(err) = &result {
        // Get it out of the way so it isn't tried again, and say why.
        if let Ok(moved) = move_into(input, &folder.join(FAILED)) {
            let note = moved.with_extension("pdf.txt");
            std::fs::write(note, format!("pdfmonster couldn't process this file: {err:#}\n")).ok();
        }
    }
    result
}

fn move_into(file: &Path, dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    let target = chain::unused(dir.join(file.file_name().unwrap_or_default()));
    if std::fs::rename(file, &target).is_err() {
        // Renaming fails across drives; copy instead.
        std::fs::copy(file, &target).with_context(|| format!("could not move {}", file.display()))?;
        std::fs::remove_file(file).ok();
    }
    Ok(target)
}

/// Where the list of watched folders is kept between runs.
fn config_file() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("pdfmonster").join("watched-folders.txt"))
}

/// The watched folders saved last time.
pub fn load() -> Vec<Watch> {
    config_file()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .map(|text| parse(&text))
        .unwrap_or_default()
}

pub fn save(watches: &[Watch]) -> Result<()> {
    let file = config_file().context("no settings folder on this system")?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&file, format(watches)).with_context(|| format!("could not write {}", file.display()))
}

/// One watch per line: the chain, a tab, then the folder.
fn parse(text: &str) -> Vec<Watch> {
    text.lines()
        .filter_map(|line| {
            let (chain, folder) = line.split_once('\t')?;
            Some(Watch { folder: PathBuf::from(folder), chain: Chain::parse(chain).ok()? })
        })
        .collect()
}

fn format(watches: &[Watch]) -> String {
    watches.iter().map(|w| format!("{}\t{}\n", w.chain, w.folder.display())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::tests::text_pdf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pdfmonster-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn waits_until_files_settle() {
        let dir = temp_dir("settle");
        let mut watcher = Watcher::default();
        std::fs::write(dir.join("a.pdf"), b"%PDF-").unwrap();
        std::fs::write(dir.join("notes.txt"), b"hi").unwrap();
        std::fs::write(dir.join(".hidden.pdf"), b"%PDF-").unwrap();
        assert!(watcher.ready(&dir).is_empty(), "a new file is never ready right away");
        assert_eq!(watcher.ready(&dir), [dir.join("a.pdf")]);
        // Still being written: not ready until it stops changing.
        std::fs::write(dir.join("a.pdf"), b"%PDF-1.7 more").unwrap();
        assert!(watcher.ready(&dir).is_empty());
        assert_eq!(watcher.ready(&dir), [dir.join("a.pdf")]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn processes_into_done_and_moves_originals() {
        let dir = temp_dir("process");
        text_pdf(&["one"]).save(dir.join("scan.pdf")).unwrap();
        std::fs::write(dir.join("broken.pdf"), b"not a pdf").unwrap();
        let chain = Chain::parse("squish > hush").unwrap();

        let ok = process(&dir.join("scan.pdf"), &chain);
        let done = ok.unwrap();
        assert_eq!(done.output, dir.join(DONE).join("scan.pdf"));
        assert!(lopdf::Document::load(&done.output).is_ok());
        assert!(dir.join(ORIGINALS).join("scan.pdf").exists());
        assert!(!dir.join("scan.pdf").exists());

        let bad = process(&dir.join("broken.pdf"), &chain);
        assert!(bad.is_err());
        assert!(dir.join(FAILED).join("broken.pdf").exists());
        assert!(dir.join(FAILED).join("broken.pdf.txt").exists());
        assert!(!dir.join("broken.pdf").exists());

        // The same name again doesn't overwrite the first result.
        text_pdf(&["two"]).save(dir.join("scan.pdf")).unwrap();
        let again = process(&dir.join("scan.pdf"), &chain).unwrap();
        assert_eq!(again.output, dir.join(DONE).join("scan 2.pdf"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn settings_round_trip() {
        let watches = vec![
            Watch { folder: PathBuf::from("/scans/in box"), chain: Chain::classic() },
            Watch { folder: PathBuf::from("C:\\Users\\me\\Scans"), chain: Chain::parse("peek > hush").unwrap() },
        ];
        assert_eq!(parse(&format(&watches)), watches);
        assert!(parse("garbage\nstitch\t/x\n").is_empty());
    }
}
