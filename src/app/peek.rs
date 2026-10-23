//! Peek's screen: drop in a scan, see what Peek read on each page, save it searchable.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use gpui::{
    ClickEvent, Context, ExternalPaths, FocusHandle, FontWeight, Image, PathPromptOptions, RenderImage,
    SharedString, Window, div, img, prelude::*, px, rgb,
};
use lopdf::Document;

use super::{
    Open, Save, button, empty_state, file_name, is_pdf, pack::Kind, plural, render_rgba, status_bar, theme,
    toolbar,
};
use crate::{peek, render};

const THUMB_WIDTH: f32 = 130.0;
/// Lines of read text shown per page; the rest is in the saved file.
const PREVIEW_LINES: usize = 12;

pub struct Peek {
    pub focus: FocusHandle,
    portrait: Arc<Image>,
    loaded: Option<Loaded>,
    busy: Option<SharedString>,
    status: SharedString,
}

struct Loaded {
    path: PathBuf,
    pages: Vec<Row>,
    report: peek::Report,
    /// The searchable file, ready to write.
    output: Arc<Vec<u8>>,
}

struct Row {
    image: Option<Arc<RenderImage>>,
    aspect: f32,
    text: Option<peek::PageText>,
}

impl Peek {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Peek {
            focus: cx.focus_handle(),
            portrait: Kind::Peek.portrait(),
            loaded: None,
            busy: None,
            status: "Feed me a scan and I'll read it.".into(),
        }
    }

    pub fn load(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let name = file_name(&path);
        self.busy = Some(format!("Peeking at {name}…").into());
        cx.notify();
        let done = Arc::new(AtomicUsize::new(0));
        let total = Arc::new(AtomicUsize::new(0));

        // Counts pages up in the status bar while the reading runs.
        let (progress_done, progress_total) = (done.clone(), total.clone());
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(200)).await;
                let keep_going = this
                    .update(cx, |this, cx| {
                        let total = progress_total.load(Ordering::Relaxed);
                        if this.busy.is_none() {
                            return false;
                        }
                        if total > 0 {
                            let done = progress_done.load(Ordering::Relaxed).min(total);
                            this.busy = Some(format!("Reading {name}: page {done} of {total}…").into());
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        })
        .detach();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let data = Arc::new(
                        std::fs::read(&path)
                            .map_err(|e| anyhow::anyhow!("could not open {}: {e}", path.display()))?,
                    );
                    let mut doc = Document::load_mem(&data)?;
                    total.store(doc.get_pages().len(), Ordering::Relaxed);
                    let texts = peek::read(&doc, data.clone(), &|| {
                        done.fetch_add(1, Ordering::Relaxed);
                    })?;
                    let report = peek::add_text(&mut doc, &texts)?;
                    let mut output = Vec::new();
                    doc.save_to(&mut output)?;
                    let thumbs = render::thumbnails(data, THUMB_WIDTH * 2.0);
                    let pages = texts
                        .into_iter()
                        .enumerate()
                        .map(|(ix, text)| {
                            let image = thumbs.get(ix).and_then(Option::as_ref);
                            Row {
                                aspect: image.map_or(1.414, |i| i.height() as f32 / i.width() as f32),
                                image: image.map(render_rgba),
                                text: Some(text),
                            }
                        })
                        .collect();
                    anyhow::Ok(Loaded { path, pages, report, output: Arc::new(output) })
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(loaded) => {
                        if let Some(old) = this.loaded.replace(loaded) {
                            for image in old.pages.into_iter().filter_map(|p| p.image) {
                                cx.drop_image(image, None);
                            }
                        }
                        this.status = this.summary();
                    }
                    Err(err) => this.status = format!("Blergh, couldn't read that: {err:#}").into(),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn summary(&self) -> SharedString {
        let Some(loaded) = &self.loaded else { return "Feed me a scan and I'll read it.".into() };
        let r = &loaded.report;
        match (r.read, r.had_text) {
            (0, 0) => "I couldn't draw any of these pages, so there was nothing to read.".into(),
            (0, _) => "Every page already has text. Nothing for me to do here.".into(),
            (read, 0) => format!("Read {read} page{} · {} words", plural(read), r.words).into(),
            (read, had) => format!(
                "Read {read} page{} · {} words · {had} already had text",
                plural(read),
                r.words
            )
            .into(),
        }
    }

    fn open(&mut self, _: &Open, _: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Peek".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(path) = paths.into_iter().next()
            {
                this.update(cx, |this, cx| this.load(path, cx)).ok();
            }
        })
        .detach();
    }

    fn save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        let Some(loaded) = &self.loaded else { return };
        if self.busy.is_some() || loaded.report.read == 0 {
            return;
        }
        let suggested = peek::default_output(&loaded.path);
        let dir = suggested.parent().unwrap_or(Path::new(".")).to_path_buf();
        let target = cx.prompt_for_new_path(&dir, Some(&file_name(&suggested)));
        let bytes = loaded.output.clone();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(out))) = target.await else { return };
            let result = cx.background_executor().spawn(async move { std::fs::write(&out, &*bytes).map(|_| out) }).await;
            this.update(cx, |this, cx| {
                this.status = match result {
                    Ok(out) => format!("Saved {}. Go on, search it.", file_name(&out)).into(),
                    Err(err) => format!("Couldn't save: {err}").into(),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_row(&self, ix: usize, row: &Row) -> gpui::AnyElement {
        let color = Kind::Peek.species().color;
        let (label, lines): (String, Vec<String>) = match &row.text {
            Some(peek::PageText::Read(lines)) => {
                let words: usize = lines.iter().map(|l| l.words.len()).sum();
                let mut shown: Vec<String> = lines.iter().take(PREVIEW_LINES).map(peek::Line::text).collect();
                if lines.len() > PREVIEW_LINES {
                    shown.push(format!("… and {} more lines", lines.len() - PREVIEW_LINES));
                }
                (if words == 0 { "no words found".into() } else { format!("{words} word{} read", plural(words)) }, shown)
            }
            Some(peek::PageText::HadText) => ("already searchable".into(), Vec::new()),
            Some(peek::PageText::Unreadable) | None => ("couldn't draw this page".into(), Vec::new()),
        };
        div()
            .flex()
            .gap_4()
            .p_3()
            .rounded_lg()
            .bg(rgb(theme::CARD))
            .child(
                div()
                    .flex_none()
                    .w(px(THUMB_WIDTH))
                    .h(px(THUMB_WIDTH * row.aspect))
                    .rounded_md()
                    .overflow_hidden()
                    .bg(gpui::white())
                    .when_some(row.image.clone(), |d, image| d.child(img(image).size_full())),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .overflow_hidden()
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(div().font_weight(FontWeight::BOLD).child(format!("Page {}", ix + 1)))
                            .child(div().text_color(rgb(color)).child(label)),
                    )
                    .children(
                        lines
                            .into_iter()
                            .map(|line| div().text_sm().text_color(rgb(theme::MUTED)).truncate().child(line)),
                    ),
            )
            .into_any_element()
    }
}

impl Render for Peek {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.loaded {
            None => empty_state(
                self.portrait.clone(),
                "Feed me a scan",
                "I'll read every page and make the text searchable, without changing how it looks.",
                button("open-big", "Open a PDF…", true)
                    .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.open(&Open, w, cx))),
            )
            .into_any_element(),
            Some(loaded) => {
                let rows: Vec<_> = loaded.pages.iter().enumerate().map(|(ix, row)| self.render_row(ix, row)).collect();
                div()
                    .id("pages")
                    .flex_1()
                    .overflow_y_scroll()
                    .p_4()
                    .child(div().flex().flex_col().gap_3().children(rows))
                    .into_any_element()
            }
        };

        let title = self.loaded.as_ref().map(|l| file_name(&l.path)).unwrap_or_default();
        let mut buttons = vec![
            button("open", "Open…", false)
                .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.open(&Open, w, cx)))
                .into_any_element(),
        ];
        if self.loaded.as_ref().is_some_and(|l| l.report.read > 0) {
            buttons.push(
                button("save", "Save searchable", true)
                    .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.save(&Save, w, cx)))
                    .into_any_element(),
            );
        }
        let status = self.busy.clone().unwrap_or_else(|| self.status.clone());

        div()
            .key_context("Peek")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::open))
            .on_action(cx.listener(Self::save))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                match paths.paths().iter().find(|p| is_pdf(p)) {
                    Some(path) => this.load(path.clone(), cx),
                    None => {
                        this.status = "Only PDFs, please. I'm a picky eater.".into();
                        cx.notify();
                    }
                }
            }))
            .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(rgb(0x221c33)))
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar(self.portrait.clone(), Kind::Peek, title, buttons))
            .child(body)
            .child(status_bar(status, self.busy.is_some(), Some("esc: all monsters")))
    }
}
