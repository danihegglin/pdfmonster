//! The gpui front end: drop a PDF in, eyeball the fixes, save.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    App, Application, Bounds, ClickEvent, Context, ExternalPaths, FocusHandle, FontWeight, Hsla,
    Image, ImageFormat, KeyBinding, Menu, MenuItem, PathPromptOptions, RenderImage, SharedString,
    Stateful, TitlebarOptions, Window, WindowBounds, WindowOptions, actions, div, img, prelude::*,
    px, rgb, size,
};
use image::{Frame, GrayImage, RgbaImage, imageops};
use lopdf::{Document, ObjectId};
use rayon::prelude::*;

use crate::{deskew, pdf};

actions!(pdfmonster, [Open, Save, ToggleOriginal, Quit]);

const MASCOT: &[u8] = include_bytes!("../assets/monster.svg");
const CARD_WIDTH: f32 = 200.0;
const NUDGE: f32 = 0.1;

mod theme {
    pub const BG: u32 = 0x16131e;
    pub const PANEL: u32 = 0x1f1a2b;
    pub const CARD: u32 = 0x282236;
    pub const CARD_HOVER: u32 = 0x322a44;
    pub const BORDER: u32 = 0x3a3150;
    pub const ACCENT: u32 = 0x7c6cf2;
    pub const ACCENT_HOVER: u32 = 0x9185f6;
    pub const YELLOW: u32 = 0xffd166;
    pub const MINT: u32 = 0x6ee7b7;
    pub const PINK: u32 = 0xff8fab;
    pub const TEXT: u32 = 0xece8f6;
    pub const MUTED: u32 = 0x9a93ad;
}

pub fn run(initial: Option<PathBuf>) {
    Application::new().run(move |cx: &mut App| {
        cx.activate(true);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([
            KeyBinding::new("secondary-o", Open, None),
            KeyBinding::new("secondary-s", Save, None),
            KeyBinding::new("space", ToggleOriginal, None),
            KeyBinding::new("secondary-q", Quit, None),
        ]);
        cx.set_menus(vec![
            Menu {
                name: "pdfmonster".into(),
                items: vec![MenuItem::action("Quit pdfmonster", Quit)],
            },
            Menu {
                name: "File".into(),
                items: vec![
                    MenuItem::action("Open…", Open),
                    MenuItem::action("Save Aligned…", Save),
                ],
            },
            Menu {
                name: "View".into(),
                items: vec![MenuItem::action("Toggle Before / After", ToggleOriginal)],
            },
        ]);
        cx.on_window_closed(|cx| cx.quit()).detach();

        let bounds = Bounds::centered(None, size(px(1120.), px(780.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("pdfmonster".into()),
                ..Default::default()
            }),
            app_id: Some("pdfmonster".into()),
            ..Default::default()
        };
        cx.open_window(options, |window, cx| {
            cx.new(|cx| {
                let focus = cx.focus_handle();
                window.focus(&focus);
                let mut monster = Monster {
                    focus,
                    mascot: Arc::new(Image::from_bytes(ImageFormat::Svg, MASCOT.to_vec())),
                    loaded: None,
                    busy: None,
                    status: "Feed me a crooked scan.".into(),
                    show_original: false,
                };
                if let Some(path) = initial {
                    monster.load(path, cx);
                }
                monster
            })
        })
        .expect("failed to open window");
    });
}

struct Monster {
    focus: FocusHandle,
    mascot: Arc<Image>,
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

impl Monster {
    fn load(&mut self, path: PathBuf, cx: &mut Context<Self>) {
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
        let has_doc = self.loaded.is_some();

        div()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_2()
            .bg(rgb(theme::PANEL))
            .border_b_1()
            .border_color(rgb(theme::BORDER))
            .child(img(self.mascot.clone()).size(px(34.)))
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::BOLD)
                    .child("pdfmonster"),
            )
            .child(div().text_color(rgb(theme::MUTED)).truncate().child(title))
            .child(div().flex_1())
            .child(
                button("open", "Open…", false)
                    .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.open(&Open, w, cx))),
            )
            .when(has_doc, |bar| {
                bar.child(
                    button(
                        "toggle",
                        if self.show_original { "Showing: before" } else { "Showing: after" },
                        false,
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, w, cx| {
                        this.toggle_original(&ToggleOriginal, w, cx)
                    })),
                )
                .child(
                    button("save", "Save aligned", true)
                        .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.save(&Save, w, cx))),
                )
            })
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

impl Render for Monster {
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
        let status_color = if self.busy.is_some() { theme::YELLOW } else { theme::MUTED };

        div()
            .key_context("Monster")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::open))
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::toggle_original))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                let pdf = paths
                    .paths()
                    .iter()
                    .find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")));
                match pdf {
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
            .bg(rgb(theme::BG))
            .text_color(rgb(theme::TEXT))
            .text_sm()
            .child(self.render_toolbar(cx))
            .child(body)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_1()
                    .bg(rgb(theme::PANEL))
                    .border_t_1()
                    .border_color(rgb(theme::BORDER))
                    .text_xs()
                    .child(div().size(px(8.)).rounded_full().bg(rgb(if self.busy.is_some() {
                        theme::YELLOW
                    } else {
                        theme::PINK
                    })))
                    .child(div().text_color(rgb(status_color)).child(status))
                    .child(div().flex_1())
                    .when(self.loaded.is_some(), |d| {
                        d.child(
                            div()
                                .text_color(rgb(theme::MUTED))
                                .child("click a page to leave it alone · space: before/after"),
                        )
                    }),
            )
    }
}

fn button(id: &'static str, label: impl Into<SharedString>, primary: bool) -> Stateful<gpui::Div> {
    let (bg, hover, fg) = if primary {
        (theme::ACCENT, theme::ACCENT_HOVER, 0xffffff)
    } else {
        (theme::CARD, theme::CARD_HOVER, theme::TEXT)
    };
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .bg(rgb(bg))
        .text_color(rgb(fg))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(hover)))
        .child(label.into())
}

fn small_button(id: (&'static str, usize), label: &'static str) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .w(px(22.))
        .h(px(22.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .bg(rgb(theme::CARD_HOVER))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(theme::ACCENT)))
        .child(label)
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

fn render_image(gray: &GrayImage) -> Arc<RenderImage> {
    // Gray pixels are identical in RGBA and BGRA, so no channel swizzle is needed.
    let rgba = RgbaImage::from_fn(gray.width(), gray.height(), |x, y| {
        let v = gray.get_pixel(x, y).0[0];
        image::Rgba([v, v, v, 255])
    });
    Arc::new(RenderImage::new(vec![Frame::new(rgba)]))
}

fn drop_images(page: &PageView, cx: &mut App) {
    for image in [&page.original, &page.aligned].into_iter().flatten() {
        cx.drop_image(image.clone(), None);
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}
