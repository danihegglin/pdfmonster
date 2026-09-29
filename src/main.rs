mod app;
mod deskew;
mod pdf;

use std::{path::PathBuf, process::ExitCode, time::Instant};

const USAGE: &str = "\
pdfmonster — straightens crooked PDF scans

USAGE:
    pdfmonster [FILE.pdf]                      open the app (optionally with a file)
    pdfmonster fix <IN.pdf> [-o OUT.pdf]       straighten headlessly
                                               (default OUT: IN.aligned.pdf)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-h" | "--help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("fix") => match fix(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("pdfmonster: {err:#}");
                ExitCode::FAILURE
            }
        },
        other => {
            app::run(other.map(PathBuf::from));
            ExitCode::SUCCESS
        }
    }
}

fn fix(args: &[String]) -> anyhow::Result<()> {
    let mut input = None;
    let mut output = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-o" | "--output" => output = it.next().map(PathBuf::from),
            _ if input.is_none() => input = Some(PathBuf::from(arg)),
            _ => anyhow::bail!("unexpected argument {arg:?}\n\n{USAGE}"),
        }
    }
    let input = input.ok_or_else(|| anyhow::anyhow!("missing input file\n\n{USAGE}"))?;
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
