//! The pack: every monster, what it does, and how it looks.
//!
//! The drawings match the ones on the website. They're built from a handful of
//! body, eye and mouth parts rather than shipped as separate files.

use std::{fmt::Write as _, sync::Arc};

use gpui::{Image, ImageFormat};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Munch,
    Squish,
    Stitch,
    Snip,
    Peek,
    Hush,
}

pub const PACK: [Kind; 6] = [
    Kind::Munch,
    Kind::Squish,
    Kind::Stitch,
    Kind::Snip,
    Kind::Peek,
    Kind::Hush,
];

#[derive(Clone, Copy)]
enum Body {
    Round,
    Boxy,
    Tall,
    Ghost,
}

#[derive(Clone, Copy)]
enum Mouth {
    Chomp,
    Grin,
    O,
    Stitch,
    Zip,
    Smirk,
}

pub struct Species {
    pub name: &'static str,
    pub job: &'static str,
    pub blurb: &'static str,
    pub color: u32,
    /// Whether this monster has moved into the app yet.
    pub ready: bool,
    body: Body,
    eyes: u8,
    mouth: Mouth,
    horns: bool,
}

impl Kind {
    pub fn species(self) -> &'static Species {
        match self {
            Kind::Munch => &Species {
                name: "Munch",
                job: "Straightens skewed scans",
                blurb: "Feed it crooked pages. Munch chews off the tilt and spits them back straight.",
                color: 0xff6b4a,
                ready: true,
                body: Body::Round,
                eyes: 2,
                mouth: Mouth::Chomp,
                horns: true,
            },
            Kind::Squish => &Species {
                name: "Squish",
                job: "Shrinks file sizes",
                blurb: "Squeezes bloated PDFs down to something you can actually send.",
                color: 0x7c5cff,
                ready: true,
                body: Body::Boxy,
                eyes: 1,
                mouth: Mouth::Grin,
                horns: false,
            },
            Kind::Stitch => &Species {
                name: "Stitch",
                job: "Merges PDFs",
                blurb: "Drop in a pile of files, drag them into order, get one tidy document.",
                color: 0x21c18a,
                ready: true,
                body: Body::Tall,
                eyes: 2,
                mouth: Mouth::Stitch,
                horns: false,
            },
            Kind::Snip => &Species {
                name: "Snip",
                job: "Splits & extracts pages",
                blurb: "Need pages 4–9 and nothing else? Snip cuts out exactly that.",
                color: 0xffb800,
                ready: true,
                body: Body::Ghost,
                eyes: 3,
                mouth: Mouth::Smirk,
                horns: false,
            },
            Kind::Peek => &Species {
                name: "Peek",
                job: "Makes scans searchable",
                blurb: "Reads every word on your scans so you can search, select and copy them.",
                color: 0x2bb3ff,
                ready: false,
                body: Body::Round,
                eyes: 1,
                mouth: Mouth::O,
                horns: false,
            },
            Kind::Hush => &Species {
                name: "Hush",
                job: "Redacts & strips metadata",
                blurb: "Blacks out the sensitive bits for real and wipes hidden author and edit history.",
                color: 0x5a5a80,
                ready: false,
                body: Body::Boxy,
                eyes: 2,
                mouth: Mouth::Zip,
                horns: true,
            },
        }
    }

    pub fn portrait(self) -> Arc<Image> {
        Arc::new(Image::from_bytes(ImageFormat::Svg, svg(self.species()).into_bytes()))
    }
}

const INK: &str = "#1d1b2e";
const HORN: &str = "#fff6e5";

fn svg(s: &Species) -> String {
    let color = format!("#{:06x}", s.color);
    let mut out = String::from(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 -2 120 124" width="240" height="248">"#,
    );
    let _ = write!(
        out,
        r#"<g stroke="{INK}" stroke-width="4" stroke-linejoin="round" stroke-linecap="round">"#
    );

    if s.horns {
        let top = match s.body {
            Body::Round | Body::Ghost => 14,
            Body::Boxy => 20,
            Body::Tall => 10,
        };
        let _ = write!(
            out,
            r#"<g transform="translate(0 {})" fill="{HORN}"><path d="M38 24 L30 2 L50 17 Z"/><path d="M82 24 L90 2 L70 17 Z"/></g>"#,
            top - 14
        );
    }
    if !matches!(s.body, Body::Ghost) {
        let _ = write!(
            out,
            r#"<g fill="{color}"><ellipse cx="42" cy="110" rx="11" ry="6"/><ellipse cx="78" cy="110" rx="11" ry="6"/></g>"#
        );
    }
    let body = match s.body {
        Body::Round => r#"<path d="M60 14C90 14 106 38 106 66C106 94 88 108 60 108C32 108 14 94 14 66C14 38 30 14 60 14Z"/>"#,
        Body::Boxy => r#"<rect x="16" y="20" width="88" height="88" rx="24"/>"#,
        Body::Tall => r#"<rect x="26" y="10" width="68" height="98" rx="32"/>"#,
        Body::Ghost => r#"<path d="M18 60C18 30 36 14 60 14C84 14 102 30 102 60V110L88 100L74 110L60 100L46 110L32 100L18 110Z"/>"#,
    };
    let _ = write!(out, r#"<g fill="{color}">{body}</g>"#);

    let eyes: &[(f32, f32, f32)] = match s.eyes {
        1 => &[(60., 50., 15.)],
        2 => &[(45., 50., 10.), (75., 50., 10.)],
        _ => &[(40., 54., 8.), (60., 42., 9.), (80., 54., 8.)],
    };
    for &(x, y, r) in eyes {
        let _ = write!(
            out,
            r##"<circle cx="{x}" cy="{y}" r="{r}" fill="#fff"/><circle cx="{}" cy="{}" r="{}" fill="{INK}" stroke="none"/>"##,
            x + r * 0.2,
            y + r * 0.15,
            r * 0.45
        );
    }

    out.push_str(match s.mouth {
        Mouth::Chomp => concat!(
            r##"<path d="M32 72 Q60 108 88 72 Z" fill="#1d1b2e"/>"##,
            r##"<path d="M38 73 L43 81 L48 73 M56 73 L60 82 L64 73 M72 73 L77 81 L82 73" fill="#fff" stroke-width="2.5"/>"##,
            r##"<path d="M48 92 Q60 84 72 92 Q60 98 48 92 Z" fill="#ff8fa8" stroke="none"/>"##,
        ),
        Mouth::Grin => concat!(
            r##"<g fill="none"><path d="M38 76 Q60 96 82 76"/>"##,
            r##"<path d="M52 84 L52 91 L60 91 L60 86" fill="#fff" stroke-width="2.5"/></g>"##,
        ),
        Mouth::O => r##"<ellipse cx="60" cy="82" rx="7" ry="9" fill="#1d1b2e"/>"##,
        Mouth::Stitch => concat!(
            r##"<g fill="none"><path d="M40 80 H80"/>"##,
            r##"<path d="M46 74 V86 M56 74 V86 M66 74 V86 M76 74 V86" stroke-width="3"/></g>"##,
        ),
        Mouth::Zip => r##"<path d="M38 80 L44 74 L50 80 L56 74 L62 80 L68 74 L74 80 L80 74" fill="none"/>"##,
        Mouth::Smirk => r##"<path d="M42 80 Q62 92 80 74" fill="none"/>"##,
    });
    out.push_str("</g></svg>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_portrait_is_well_formed() {
        for kind in PACK {
            let svg = svg(kind.species());
            let tree = usvg::Tree::from_str(&svg, &usvg::Options::default())
                .unwrap_or_else(|e| panic!("{kind:?}: {e}"));
            assert!(tree.root().has_children(), "{kind:?}");
        }
    }
}
