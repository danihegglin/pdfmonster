//! Munch's screen: drop a PDF in, eyeball the fixes, save.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    ClickEvent, Context, ExternalPaths, FocusHandle, FontWeight, Hsla, Image, ImageFormat,
    PathPromptOptions, RenderImage, SharedString, Window, div, img, prelude::*, px, rgb,
};
use image::{GrayImage, imageops};
use lopdf::{Document, ObjectId};
use rayon::prelude::*;

use super::{
    Open, Save, ToggleOriginal, button, file_name, is_pdf, pack::Kind, render_image, small_button,
    status_bar, theme, toolbar,
};
use crate::{deskew, pdf};

const MASCOT: &[u8] = include_bytes!("../../assets/monster.svg");
const CARD_WIDTH: f32 = 200.0;
const NUDGE: f32 = 0.1;

pub struct Munch {
    pub focus: FocusHandle,
    mascot: Arc<Image>,
    portrait: Arc<Image>,
    loaded: Option<Loaded>,
    busy: Option<SharedString>,
    status: SharedString,
    show_original: bool,
}

struct Loaded {
    path: PathBuf,
    doc: Arc<Document>,
    pages: Vec<PageView>,
    elapsed: Duration,
}

struct PageView {
    number: u32,
    id: ObjectId,
    detected: f32,
    angle: f32,
    skip: bool,
    note: Option<&'static str>,
    /// Preview in display orientation (page /Rotate applied).
    preview: Option<GrayImage>,
    original: Option<Arc<RenderImage>>,
    aligned: Option<Arc<RenderImage>>,
}

impl PageView {
    fn correction(&self) -> f32 {
        if self.skip { 0.0 } else { self.angle }
    }

    fn is_crooked(&self) -> bool {
        self.correction().abs() >= pdf::MIN_CORRECTION
    }
}

impl Munch {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Munch {
            focus: cx.focus_handle(),
            mascot: Arc::new(Image::from_bytes(ImageFormat::Svg, MASCOT.to_vec())),
            portrait: Kind::Munch.portrait(),
            loaded: None,
            busy: None,
            status: "Feed me a crooked scan.".into(),
            show_original: false,
        }
    }

    pub fn load(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let name = file_name(&path);
        self.busy = Some(format!("Munching {name}…").into());
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let start = Instant::now();
                    let doc = pdf::open(&path)?;
                    let pages = pdf::analyze(&doc)
                        .into_par_iter()
                        .map(page_view)
                        .collect::<Vec<_>>();
                    anyhow::Ok(Loaded {
                        path,
                        doc: Arc::new(doc),
                        pages,
                        elapsed: start.elapsed(),
                    })
                })
                .await;

            this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(loaded) => {
                        if let Some(old) = this.loaded.replace(loaded) {
                            for page in old.pages {
                                drop_images(&page, cx);
                            }
                        }
                        this.status = this.summary();
                    }
                    Err(err) => this.status = format!("Blergh, couldn't eat that: {err:#}").into(),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn summary(&self) -> SharedString {
        let Some(loaded) = &self.loaded else {
            return "Feed me a crooked scan.".into();
        };
        let crooked = loaded.pages.iter().filter(|p| p.is_crooked()).count();
        let total = loaded.pages.len();
        format!(
            "{total} page{} · {crooked} crooked · analysed in {} ms",
            if total == 1 { "" } else { "s" },
            loaded.elapsed.as_millis()
        )
        .into()
    }

    fn open(&mut self, _: &Open, _: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Straighten".into()),
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
        if self.busy.is_some() {
            return;
        }
        let suggested = pdf::default_output(&loaded.path);
        let dir = suggested.parent().unwrap_or(Path::new(".")).to_path_buf();
        let target = cx.prompt_for_new_path(&dir, Some(&file_name(&suggested)));
        let doc = loaded.doc.clone();
        let corrections: Vec<(ObjectId, f32)> =
            loaded.pages.iter().map(|p| (p.id, p.correction())).collect();

        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(out))) = target.await else { return };
            this.update(cx, |this, cx| {
                this.busy = Some("Straightening…".into());
                cx.notify();
            })
            .ok();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let start = Instant::now();
                    let changed = pdf::save_straightened((*doc).clone(), &corrections, &out)?;
                    anyhow::Ok((changed, out, start.elapsed()))
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = None;
                this.status = match result {
                    Ok((changed, out, took)) => format!(
                        "Straightened {changed} page{} → {} in {} ms. Nom.",
                        if changed == 1 { "" } else { "s" },
                        file_name(&out),
                        took.as_millis()
                    )
                    .into(),
                    Err(err) => format!("Couldn't save: {err:#}").into(),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn toggle_original(&mut self, _: &ToggleOriginal, _: &mut Window, cx: &mut Context<Self>) {
        self.show_original = !self.show_original;
        cx.notify();
    }

    fn set_angle(&mut self, ix: usize, angle: f32, cx: &mut Context<Self>) {
        let Some(page) = self.loaded.as_mut().and_then(|l| l.pages.get_mut(ix)) else {
            return;
        };
        page.angle = (angle * 100.0).round() / 100.0;
        if let Some(preview) = &page.preview {
            let fresh = render_image(&deskew::straighten(preview, page.angle));
            if let Some(old) = page.aligned.replace(fresh) {
                cx.drop_image(old, None);
            }
        }
        self.status = self.summary();
        cx.notify();
    }

    fn toggle_skip(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(page) = self.loaded.as_mut().and_then(|l| l.pages.get_mut(ix)) {
            page.skip = !page.skip;
        }
        self.status = self.summary();
        cx.notify();
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self
            .loaded
            .as_ref()
            .map(|l| file_name(&l.path))
            .unwrap_or_default();
        let mut buttons = vec![
            button("open", "Open…", false)
                .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.open(&Open, w, cx)))
                .into_any_element(),
        ];
        if self.loaded.is_some() {
            buttons.push(
                button(
                    "toggle",
                    if self.show_original { "Showing: before" } else { "Showing: after" },
                    false,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, w, cx| {
                    this.toggle_original(&ToggleOriginal, w, cx)
                }))
                .into_any_element(),
            );
            buttons.push(
                button("save", "Save aligned", true)
                    .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.save(&Save, w, cx)))
                    .into_any_element(),
            );
        }
        toolbar(self.portrait.clone(), Kind::Munch, title, buttons)
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let modifier = if cfg!(target_os = "macos") { "⌘" } else { "Ctrl+" };
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_4()
            .child(img(self.mascot.clone()).w(px(200.)).h(px(188.)))
            .child(
                div()
                    .text_2xl()
                    .font_weight(FontWeight::BOLD)
                    .child("Feed me crooked scans"),
            )
            .child(
                div()
                    .text_color(rgb(theme::MUTED))
                    .child(format!("Drop a PDF anywhere, or press {modifier}O.")),
            )
            .child(
                button("open-big", "Open a PDF…", true)
                    .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.open(&Open, w, cx))),
            )
    }

    fn render_card(&self, ix: usize, page: &PageView, cx: &mut Context<Self>) -> gpui::AnyElement {
        let image = if self.show_original { &page.original } else { &page.aligned };
        let aspect = page
            .preview
            .as_ref()
            .map(|p| p.height() as f32 / p.width() as f32)
            .unwrap_or(1.414);

        let (badge, badge_color): (SharedString, u32) = if let Some(note) = page.note {
            (note.into(), theme::MUTED)
        } else if page.skip {
            ("left as is".into(), theme::MUTED)
        } else if page.is_crooked() {
            (format!("↻ {:+.2}°", page.angle).into(), theme::YELLOW)
        } else {
            ("straight ✓".into(), theme::MINT)
        };
        let border: Hsla = if page.is_crooked() {
            rgb(theme::ACCENT).into()
        } else {
            gpui::transparent_black()
        };

        let preview = div()
            .id(("preview", ix))
            .w(px(CARD_WIDTH))
            .h(px(CARD_WIDTH * aspect))
            .rounded_md()
            .overflow_hidden()
            .bg(gpui::white())
            .cursor_pointer()
            .when(page.skip, |d| d.opacity(0.45))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_skip(ix, cx)))
            .when_some(image.clone(), |d, image| d.child(img(image).size_full()));

        let angle = page.angle;
        let detected = page.detected;
        let controls = div()
            .flex()
            .items_center()
            .gap_1()
            .when(page.preview.is_some(), |row| {
                row.child(
                    small_button(("minus", ix), "−")
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.set_angle(ix, angle - NUDGE, cx)
                        })),
                )
                .child(
                    small_button(("plus", ix), "+")
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.set_angle(ix, angle + NUDGE, cx)
                        })),
                )
                .when((angle - detected).abs() > 0.001, |row| {
                    row.child(small_button(("reset", ix), "↺").on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| this.set_angle(ix, detected, cx),
                    )))
                })
            });

        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .rounded_lg()
            .bg(rgb(theme::CARD))
            .border_2()
            .border_color(border)
            .child(preview)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_sm()
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(div().text_color(rgb(theme::MUTED)).child(format!("p. {}", page.number)))
                            .child(div().text_color(rgb(badge_color)).child(badge)),
                    )
                    .child(controls),
            )
            .into_any_element()
    }
}

impl Render for Munch {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.loaded {
            None => self.render_empty(cx).into_any_element(),
            Some(loaded) => {
                let cards: Vec<_> = loaded
                    .pages
                    .iter()
                    .enumerate()
                    .map(|(ix, page)| self.render_card(ix, page, cx))
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

        let status = self.busy.clone().unwrap_or_else(|| self.status.clone());
        let hint = self
            .loaded
            .is_some()
            .then_some("click a page to leave it alone · space: before/after · esc: all monsters");

        div()
            .key_context("Munch")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::open))
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::toggle_original))
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
            .child(self.render_toolbar(cx))
            .child(body)
            .child(status_bar(status, self.busy.is_some(), hint))
    }
}

fn page_view(page: pdf::Page) -> PageView {
    let preview = page.preview.as_ref().map(|p| match page.rotate {
        90 => imageops::rotate90(p),
        180 => imageops::rotate180(p),
        270 => imageops::rotate270(p),
        _ => p.clone(),
    });
    let angle = page.suggested_angle();
    PageView {
        number: page.number,
        id: page.id,
        detected: angle,
        angle,
        skip: false,
        note: page.note,
        original: preview.as_ref().map(render_image),
        aligned: preview.as_ref().map(|p| render_image(&deskew::straighten(p, angle))),
        preview,
    }
}

fn drop_images(page: &PageView, cx: &mut gpui::App) {
    for image in [&page.original, &page.aligned].into_iter().flatten() {
        cx.drop_image(image.clone(), None);
    }
}
