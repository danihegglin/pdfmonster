mod app;
mod deskew;
mod pages;
mod pdf;
mod stitch;

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
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-h" | "--help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(cmd @ ("fix" | "stitch")) => {
            let result = match cmd {
                "fix" => fix(&args[1..]),
                _ => stitch(&args[1..]),
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
