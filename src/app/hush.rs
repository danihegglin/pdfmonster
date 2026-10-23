//! Hush's screen: draw boxes over what must go (or search for it), wipe the
//! metadata, save.

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::Instant,
};

use gpui::{
    Bounds, ClickEvent, Context, ExternalPaths, FocusHandle, FontWeight, Image, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathPromptOptions, Pixels, Point,
    RenderImage, SharedString, Window, canvas, div, img, prelude::*, px, rgb,
};
use lopdf::Document;

use super::{
    Open, Save, button, empty_state, file_name, is_pdf, pack::Kind, plural, render_rgba,
    status_bar, theme, toolbar,
};
use crate::{
    content::Matrix,
    hush::{self, Area},
    render,
};

const PAGE_WIDTH: f32 = 300.0;

pub struct Hush {
    pub focus: FocusHandle,
    search_focus: FocusHandle,
    portrait: Arc<Image>,
    loaded: Option<Loaded>,
    areas: Vec<Area>,
    /// A box being drawn: page index, start and current point in window coordinates.
    drawing: Option<(usize, Point<Pixels>, Point<Pixels>)>,
    /// Where each page preview ended up on screen, filled in while painting.
    page_bounds: Rc<RefCell<Vec<Option<Bounds<Pixels>>>>>,
    search: String,
    wipe_metadata: bool,
    busy: Option<SharedString>,
    status: SharedString,
}

struct Loaded {
    path: PathBuf,
    doc: Arc<Document>,
    pages: Vec<PageView>,
    metadata: Vec<String>,
}

struct PageView {
    image: Option<Arc<RenderImage>>,
    /// Page coordinates → rendering at scale 1, and that rendering's size.
    geometry: Option<(Matrix, (f32, f32))>,
}

impl PageView {
    fn aspect(&self) -> f32 {
        self.geometry.map_or(1.414, |(_, (w, h))| h / w)
    }

    /// Page coordinates → pixels inside the preview.
    fn to_preview(&self) -> Option<Matrix> {
        let (m, (w, _)) = self.geometry?;
        let s = (PAGE_WIDTH / w) as f64;
        Some([m[0] * s, m[1] * s, m[2] * s, m[3] * s, m[4] * s, m[5] * s])
    }
}

impl Hush {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Hush {
            focus: cx.focus_handle(),
            search_focus: cx.focus_handle(),
            portrait: Kind::Hush.portrait(),
            loaded: None,
            areas: Vec::new(),
            drawing: None,
            page_bounds: Rc::default(),
            search: String::new(),
            wipe_metadata: true,
            busy: None,
            status: "Feed me a PDF with secrets in it.".into(),
        }
    }

    pub fn load(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.busy = Some(format!("Reading {} very discreetly…", file_name(&path)).into());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let bytes = Arc::new(
                        std::fs::read(&path).map_err(|e| anyhow::anyhow!("could not open {}: {e}", path.display()))?,
                    );
                    let doc = Document::load_mem(&bytes)?;
                    let geometry = render::page_geometry(bytes.clone());
                    let thumbs = render::thumbnails(bytes, PAGE_WIDTH * 2.0 * 1.42);
                    let pages = (0..doc.get_pages().len())
                        .map(|ix| PageView {
                            image: thumbs.get(ix).and_then(Option::as_ref).map(render_rgba),
                            geometry: geometry.get(ix).copied().flatten(),
                        })
                        .collect();
                    let metadata = hush::describe_metadata(&doc);
                    anyhow::Ok(Loaded { path, doc: Arc::new(doc), pages, metadata })
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
                        this.areas.clear();
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
        let Some(loaded) = &self.loaded else { return "Feed me a PDF with secrets in it.".into() };
        let boxes = self.areas.len();
        let meta = if self.wipe_metadata && !loaded.metadata.is_empty() { " · metadata will be wiped" } else { "" };
        match boxes {
            0 => format!("Drag over anything that should disappear, or search for it{meta}").into(),
            n => format!("{n} box{} to black out{meta}", if n == 1 { "" } else { "es" }).into(),
        }
    }

    fn open(&mut self, _: &Open, _: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Hush".into()),
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

    fn find(&mut self, cx: &mut Context<Self>) {
        let Some(loaded) = &self.loaded else { return };
        let needle = self.search.trim().to_string();
        if needle.is_empty() {
            return;
        }
        let found = hush::find_text(&loaded.doc, &needle);
        self.status = match found.len() {
            0 => format!("No \"{needle}\" in here. Scanned pages need Peek first.").into(),
            n => format!("Boxed {n} × \"{needle}\"").into(),
        };
        self.areas.extend(found);
        self.search.clear();
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if key.modifiers.platform || key.modifiers.control {
            return;
        }
        match key.key.as_str() {
            "enter" => self.find(cx),
            "backspace" => {
                self.search.pop();
            }
            _ => match &key.key_char {
                Some(c) => self.search.push_str(c),
                None => return,
            },
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Turns a box drawn on screen into a box on the page.
    fn finish_drawing(&mut self, cx: &mut Context<Self>) {
        let Some((ix, a, b)) = self.drawing.take() else { return };
        let bounds = self.page_bounds.borrow().get(ix).copied().flatten();
        let Some(loaded) = &self.loaded else { return };
        let (Some(bounds), Some(m)) = (bounds, loaded.pages[ix].to_preview()) else { return };
        let Some(inverse) = hush::invert(&m) else { return };
        let local = |p: Point<Pixels>| {
            (f32::from(p.x - bounds.origin.x) as f64, f32::from(p.y - bounds.origin.y) as f64)
        };
        let ((x0, y0), (x1, y1)) = (local(a), local(b));
        if (x1 - x0).abs() < 3.0 || (y1 - y0).abs() < 3.0 {
            cx.notify();
            return;
        }
        let rect = hush::transform_rect(&[x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)], &inverse);
        self.areas.push(Area { page: ix as u32 + 1, rect });
        self.status = self.summary();
        cx.notify();
    }

    fn save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        let Some(loaded) = &self.loaded else { return };
        if self.busy.is_some() || (self.areas.is_empty() && !self.wipe_metadata) {
            return;
        }
        let suggested = hush::default_output(&loaded.path);
        let dir = suggested.parent().unwrap_or(Path::new(".")).to_path_buf();
        let target = cx.prompt_for_new_path(&dir, Some(&file_name(&suggested)));
        let doc = loaded.doc.clone();
        let areas = self.areas.clone();
        let wipe = self.wipe_metadata;
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(out))) = target.await else { return };
            this.update(cx, |this, cx| {
                this.busy = Some("Hushing…".into());
                cx.notify();
            })
            .ok();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let start = Instant::now();
                    let mut doc = (*doc).clone();
                    let report = hush::redact(&mut doc, &areas)?;
                    let wiped = if wipe { hush::strip_metadata(&mut doc) } else { 0 };
                    doc.save(&out).map_err(|e| anyhow::anyhow!("could not write {}: {e}", out.display()))?;
                    let mut message = format!(
                        "Hushed → {} in {} ms: {} glyph{}, {} image{} blacked out",
                        file_name(&out),
                        start.elapsed().as_millis(),
                        report.glyphs,
                        plural(report.glyphs),
                        report.images,
                        plural(report.images),
                    );
                    if wiped > 0 {
                        message.push_str(", metadata wiped");
                    }
                    if !report.removed_whole.is_empty() {
                        message.push_str(&format!(
                            ". Removed whole because I couldn't cut precisely: {}",
                            report.removed_whole.join("; ")
                        ));
                    }
                    anyhow::Ok(message)
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = None;
                this.status = match result {
                    Ok(message) => message.into(),
                    Err(err) => format!("Couldn't save: {err:#}").into(),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_page(&self, ix: usize, page: &PageView, cx: &mut Context<Self>) -> gpui::AnyElement {
        let height = PAGE_WIDTH * page.aspect();
        let to_preview = page.to_preview();
        let boxes: Vec<_> = self
            .areas
            .iter()
            .enumerate()
            .filter(|(_, a)| a.page as usize == ix + 1)
            .filter_map(|(area_ix, a)| {
                let r = hush::transform_rect(&a.rect, &to_preview?);
                Some(
                    div()
                        .id(("box", area_ix))
                        .absolute()
                        .left(px(r[0] as f32))
                        .top(px(r[1] as f32))
                        .w(px((r[2] - r[0]) as f32))
                        .h(px((r[3] - r[1]) as f32))
                        .bg(gpui::black())
                        .opacity(0.85)
                        .cursor_pointer()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.areas.remove(area_ix);
                            this.status = this.summary();
                            cx.notify();
                        })),
                )
            })
            .collect();
        let drawing = self.drawing.filter(|d| d.0 == ix).and_then(|(_, a, b)| {
            let bounds = self.page_bounds.borrow().get(ix).copied().flatten()?;
            let (x0, y0) = (a.x.min(b.x) - bounds.origin.x, a.y.min(b.y) - bounds.origin.y);
            Some(
                div()
                    .absolute()
                    .left(x0)
                    .top(y0)
                    .w((a.x - b.x).abs())
                    .h((a.y - b.y).abs())
                    .border_2()
                    .border_color(rgb(Kind::Hush.species().color))
                    .bg(gpui::black())
                    .opacity(0.5),
            )
        });
        let bounds_store = self.page_bounds.clone();

        div()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_lg()
            .bg(rgb(theme::CARD))
            .child(
                div()
                    .id(("page", ix))
                    .relative()
                    .w(px(PAGE_WIDTH))
                    .h(px(height))
                    .bg(gpui::white())
                    .cursor_crosshair()
                    .when_some(page.image.clone(), |d, image| d.child(img(image).size_full()))
                    .child(
                        canvas(
                            move |bounds, _, _| {
                                let mut store = bounds_store.borrow_mut();
                                if store.len() <= ix {
                                    store.resize(ix + 1, None);
                                }
                                store[ix] = Some(bounds);
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    )
                    .children(boxes)
                    .children(drawing)
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                        this.drawing = Some((ix, e.position, e.position));
                        cx.notify();
                    }))
                    .on_mouse_move(cx.listener(move |this, e: &MouseMoveEvent, _, cx| {
                        if let Some(d) = &mut this.drawing
                            && d.0 == ix
                        {
                            d.2 = e.position;
                            cx.notify();
                        }
                    }))
                    .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, cx| {
                        this.finish_drawing(cx)
                    })),
            )
            .child(div().text_sm().text_color(rgb(theme::MUTED)).child(format!("p. {}", ix + 1)))
            .into_any_element()
    }

    fn render_sidebar(&self, loaded: &Loaded, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.search_focus.is_focused(window);
        let color = Kind::Hush.species().color;
        let search = div()
            .id("search")
            .track_focus(&self.search_focus)
            .on_key_down(cx.listener(Self::key_down))
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(theme::BG))
            .border_1()
            .border_color(rgb(if focused { theme::ACCENT } else { theme::BORDER }))
            .cursor_text()
            .min_h(px(26.))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                window.focus(&this.search_focus);
                cx.notify();
            }))
            .child(if self.search.is_empty() && !focused {
                div().text_color(rgb(theme::MUTED)).child("a name, an IBAN, …")
            } else {
                div().child(format!("{}{}", self.search, if focused { "▏" } else { "" }))
            });

        let metadata: Vec<_> = loaded
            .metadata
            .iter()
            .map(|m| div().text_xs().text_color(rgb(theme::MUTED)).child(format!("• {m}")))
            .collect();
        let no_metadata = loaded.metadata.is_empty();

        div()
            .w(px(270.))
            .flex_none()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .bg(rgb(theme::PANEL))
            .border_l_1()
            .border_color(rgb(theme::BORDER))
            .child(div().font_weight(FontWeight::BOLD).child("Black out text"))
            .child(search)
            .child(
                button("find", "Find & box it", false)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.find(cx))),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(theme::MUTED))
                    .child("Or drag over anything on a page. Click a box to remove it."),
            )
            .when(!self.areas.is_empty(), |d| {
                d.child(
                    button("clear", "Remove all boxes", false).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.areas.clear();
                        this.status = this.summary();
                        cx.notify();
                    })),
                )
            })
            .child(div().mt_4().font_weight(FontWeight::BOLD).child("Hidden metadata"))
            .child(if no_metadata {
                div().text_xs().text_color(rgb(theme::MINT)).child("None found. Nothing to wipe.").into_any_element()
            } else {
                div().flex().flex_col().gap_1().children(metadata).into_any_element()
            })
            .when(!no_metadata, |d| {
                d.child(
                    div()
                        .id("wipe")
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.wipe_metadata = !this.wipe_metadata;
                            this.status = this.summary();
                            cx.notify();
                        }))
                        .child(
                            div()
                                .size(px(16.))
                                .rounded_sm()
                                .border_2()
                                .border_color(rgb(color))
                                .when(self.wipe_metadata, |d| d.bg(rgb(color))),
                        )
                        .child("Wipe it when saving"),
                )
            })
    }
}

impl Render for Hush {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.loaded {
            None => empty_state(
                self.portrait.clone(),
                "Feed me a PDF with secrets in it",
                "I'll black out the sensitive bits for real, and wipe the hidden metadata.",
                button("open-big", "Open a PDF…", true)
                    .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.open(&Open, w, cx))),
            )
            .into_any_element(),
            Some(loaded) => {
                let pages: Vec<_> = loaded
                    .pages
                    .iter()
                    .enumerate()
                    .map(|(ix, page)| self.render_page(ix, page, cx))
                    .collect();
                div()
                    .flex_1()
                    .flex()
                    .min_h_0()
                    .child(
                        div()
                            .id("pages")
                            .flex_1()
                            .overflow_y_scroll()
                            .p_4()
                            .child(div().flex().flex_wrap().gap_4().children(pages)),
                    )
                    .child(self.render_sidebar(loaded, window, cx))
                    .into_any_element()
            }
        };

        let title = self.loaded.as_ref().map(|l| file_name(&l.path)).unwrap_or_default();
        let mut buttons = vec![
            button("open", "Open…", false)
                .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.open(&Open, w, cx)))
                .into_any_element(),
        ];
        if self.loaded.is_some() && (!self.areas.is_empty() || self.wipe_metadata) {
            buttons.push(
                button("save", "Save hushed", true)
                    .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.save(&Save, w, cx)))
                    .into_any_element(),
            );
        }
        let status = self.busy.clone().unwrap_or_else(|| self.status.clone());

        div()
            .key_context("Hush")
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
            // Finish a box even if the mouse is let go outside the page.
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, cx| this.finish_drawing(cx)))
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar(self.portrait.clone(), Kind::Hush, title, buttons))
            .child(body)
            .child(status_bar(status, self.busy.is_some(), Some("esc: all monsters")))
    }
}
