//! Snip's screen: pick pages, cut them out into a new PDF or split them up.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use gpui::{
    ClickEvent, Context, ExternalPaths, FocusHandle, Hsla, Image, PathPromptOptions, RenderImage,
    SharedString, Window, div, img, prelude::*, px, rgb,
};
use lopdf::Document;

use super::{
    Open, Save, button, empty_state, file_name, is_pdf, pack::Kind, plural, render_rgba,
    status_bar, theme, toolbar,
};
use crate::{render, snip};

const THUMB_WIDTH: f32 = 150.0;

pub struct Snip {
    pub focus: FocusHandle,
    portrait: Arc<Image>,
    loaded: Option<Loaded>,
    /// Where a shift-click range starts.
    anchor: Option<usize>,
    busy: Option<SharedString>,
    status: SharedString,
}

struct Loaded {
    path: PathBuf,
    doc: Arc<Document>,
    pages: Vec<Thumb>,
}

struct Thumb {
    image: Option<Arc<RenderImage>>,
    aspect: f32,
    selected: bool,
}

impl Snip {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Snip {
            focus: cx.focus_handle(),
            portrait: Kind::Snip.portrait(),
            loaded: None,
            anchor: None,
            busy: None,
            status: "Feed me a PDF and pick the pages you want.".into(),
        }
    }

    pub fn load(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.busy = Some(format!("Sharpening scissors for {}…", file_name(&path)).into());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let bytes = std::fs::read(&path)
                        .map_err(|e| anyhow::anyhow!("could not open {}: {e}", path.display()))?;
                    let doc = Document::load_mem(&bytes)?;
                    let thumbs = render::thumbnails(Arc::new(bytes), THUMB_WIDTH * 2.0);
                    let count = doc.get_pages().len();
                    let pages = (0..count)
                        .map(|ix| {
                            let image = thumbs.get(ix).and_then(Option::as_ref);
                            Thumb {
                                aspect: image.map_or(1.414, |i| i.height() as f32 / i.width() as f32),
                                image: image.map(render_rgba),
                                selected: false,
                            }
                        })
                        .collect();
                    anyhow::Ok(Loaded { path, doc: Arc::new(doc), pages })
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
                        this.anchor = None;
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

    /// Selected page numbers (1-based), or every page if none are selected.
    fn chosen(&self, all_if_none: bool) -> Vec<u32> {
        let Some(loaded) = &self.loaded else { return Vec::new() };
        let selected: Vec<u32> = loaded
            .pages
            .iter()
            .enumerate()
            .filter(|(_, p)| p.selected)
            .map(|(ix, _)| ix as u32 + 1)
            .collect();
        if selected.is_empty() && all_if_none {
            (1..=loaded.pages.len() as u32).collect()
        } else {
            selected
        }
    }

    fn summary(&self) -> SharedString {
        let Some(loaded) = &self.loaded else {
            return "Feed me a PDF and pick the pages you want.".into();
        };
        let total = loaded.pages.len();
        match self.chosen(false).len() {
            0 => format!("{total} page{} · click pages to pick them", plural(total)).into(),
            n => format!("{total} page{} · {n} picked", plural(total)).into(),
        }
    }

    fn open(&mut self, _: &Open, _: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Snip".into()),
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

    fn click_page(&mut self, ix: usize, event: &ClickEvent, cx: &mut Context<Self>) {
        let Some(loaded) = &mut self.loaded else { return };
        match self.anchor {
            Some(anchor) if event.modifiers().shift => {
                let (a, b) = (anchor.min(ix), anchor.max(ix));
                for page in &mut loaded.pages[a..=b] {
                    page.selected = true;
                }
            }
            _ => {
                loaded.pages[ix].selected = !loaded.pages[ix].selected;
                self.anchor = Some(ix);
            }
        }
        self.status = self.summary();
        cx.notify();
    }

    fn select_all(&mut self, selected: bool, cx: &mut Context<Self>) {
        if let Some(loaded) = &mut self.loaded {
            loaded.pages.iter_mut().for_each(|p| p.selected = selected);
        }
        self.anchor = None;
        self.status = self.summary();
        cx.notify();
    }

    /// Saves the picked pages as one new PDF.
    fn save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        let Some(loaded) = &self.loaded else { return };
        let numbers = self.chosen(false);
        if numbers.is_empty() || self.busy.is_some() {
            return;
        }
        let suggested = snip::output_for(&loaded.path, &numbers);
        let dir = suggested.parent().unwrap_or(Path::new(".")).to_path_buf();
        let target = cx.prompt_for_new_path(&dir, Some(&file_name(&suggested)));
        let doc = loaded.doc.clone();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(out))) = target.await else { return };
            this.update(cx, |this, cx| {
                this.busy = Some("Snipping…".into());
                cx.notify();
            })
            .ok();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let start = Instant::now();
                    write(&doc, &numbers, &out)?;
                    anyhow::Ok(format!(
                        "Snipped {} page{} → {} in {} ms. Snip snip.",
                        numbers.len(),
                        plural(numbers.len()),
                        file_name(&out),
                        start.elapsed().as_millis()
                    ))
                })
                .await;
            this.update(cx, |this, cx| this.finish(result, cx)).ok();
        })
        .detach();
    }

    /// Saves every picked page (or every page) as its own PDF in a folder.
    fn split(&mut self, cx: &mut Context<Self>) {
        let Some(loaded) = &self.loaded else { return };
        if self.busy.is_some() {
            return;
        }
        let numbers = self.chosen(true);
        let folder = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Save pages here".into()),
        });
        let doc = loaded.doc.clone();
        let input = loaded.path.clone();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(dirs))) = folder.await else { return };
            let Some(dir) = dirs.into_iter().next() else { return };
            this.update(cx, |this, cx| {
                this.busy = Some("Splitting…".into());
                cx.notify();
            })
            .ok();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let start = Instant::now();
                    let name = input.file_name().unwrap_or_default();
                    for &n in &numbers {
                        write(&doc, &[n], &snip::output_for(&dir.join(name), &[n]))?;
                    }
                    anyhow::Ok(format!(
                        "Split into {} file{} in {} ({} ms).",
                        numbers.len(),
                        plural(numbers.len()),
                        dir.display(),
                        start.elapsed().as_millis()
                    ))
                })
                .await;
            this.update(cx, |this, cx| this.finish(result, cx)).ok();
        })
        .detach();
    }

    fn finish(&mut self, result: anyhow::Result<String>, cx: &mut Context<Self>) {
        self.busy = None;
        self.status = match result {
            Ok(message) => message.into(),
            Err(err) => format!("Couldn't save: {err:#}").into(),
        };
        cx.notify();
    }

    fn render_page(&self, ix: usize, page: &Thumb, cx: &mut Context<Self>) -> gpui::AnyElement {
        let border: Hsla = if page.selected {
            rgb(Kind::Snip.species().color).into()
        } else {
            gpui::transparent_black()
        };
        div()
            .id(("page", ix))
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_lg()
            .bg(rgb(if page.selected { theme::CARD_HOVER } else { theme::CARD }))
            .border_2()
            .border_color(border)
            .cursor_pointer()
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.click_page(ix, event, cx)
            }))
            .child(
                div()
                    .w(px(THUMB_WIDTH))
                    .h(px(THUMB_WIDTH * page.aspect))
                    .rounded_md()
                    .overflow_hidden()
                    .bg(gpui::white())
                    .when(!page.selected && !self.chosen(false).is_empty(), |d| d.opacity(0.5))
                    .when_some(page.image.clone(), |d, image| d.child(img(image).size_full())),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_sm()
                    .child(div().text_color(rgb(theme::MUTED)).child(format!("p. {}", ix + 1)))
                    .when(page.selected, |d| {
                        d.child(div().text_color(rgb(Kind::Snip.species().color)).child("✂ keep"))
                    }),
            )
            .into_any_element()
    }
}

/// Writes the given pages of `doc` to `out`.
fn write(doc: &Document, numbers: &[u32], out: &Path) -> anyhow::Result<()> {
    snip::extract(doc, numbers)?
        .save(out)
        .map_err(|e| anyhow::anyhow!("could not write {}: {e}", out.display()))?;
    Ok(())
}

impl Render for Snip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.loaded {
            None => empty_state(
                self.portrait.clone(),
                "Feed me a PDF",
                "Then pick the pages you want, and I'll cut out exactly those.",
                button("open-big", "Open a PDF…", true)
                    .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.open(&Open, w, cx))),
            )
            .into_any_element(),
            Some(loaded) => {
                let cards: Vec<_> = loaded
                    .pages
                    .iter()
                    .enumerate()
                    .map(|(ix, page)| self.render_page(ix, page, cx))
                    .collect();
                div()
                    .id("pages")
                    .flex_1()
                    .overflow_y_scroll()
                    .p_4()
                    .child(div().flex().flex_wrap().gap_4().children(cards))
                    .into_any_element()
            }
        };

        let title = self.loaded.as_ref().map(|l| file_name(&l.path)).unwrap_or_default();
        let picked = self.chosen(false).len();
        let mut buttons = vec![
            button("open", "Open…", false)
                .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.open(&Open, w, cx)))
                .into_any_element(),
        ];
        if self.loaded.is_some() {
            buttons.push(if picked > 0 {
                button("none", "Pick none", false)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.select_all(false, cx)))
                    .into_any_element()
            } else {
                button("all", "Pick all", false)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.select_all(true, cx)))
                    .into_any_element()
            });
            buttons.push(
                button(
                    "split",
                    if picked > 0 { "Split picked into files" } else { "Split into single pages" },
                    false,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.split(cx)))
                .into_any_element(),
            );
            if picked > 0 {
                buttons.push(
                    button("save", format!("Save {picked} page{}", plural(picked)), true)
                        .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.save(&Save, w, cx)))
                        .into_any_element(),
                );
            }
        }
        let status = self.busy.clone().unwrap_or_else(|| self.status.clone());
        let hint = self
            .loaded
            .is_some()
            .then_some("click to pick · shift-click for a range · esc: all monsters");

        div()
            .key_context("Snip")
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
            .child(toolbar(self.portrait.clone(), Kind::Snip, title, buttons))
            .child(body)
            .child(status_bar(status, self.busy.is_some(), hint))
    }
}
