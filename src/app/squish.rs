//! Squish's screen: drop in a PDF, see how small each level gets it, save.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use gpui::{
    ClickEvent, Context, ExternalPaths, FocusHandle, FontWeight, Image, PathPromptOptions,
    SharedString, Window, div, img, prelude::*, px, rgb,
};
use rayon::prelude::*;

use super::{Open, Save, button, empty_state, file_name, is_pdf, pack::Kind, status_bar, theme, toolbar};
use crate::{human_size, pdf, squish};

pub struct Squish {
    pub focus: FocusHandle,
    portrait: Arc<Image>,
    loaded: Option<Loaded>,
    level: squish::Level,
    busy: Option<SharedString>,
    status: SharedString,
}

struct Loaded {
    path: PathBuf,
    original: u64,
    /// The squished file for each level, ready to write.
    results: Vec<(squish::Level, Arc<Vec<u8>>)>,
}

impl Squish {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Squish {
            focus: cx.focus_handle(),
            portrait: Kind::Squish.portrait(),
            loaded: None,
            level: squish::Level::default(),
            busy: None,
            status: "Feed me a bloated PDF.".into(),
        }
    }

    pub fn load(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.busy = Some(format!("Squeezing {}…", file_name(&path)).into());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let original = std::fs::metadata(&path)?.len();
                    let doc = pdf::open(&path)?;
                    // Try every level at once; it's quick, and lets you pick by size.
                    let results = squish::Level::ALL
                        .par_iter()
                        .map(|&level| {
                            let mut doc = doc.clone();
                            squish::squish(&mut doc, level);
                            anyhow::Ok((level, Arc::new(squish::to_bytes(&mut doc)?)))
                        })
                        .collect::<anyhow::Result<Vec<_>>>()?;
                    anyhow::Ok(Loaded { path, original, results })
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(loaded) => {
                        this.loaded = Some(loaded);
                        this.status = this.summary();
                    }
                    Err(err) => this.status = format!("Blergh, couldn't squeeze that: {err:#}").into(),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn chosen(&self) -> Option<(&Loaded, &Arc<Vec<u8>>)> {
        let loaded = self.loaded.as_ref()?;
        let (_, bytes) = loaded.results.iter().find(|(l, _)| *l == self.level)?;
        Some((loaded, bytes))
    }

    fn summary(&self) -> SharedString {
        match self.chosen() {
            Some((loaded, bytes)) => format!(
                "{} → {} with {} squish",
                human_size(loaded.original),
                human_size(bytes.len() as u64),
                self.level.label()
            )
            .into(),
            None => "Feed me a bloated PDF.".into(),
        }
    }

    fn open(&mut self, _: &Open, _: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Squish".into()),
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
        let Some((loaded, bytes)) = self.chosen() else { return };
        let suggested = squish::default_output(&loaded.path);
        let dir = suggested.parent().unwrap_or(Path::new(".")).to_path_buf();
        let target = cx.prompt_for_new_path(&dir, Some(&file_name(&suggested)));
        let bytes = bytes.clone();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(out))) = target.await else { return };
            let size = bytes.len() as u64;
            let result = cx.background_executor().spawn(async move { std::fs::write(&out, &*bytes).map(|_| out) }).await;
            this.update(cx, |this, cx| {
                this.status = match result {
                    Ok(out) => format!("Squished to {} → {}. Much better.", human_size(size), file_name(&out)).into(),
                    Err(err) => format!("Couldn't save: {err}").into(),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_choice(&self, level: squish::Level, size: u64, original: u64, cx: &mut Context<Self>) -> gpui::AnyElement {
        let selected = level == self.level;
        let color = Kind::Squish.species().color;
        let saved = 100.0 - size as f64 * 100.0 / original.max(1) as f64;
        let (title, about) = match level {
            squish::Level::Light => ("Light", "300 dpi · for printing"),
            squish::Level::Medium => ("Medium", "150 dpi · screen & email"),
            squish::Level::Strong => ("Strong", "100 dpi · smallest"),
        };
        div()
            .id(level.label())
            .w(px(200.))
            .flex()
            .flex_col()
            .gap_1()
            .p_4()
            .rounded_xl()
            .bg(rgb(if selected { theme::CARD_HOVER } else { theme::CARD }))
            .border_2()
            .border_color(if selected { rgb(color).into() } else { gpui::transparent_black() })
            .cursor_pointer()
            .hover(|s| s.bg(rgb(theme::CARD_HOVER)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.level = level;
                this.status = this.summary();
                cx.notify();
            }))
            .child(div().text_lg().font_weight(FontWeight::BOLD).child(title))
            .child(div().text_xs().text_color(rgb(theme::MUTED)).child(about))
            .child(div().mt_2().text_2xl().font_weight(FontWeight::BOLD).child(human_size(size)))
            .child(div().text_color(rgb(color)).child(if saved >= 1.0 {
                format!("{saved:.0}% smaller")
            } else {
                "already lean".to_string()
            }))
            .into_any_element()
    }
}

impl Render for Squish {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.loaded {
            None => empty_state(
                self.portrait.clone(),
                "Feed me a bloated PDF",
                "I'll squeeze it down to something you can actually send.",
                button("open-big", "Open a PDF…", true)
                    .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.open(&Open, w, cx))),
            )
            .into_any_element(),
            Some(loaded) => {
                let choices: Vec<_> = loaded
                    .results
                    .iter()
                    .map(|(level, bytes)| self.render_choice(*level, bytes.len() as u64, loaded.original, cx))
                    .collect();
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_6()
                    .child(img(self.portrait.clone()).w(px(110.)).h(px(114.)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .child(div().text_xl().font_weight(FontWeight::BOLD).child(file_name(&loaded.path)))
                            .child(
                                div()
                                    .text_color(rgb(theme::MUTED))
                                    .child(format!("{} now. How much should I squeeze?", human_size(loaded.original))),
                            ),
                    )
                    .child(div().flex().gap_4().children(choices))
                    .into_any_element()
            }
        };

        let title = self.loaded.as_ref().map(|l| file_name(&l.path)).unwrap_or_default();
        let mut buttons = vec![
            button("open", "Open…", false)
                .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.open(&Open, w, cx)))
                .into_any_element(),
        ];
        if self.loaded.is_some() {
            buttons.push(
                button("save", "Save squished", true)
                    .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.save(&Save, w, cx)))
                    .into_any_element(),
            );
        }
        let status = self.busy.clone().unwrap_or_else(|| self.status.clone());

        div()
            .key_context("Squish")
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
            .child(toolbar(self.portrait.clone(), Kind::Squish, title, buttons))
            .child(body)
            .child(status_bar(status, self.busy.is_some(), Some("esc: all monsters")))
    }
}
