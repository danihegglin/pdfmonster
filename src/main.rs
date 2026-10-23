// Release builds on Windows are GUI apps, so no console window opens behind the app.
// The CLI reattaches to the terminal it was started from (see `attach_parent_console`).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod chain;
mod content;
mod deskew;
mod hush;
mod pages;
mod pdf;
mod peek;
mod render;
mod snip;
mod squish;
mod std_widths;
mod stitch;
mod watch;

use std::{path::PathBuf, process::ExitCode, time::Instant};

const USAGE: &str = "\
pdfmonster — straightens crooked PDF scans

USAGE:
    pdfmonster [FILE.pdf…]                     open the app (one file opens Munch,
                                               several open Stitch)
    pdfmonster fix <IN.pdf> [-o OUT.pdf]       straighten headlessly
                                               (default OUT: IN.aligned.pdf)
    pdfmonster stitch <A.pdf> <B.pdf>… [-o OUT.pdf]
                                               merge PDFs in the order given
                                               (default OUT: A.stitched.pdf)
    pdfmonster snip <IN.pdf> <PAGES>… [-o OUT.pdf]
                                               keep only some pages, e.g. 4-9 or 1,3,10-
                                               (one file per PAGES; default IN.p4-9.pdf)
    pdfmonster snip <IN.pdf> --every N         split into files of N pages each
    pdfmonster hush <IN.pdf> [--text WORDS]… [--box PAGE:X0,Y0,X1,Y1]… [--keep-metadata] [-o OUT.pdf]
                                               redact for real and strip metadata
                                               (default OUT: IN.hushed.pdf)
    pdfmonster squish <IN.pdf> [--light|--strong] [-o OUT.pdf]
                                               shrink the file (default OUT: IN.squished.pdf)
    pdfmonster peek <IN.pdf> [-o OUT.pdf]      read scanned pages and make them searchable
                                               (default OUT: IN.searchable.pdf)
    pdfmonster chain <STEPS> <IN.pdf>… [-o OUT.pdf]
                                               run monsters one after the other, e.g.
                                               \"munch > peek > squish\" (steps: munch, peek,
                                               squish[:light|:strong], hush)
                                               (default OUT: IN.chained.pdf)
    pdfmonster watch <FOLDER> <STEPS>          run a chain on every PDF that lands in FOLDER;
                                               results go to FOLDER/done, originals to
                                               FOLDER/originals (stop with Ctrl-C)
";

fn main() -> ExitCode {
    attach_parent_console();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-h" | "--help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(cmd @ ("fix" | "stitch" | "snip" | "squish" | "hush" | "peek" | "chain" | "watch")) => {
            let result = match cmd {
                "fix" => fix(&args[1..]),
                "stitch" => stitch(&args[1..]),
                "snip" => snip(&args[1..]),
                "hush" => hush(&args[1..]),
                "peek" => peek(&args[1..]),
                "chain" => chain(&args[1..]),
                "watch" => watch(&args[1..]),
                _ => squish(&args[1..]),
            };
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("pdfmonster: {err:#}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            app::run(args.iter().map(PathBuf::from).collect());
            ExitCode::SUCCESS
        }
    }
}

/// A GUI-subsystem exe gets no console, so `--help` and the subcommands would print nothing
/// when run from a terminal. Attach to the parent's console unless output is already redirected.
#[cfg(windows)]
fn attach_parent_console() {
    use std::ffi::c_void;
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
        fn GetStdHandle(std_handle: u32) -> *mut c_void;
    }
    // SAFETY: plain Win32 calls without pointer arguments.
    unsafe {
        let out = GetStdHandle(STD_OUTPUT_HANDLE);
        if out.is_null() || out as isize == -1 {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}

/// Splits a subcommand's arguments into inputs and the `-o` output.
fn inputs_and_output(args: &[String]) -> anyhow::Result<(Vec<PathBuf>, Option<PathBuf>)> {
    let mut inputs = Vec::new();
    let mut output = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-o" | "--output" => {
                output = Some(PathBuf::from(
                    it.next().ok_or_else(|| anyhow::anyhow!("-o needs a file name"))?,
                ))
            }
            _ => inputs.push(PathBuf::from(arg)),
        }
    }
    Ok((inputs, output))
}

fn fix(args: &[String]) -> anyhow::Result<()> {
    let (inputs, output) = inputs_and_output(args)?;
    let input = match &inputs[..] {
        [input] => input.clone(),
        [] => anyhow::bail!("missing input file\n\n{USAGE}"),
        [_, extra, ..] => anyhow::bail!("unexpected argument {extra:?}\n\n{USAGE}"),
    };
    let output = output.unwrap_or_else(|| pdf::default_output(&input));

    let start = Instant::now();
    let doc = pdf::open(&input)?;
    let pages = pdf::analyze(&doc);
    for page in &pages {
        match (page.note, page.skew) {
            (Some(note), _) => println!("page {:>4}: skipped ({note})", page.number),
            (None, Some(skew)) => println!(
                "page {:>4}: {:+.2}° (confidence {:.1}){}",
                page.number,
                skew.degrees,
                skew.confidence,
                if page.suggested_angle() == 0.0 { " — left as is" } else { "" }
            ),
            _ => {}
        }
    }
    let corrections: Vec<_> = pages.iter().map(|p| (p.id, p.suggested_angle())).collect();
    let changed = pdf::save_straightened(doc, &corrections, &output)?;
    println!(
        "straightened {changed}/{} pages → {} in {} ms",
        pages.len(),
        output.display(),
        start.elapsed().as_millis()
    );
    Ok(())
}

fn stitch(args: &[String]) -> anyhow::Result<()> {
    let (inputs, output) = inputs_and_output(args)?;
    anyhow::ensure!(inputs.len() >= 2, "stitch needs at least two PDFs\n\n{USAGE}");
    let output = output.unwrap_or_else(|| stitch::default_output(&inputs[0]));

    let start = Instant::now();
    let docs = inputs.iter().map(|p| pdf::open(p)).collect::<anyhow::Result<Vec<_>>>()?;
    let counts: Vec<usize> = docs.iter().map(|d| d.get_pages().len()).collect();
    let mut merged = stitch::merge(docs)?;
    merged
        .save(&output)
        .map_err(|e| anyhow::anyhow!("could not write {}: {e}", output.display()))?;
    for (path, count) in inputs.iter().zip(&counts) {
        println!("{:>5} page{} from {}", count, if *count == 1 { " " } else { "s" }, path.display());
    }
    println!(
        "stitched {} pages → {} in {} ms",
        counts.iter().sum::<usize>(),
        output.display(),
        start.elapsed().as_millis()
    );
    Ok(())
}

fn snip(args: &[String]) -> anyhow::Result<()> {
    let mut args = args.to_vec();
    let every = match args.iter().position(|a| a == "--every") {
        Some(i) => {
            let n = args
                .get(i + 1)
                .and_then(|n| n.parse::<u32>().ok())
                .filter(|&n| n > 0)
                .ok_or_else(|| anyhow::anyhow!("--every needs a page count"))?;
            args.drain(i..=i + 1);
            Some(n)
        }
        None => None,
    };
    let (inputs, output) = inputs_and_output(&args)?;
    let Some((input, specs)) = inputs.split_first() else {
        anyhow::bail!("missing input file\n\n{USAGE}");
    };
    let start = Instant::now();
    let doc = pdf::open(input)?;
    let count = doc.get_pages().len() as u32;

    let groups: Vec<Vec<u32>> = match every {
        Some(n) => {
            anyhow::ensure!(specs.is_empty(), "use either --every or page lists, not both");
            snip::chunks(count, n)
        }
        None => {
            anyhow::ensure!(!specs.is_empty(), "which pages? e.g. 4-9\n\n{USAGE}");
            specs
                .iter()
                .map(|s| snip::parse_pages(&s.to_string_lossy(), count))
                .collect::<anyhow::Result<_>>()?
        }
    };
    anyhow::ensure!(
        output.is_none() || groups.len() == 1,
        "-o only works when writing a single file"
    );
    for group in &groups {
        let out = match &output {
            Some(out) => out.clone(),
            None => snip::output_for(input, group),
        };
        snip::extract(&doc, group)?
            .save(&out)
            .map_err(|e| anyhow::anyhow!("could not write {}: {e}", out.display()))?;
        println!("{:>5} page{} → {}", group.len(), if group.len() == 1 { " " } else { "s" }, out.display());
    }
    println!(
        "snipped {} file{} from {count} pages in {} ms",
        groups.len(),
        if groups.len() == 1 { "" } else { "s" },
        start.elapsed().as_millis()
    );
    Ok(())
}

fn squish(args: &[String]) -> anyhow::Result<()> {
    let mut level = squish::Level::default();
    let rest: Vec<String> = args
        .iter()
        .filter(|a| match a.strip_prefix("--").and_then(squish::Level::parse) {
            Some(l) => {
                level = l;
                false
            }
            None => true,
        })
        .cloned()
        .collect();
    let (inputs, output) = inputs_and_output(&rest)?;
    let input = match &inputs[..] {
        [input] => input.clone(),
        [] => anyhow::bail!("missing input file\n\n{USAGE}"),
        [_, extra, ..] => anyhow::bail!("unexpected argument {extra:?}\n\n{USAGE}"),
    };
    let output = output.unwrap_or_else(|| squish::default_output(&input));

    let start = Instant::now();
    let before = std::fs::metadata(&input)?.len();
    let mut doc = pdf::open(&input)?;
    let report = squish::squish(&mut doc, level);
    let bytes = squish::to_bytes(&mut doc)?;
    std::fs::write(&output, &bytes)
        .map_err(|e| anyhow::anyhow!("could not write {}: {e}", output.display()))?;
    let after = bytes.len() as u64;
    println!(
        "{level} squish: {}/{} images shrunk, {} → {} ({:.0}% smaller) → {} in {} ms",
        report.shrunk,
        report.images,
        human_size(before),
        human_size(after),
        100.0 - after as f64 * 100.0 / before.max(1) as f64,
        output.display(),
        start.elapsed().as_millis(),
        level = level.label(),
    );
    Ok(())
}

fn peek(args: &[String]) -> anyhow::Result<()> {
    let (inputs, output) = inputs_and_output(args)?;
    let input = match &inputs[..] {
        [input] => input.clone(),
        [] => anyhow::bail!("missing input file\n\n{USAGE}"),
        [_, extra, ..] => anyhow::bail!("unexpected argument {extra:?}\n\n{USAGE}"),
    };
    let output = output.unwrap_or_else(|| peek::default_output(&input));

    let start = Instant::now();
    let data = std::sync::Arc::new(std::fs::read(&input)?);
    let mut doc = lopdf::Document::load_mem(&data)
        .map_err(|e| anyhow::anyhow!("could not read {}: {e}", input.display()))?;
    let pages = peek::read(&doc, data, &|| {})?;
    for (number, page) in (1..).zip(&pages) {
        match page {
            peek::PageText::HadText => println!("page {number:>4}: already has text"),
            peek::PageText::Unreadable => println!("page {number:>4}: couldn't be drawn, skipped"),
            peek::PageText::Read(lines) => {
                let words: usize = lines.iter().map(|l| l.words.len()).sum();
                let first = lines.first().map(peek::Line::text).unwrap_or_default();
                println!("page {number:>4}: {words} words  {first:.60}");
            }
        }
    }
    let report = peek::add_text(&mut doc, &pages)?;
    doc.save(&output)
        .map_err(|e| anyhow::anyhow!("could not write {}: {e}", output.display()))?;
    println!(
        "read {} page{} ({} words, {} already had text) → {} in {} ms",
        report.read,
        if report.read == 1 { "" } else { "s" },
        report.words,
        report.had_text,
        output.display(),
        start.elapsed().as_millis()
    );
    Ok(())
}

fn chain(args: &[String]) -> anyhow::Result<()> {
    let Some((steps, rest)) = args.split_first() else {
        anyhow::bail!("which monsters? e.g. \"munch > peek > squish\"\n\n{USAGE}");
    };
    let chain = chain::Chain::parse(steps)?;
    let (inputs, output) = inputs_and_output(rest)?;
    anyhow::ensure!(!inputs.is_empty(), "missing input file\n\n{USAGE}");
    anyhow::ensure!(output.is_none() || inputs.len() == 1, "-o only works with a single input");
    for input in &inputs {
        let start = Instant::now();
        let out = output.clone().unwrap_or_else(|| chain::default_output(input));
        let (bytes, log) = chain::run(pdf::open(input)?, &chain, &|_, _| {})?;
        std::fs::write(&out, &bytes).map_err(|e| anyhow::anyhow!("could not write {}: {e}", out.display()))?;
        println!("{}:", input.display());
        for line in &log {
            println!("    {line}");
        }
        println!("    → {} ({}) in {} ms", out.display(), human_size(bytes.len() as u64), start.elapsed().as_millis());
    }
    Ok(())
}

fn watch(args: &[String]) -> anyhow::Result<()> {
    let [folder, steps] = args else {
        anyhow::bail!("usage: pdfmonster watch <FOLDER> <STEPS>\n\n{USAGE}");
    };
    let folder = PathBuf::from(folder);
    anyhow::ensure!(folder.is_dir(), "{} is not a folder", folder.display());
    let chain = chain::Chain::parse(steps)?;
    println!("watching {} with {chain} (Ctrl-C to stop)", folder.display());
    let mut watcher = watch::Watcher::default();
    loop {
        for input in watcher.ready(&folder) {
            let processed = watch::process(&input, &chain);
            let name = input.file_name().unwrap_or_default().to_string_lossy().into_owned();
            match processed {
                Ok(done) => {
                    println!(
                        "{name} → {} ({} → {}) in {} ms",
                        done.output.display(),
                        human_size(done.before),
                        human_size(done.after),
                        done.millis
                    );
                    for line in &done.log {
                        println!("    {line}");
                    }
                }
                Err(err) => println!("{name}: failed, moved to {}/: {err:#}", watch::FAILED),
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

pub fn human_size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.0} KB", b as f64 / (1 << 10) as f64),
        b => format!("{b} B"),
    }
}

fn hush(args: &[String]) -> anyhow::Result<()> {
    let mut texts = Vec::new();
    let mut boxes = Vec::new();
    let mut keep_metadata = false;
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--text" => texts.push(it.next().ok_or_else(|| anyhow::anyhow!("--text needs the words to redact"))?.clone()),
            "--box" => {
                let spec = it.next().ok_or_else(|| anyhow::anyhow!("--box needs PAGE:X0,Y0,X1,Y1"))?;
                boxes.push(parse_box(spec)?);
            }
            "--keep-metadata" => keep_metadata = true,
            _ => rest.push(arg.clone()),
        }
    }
    let (inputs, output) = inputs_and_output(&rest)?;
    let input = match &inputs[..] {
        [input] => input.clone(),
        [] => anyhow::bail!("missing input file\n\n{USAGE}"),
        [_, extra, ..] => anyhow::bail!("unexpected argument {extra:?}\n\n{USAGE}"),
    };
    let output = output.unwrap_or_else(|| hush::default_output(&input));

    let start = Instant::now();
    let mut doc = pdf::open(&input)?;
    for text in &texts {
        let found = hush::find_text(&doc, text);
        println!("{:>5} × {text:?}", found.len());
        boxes.extend(found);
    }
    let report = hush::redact(&mut doc, &boxes)?;
    for note in &report.removed_whole {
        println!("removed whole: {note}");
    }
    let metadata = if keep_metadata { Vec::new() } else { hush::describe_metadata(&doc) };
    if !keep_metadata {
        hush::strip_metadata(&mut doc);
    }
    doc.save(&output)
        .map_err(|e| anyhow::anyhow!("could not write {}: {e}", output.display()))?;
    for item in &metadata {
        println!("wiped {item}");
    }
    println!(
        "hushed {} boxes ({} glyphs, {} images, {} annotations) → {} in {} ms",
        boxes.len(),
        report.glyphs,
        report.images,
        report.annotations,
        output.display(),
        start.elapsed().as_millis()
    );
    Ok(())
}

/// `3:72,700,200,720` → a box on page 3.
fn parse_box(spec: &str) -> anyhow::Result<hush::Area> {
    let parse = || -> Option<hush::Area> {
        let (page, rect) = spec.split_once(':')?;
        let n: Vec<f64> = rect.split(',').map(|v| v.trim().parse().ok()).collect::<Option<_>>()?;
        let [x0, y0, x1, y1] = n[..] else { return None };
        Some(hush::Area { page: page.trim().parse().ok()?, rect: [x0, y0, x1, y1] })
    };
    parse().ok_or_else(|| anyhow::anyhow!("{spec:?} is not PAGE:X0,Y0,X1,Y1 (in points, from the bottom left)"))
}
