<p align="center">
  <img src="assets/monster.svg" width="220" alt="Munch, the pdfmonster: a round purple one-eyed monster with yellow horns, proudly holding a perfectly straight page">
</p>

<h1 align="center">pdfmonster</h1>

<p align="center">
  <b>Feed it crooked scans. It spits out straight ones.</b><br>
  A tiny, very fast desktop app (and CLI) that straightens skewed pages in scanned PDFs.
</p>

---

Meet **Munch**. Munch hates wonky scans: the page you slapped on the copier a little sideways, the
receipt that slid while the lid closed, the 200-page stack from the document feeder where every
page is off by a different amount. Munch finds how crooked each page is and puts it straight.

## Why it's fast

- **Native, GPU-drawn UI** built with [gpui](https://www.gpui.rs), the framework behind the Zed editor. No browser, no Electron.
- **Every page is analysed in parallel** on all your CPU cores.
- **Projection-profile skew detection** on a downscaled page: a coarse 0.25° sweep across ±15°, then a
  0.02° refinement. A few milliseconds per page.
- **Lossless output.** Munch never re-encodes your scans. Each page gets a single rotation matrix
  added to its content stream, so the output is the same quality and almost exactly the same size
  as the input. An 8-page, 3 MB scan comes out about 1.6 KB bigger.

On an M-series Mac, an 8-page 200 dpi scan is analysed, straightened and saved in about 50 ms.

## Using the app

```sh
cargo run --release              # opens the app
cargo run --release -- scan.pdf  # opens the app with a file
```

The app opens on the home screen with the whole pack. Click a monster to open it, or drop a PDF
straight onto one to feed it. Dropping anywhere else feeds Munch. <kbd>Esc</kbd> takes you back
to the pack.

1. Drop a PDF on the window (or press <kbd>⌘O</kbd> / <kbd>Ctrl+O</kbd>).
2. Every page shows up already straightened. Crooked pages have a purple outline and a `↻ +1.24°` badge.
3. Press <kbd>Space</kbd> to flip between **before** and **after**.
4. Use <kbd>−</kbd> / <kbd>+</kbd> to nudge a page by 0.1°, and <kbd>↺</kbd> to go back to Munch's guess.
   Click a page to leave it alone.
5. **Save aligned** (<kbd>⌘S</kbd>) writes `yourfile.aligned.pdf`.

### Stitch: merging PDFs

Open Stitch from the pack (or drop several PDFs on the home screen, or run
`pdfmonster a.pdf b.pdf c.pdf`). Drag the files into order, or use the ↑ / ↓ buttons, then press
**Stitch & save** (<kbd>⌘S</kbd>). Pages keep their size, rotation and content exactly; bookmarks
and form fields of the inputs aren't carried over.

### Snip: splitting and extracting pages

Open a PDF in Snip and every page shows up as a thumbnail. Click pages to pick them (shift-click
picks a range), then **Save N pages** (<kbd>⌘S</kbd>) writes just those into a new PDF. **Split
into single pages** writes one file per page (or per picked page) into a folder. Everything that
only the left-out pages used is removed from the new file.

### Squish: shrinking files

Drop a PDF on Squish and it tries all three levels at once, so you can pick by size:
**Light** (300 dpi, for printing), **Medium** (150 dpi, screen and email) and **Strong**
(100 dpi, smallest). Squish works out how large each image is actually drawn on the page,
scales oversized ones down to that resolution and re-encodes them as JPEG, but only keeps the new
version when it's at least 10% smaller. It also compresses uncompressed streams, drops unused
objects and writes compressed object streams. Masks, 1-bit and CMYK images are left alone.

### Hush: redaction and metadata

Drag over anything on a page that has to go, or type a name, an account number or any other
text in the sidebar and press **Find & box it** to box every occurrence. **Save hushed** removes
what's under the boxes from the file for real, rather than only covering it:

- text is taken out glyph by glyph, and the rest of the line stays exactly where it was;
- the covered part of an image is painted black in a copy of the image;
- comments, links and form fields touching a box are deleted;
- anything Hush can't cut precisely (embedded graphics, inline images, images in CCITT, JBIG2 or
  JPEG 2000, text in unknown fonts without widths) is removed whole if it touches a box, and
  Hush tells you;
- then a black box is painted on top.

Hush also lists the hidden metadata it finds (author, title, the app that made the file, dates,
XMP, EXIF camera and GPS data in photos, author names on comments) and wipes it when saving.
Rewriting the file also drops the edit history of incrementally saved PDFs.

Text search needs real text. For scans, run Peek first.

### Peek: making scans searchable

Drop a scan on Peek and it reads every page that doesn't have text yet, then shows you what it
read. **Save searchable** writes the words back onto the pages as invisible text, each one laid
over the word it came from, so you can search, select and copy while the page looks exactly the
same. Pages that already have text are left alone, so running Peek twice does nothing the
second time.

Peek uses [ocrs](https://github.com/robertknight/ocrs), a pure-Rust OCR engine. Its two models
(about 12 MB) are built into pdfmonster, so reading works offline and your scans never leave the
computer. It reads printed Latin-script text (English and other languages without many accents
work best); handwriting and other scripts aren't supported.

### Chains: several monsters in one go

Click **Chain them up** below the pack to line monsters up: Munch straightens, Peek makes it
searchable, Squish shrinks it (click Squish to pick its level), and Hush wipes metadata. Then
drop in one PDF or a whole pile. Each file goes through every step in order and is saved next to
the original as `NAME.chained.pdf`, with a line per step saying what it did. Stitch, Snip and
Hush's redaction need you to make choices, so they aren't chainable.

## Using the CLI

```sh
pdfmonster fix scan.pdf                 # → scan.aligned.pdf
pdfmonster fix scan.pdf -o straight.pdf
pdfmonster stitch a.pdf b.pdf c.pdf     # → a.stitched.pdf
pdfmonster stitch a.pdf b.pdf -o all.pdf
pdfmonster snip scan.pdf 4-9            # → scan.p4-9.pdf
pdfmonster snip scan.pdf 1-3 10-        # → scan.p1-3.pdf and scan.p10-12.pdf
pdfmonster snip scan.pdf 3,1 -o two.pdf # pages 3 and 1, in that order
pdfmonster snip scan.pdf --every 1      # one file per page
pdfmonster squish big.pdf               # → big.squished.pdf (medium)
pdfmonster squish big.pdf --strong -o small.pdf
pdfmonster hush contract.pdf --text "Jane Example" --text "CH93 0076 2011 6238 5295 7"
pdfmonster hush scan.pdf --box 1:72,700,300,730   # page:x0,y0,x1,y1 in points from bottom left
pdfmonster hush report.pdf                        # just wipe the metadata
pdfmonster peek scan.pdf                # → scan.searchable.pdf
pdfmonster chain "munch > peek > squish" scan.pdf more.pdf   # → scan.chained.pdf, more.chained.pdf
pdfmonster chain "peek > squish:strong > hush" scan.pdf -o send.pdf
```

```
page    1: +0.71° (confidence 63.2)
page    2: +3.27° (confidence 64.2)
page    3: -2.25° (confidence 68.9)
...
straightened 8/8 pages → scan.aligned.pdf in 51 ms
```

## Building

You need a recent stable Rust (edition 2024). On macOS, gpui compiles its Metal shaders at
runtime, so you don't need Xcode's Metal toolchain. On Linux, install the usual gpui dependencies
(Vulkan, Wayland/X11 and xkbcommon headers). On Debian/Ubuntu, `packaging/linux/install-deps.sh`
installs them.

```sh
cargo build --release
cargo test
```

## Downloads

Every tagged release (`v*`) is built by GitHub Actions and attached to the
[releases page](https://github.com/danihegglin/pdfmonster/releases):

| Platform | Files |
| --- | --- |
| macOS (Apple silicon + Intel) | `.dmg`, `.app.zip`, CLI `.tar.gz` |
| Windows (x86_64, ARM64) | installer `-setup.exe`, portable `.zip` |
| Debian, Ubuntu, Mint, Pop!\_OS | `.deb` (x86_64, aarch64) |
| Fedora, openSUSE, RHEL | `.rpm` (x86_64, aarch64) |
| Arch, Manjaro, EndeavourOS | `.pkg.tar.zst` (`sudo pacman -U …`) |
| Any Linux | `.AppImage`, `.tar.gz` (x86_64, aarch64) |

The macOS app is ad-hoc signed, not notarized: the first time, right-click it and choose **Open**.

To cut a release, push a version tag: `git tag v0.2.0 && git push --tags`. The tag sets the
version of every package.

## How it works

1. **Find the scan.** For every page, [lopdf](https://github.com/J-F-Liu/lopdf) finds the largest
   embedded image and decodes it as grayscale. JPEG (`DCTDecode`) and Flate/LZW images in gray,
   RGB, CMYK or 1-bit are supported.
2. **Measure the skew.** The page is binarised with an Otsu threshold. For each candidate angle,
   the ink pixels are projected onto the rotated vertical axis. When the angle matches the lines
   of text, the histogram turns into sharp peaks and gaps. Munch picks the angle with the sharpest
   profile. Pages with too little structure, such as photos or blank pages, are left alone.
3. **Straighten losslessly.** The page's content is wrapped in `q … cm … Q` with a rotation around
   the centre of the page, on a white background so the corners stay clean.

## Limitations

- CCITT fax, JBIG2 and JPEG 2000 images can't be decoded yet. Those pages are reported and skipped.
- Munch corrects small skews of up to ±15°. It doesn't turn pages that are upside down or sideways.
- Only the page's biggest image is used to measure the angle, which is what you want for scans.

## License

[GPL-3.0](LICENSE). Munch is free-range.
