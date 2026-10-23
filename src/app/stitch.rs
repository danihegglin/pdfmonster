//! Stitch's screen: drop in a pile of PDFs, put them in order, save one.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use gpui::{
    ClickEvent, Context, ExternalPaths, FocusHandle, FontWeight, Image, PathPromptOptions,
    SharedString, Window, div, prelude::*, px, rgb,
};
use lopdf::Document;

use super::{
    Open, Save, button, empty_state, file_name, is_pdf, pack::Kind, small_button, status_bar,
    theme, toolbar,
};
use crate::{pdf, stitch};

pub struct Stitch {
    pub focus: FocusHandle,
    portrait: Arc<Image>,
    pieces: Vec<Piece>,
    busy: Option<SharedString>,
    status: SharedString,
}

struct Piece {
    path: PathBuf,
    doc: Arc<Document>,
    pages: usize,
}

/// What's being dragged while reordering the list.
#[derive(Clone)]
struct DraggedPiece {
    ix: usize,
    name: SharedString,
}

impl Render for DraggedPiece {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .rounded_md()
            .bg(rgb(theme::ACCENT))
            .text_color(rgb(0xffffff))
            .text_sm()
            .child(self.name.clone())
    }
}

impl Stitch {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Stitch {
            focus: cx.focus_handle(),
            portrait: Kind::Stitch.portrait(),
            pieces: Vec::new(),
            busy: None,
            status: "Feed me a pile of PDFs.".into(),
        }
    }

    /// Adds the files to the end of the pile.
    pub fn add(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = paths.into_iter().filter(|p| is_pdf(p)).collect();
        if paths.is_empty() {
            self.status = "Only PDFs, please. I'm a picky eater.".into();
            cx.notify();
            return;
        }
        self.busy = Some(format!("Reading {} file{}…", paths.len(), plural(paths.len())).into());
        cx.notify();

        cx.spawn(async move |this, cx| {
            let results: Vec<(PathBuf, anyhow::Result<Document>)> = cx
                .background_executor()
                .spawn(async move { paths.into_iter().map(|p| { let doc = pdf::open(&p); (p, doc) }).collect() })
                .await;
            this.update(cx, |this, cx| {
                this.busy = None;
                let mut failed = Vec::new();
                for (path, doc) in results {
                    match doc {
                        Ok(doc) => this.pieces.push(Piece {
                            pages: doc.get_pages().len(),
                            doc: Arc::new(doc),
                            path,
                        }),
                        Err(_) => failed.push(file_name(&path)),
                    }
                }
                this.status = if failed.is_empty() {
                    this.summary()
                } else {
                    format!("Blergh, couldn't read {}", failed.join(", ")).into()
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn summary(&self) -> SharedString {
        let pages: usize = self.pieces.iter().map(|p| p.pages).sum();
        match self.pieces.len() {
            0 => "Feed me a pile of PDFs.".into(),
            1 => format!("1 file · {pages} pages · add at least one more").into(),
            n => format!("{n} files · {pages} pages · drag to reorder").into(),
        }
    }

    fn open(&mut self, _: &Open, _: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Add".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                this.update(cx, |this, cx| this.add(paths, cx)).ok();
            }
        })
        .detach();
    }

    fn save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        if self.pieces.len() < 2 || self.busy.is_some() {
            return;
        }
        let suggested = stitch::default_output(&self.pieces[0].path);
        let dir = suggested.parent().unwrap_or(Path::new(".")).to_path_buf();
        let target = cx.prompt_for_new_path(&dir, Some(&file_name(&suggested)));
        let docs: Vec<Arc<Document>> = self.pieces.iter().map(|p| p.doc.clone()).collect();

        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(out))) = target.await else { return };
            this.update(cx, |this, cx| {
                this.busy = Some("Stitching…".into());
                cx.notify();
            })
            .ok();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let start = Instant::now();
                    let mut merged = stitch::merge(docs.iter().map(|d| (**d).clone()))?;
                    let pages = merged.get_pages().len();
                    merged
                        .save(&out)
                        .map_err(|e| anyhow::anyhow!("could not write {}: {e}", out.display()))?;
                    anyhow::Ok((pages, out, start.elapsed()))
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = None;
                this.status = match result {
                    Ok((pages, out, took)) => format!(
                        "Stitched {pages} pages → {} in {} ms. Neat seams.",
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

    fn move_piece(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if from >= self.pieces.len() || to >= self.pieces.len() || from == to {
            return;
        }
        let piece = self.pieces.remove(from);
        self.pieces.insert(to, piece);
        cx.notify();
    }

    fn remove_piece(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.pieces.len() {
            self.pieces.remove(ix);
        }
        self.status = self.summary();
        cx.notify();
    }

    fn render_row(&self, ix: usize, piece: &Piece, cx: &mut Context<Self>) -> gpui::AnyElement {
        let name: SharedString = file_name(&piece.path).into();
        let last = self.pieces.len() - 1;
        div()
            .id(("piece", ix))
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .rounded_lg()
            .bg(rgb(theme::CARD))
            .border_2()
            .border_color(gpui::transparent_black())
            .cursor_grab()
            .on_drag(DraggedPiece { ix, name: name.clone() }, |dragged, _, _, cx| {
                cx.new(|_| dragged.clone())
            })
            .drag_over::<DraggedPiece>(|style, _, _, _| style.border_color(rgb(theme::ACCENT)))
            .on_drop(cx.listener(move |this, dragged: &DraggedPiece, _, cx| {
                this.move_piece(dragged.ix, ix, cx)
            }))
            .child(
                div()
                    .w(px(28.))
                    .text_color(rgb(theme::MUTED))
                    .font_weight(FontWeight::BOLD)
                    .child(format!("{}.", ix + 1)),
            )
            .child(div().flex_1().truncate().child(name))
            .child(
                div()
                    .text_color(rgb(theme::MUTED))
                    .child(format!("{} page{}", piece.pages, plural(piece.pages))),
            )
            .child(
                div()
                    .flex()
                    .gap_1()
                    .when(ix > 0, |d| {
                        d.child(small_button(("up", ix), "↑").on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| this.move_piece(ix, ix - 1, cx),
                        )))
                    })
                    .when(ix < last, |d| {
                        d.child(small_button(("down", ix), "↓").on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| this.move_piece(ix, ix + 1, cx),
                        )))
                    })
                    .child(small_button(("remove", ix), "✕").on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| this.remove_piece(ix, cx),
                    ))),
            )
            .into_any_element()
    }
}

impl Render for Stitch {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = if self.pieces.is_empty() {
            empty_state(
                self.portrait.clone(),
                "Feed me a pile of PDFs",
                "Drop them anywhere, then drag them into order.",
                button("add-big", "Add PDFs…", true)
                    .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.open(&Open, w, cx))),
            )
            .into_any_element()
        } else {
            let rows: Vec<_> = self
                .pieces
                .iter()
                .enumerate()
                .map(|(ix, piece)| self.render_row(ix, piece, cx))
                .collect();
            div()
                .id("pieces")
                .flex_1()
                .overflow_y_scroll()
                .p_4()
                .child(div().max_w(px(720.)).mx_auto().flex().flex_col().gap_2().children(rows))
                .into_any_element()
        };

        let mut buttons = vec![
            button("add", "Add…", false)
                .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.open(&Open, w, cx)))
                .into_any_element(),
        ];
        if self.pieces.len() >= 2 {
            buttons.push(
                button("save", "Stitch & save", true)
                    .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.save(&Save, w, cx)))
                    .into_any_element(),
            );
        }
        let status = self.busy.clone().unwrap_or_else(|| self.status.clone());

        div()
            .key_context("Stitch")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::open))
            .on_action(cx.listener(Self::save))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.add(paths.paths().to_vec(), cx)
            }))
            .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(rgb(0x221c33)))
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar(self.portrait.clone(), Kind::Stitch, String::new(), buttons))
            .child(body)
            .child(status_bar(status, self.busy.is_some(), Some("esc: all monsters")))
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
