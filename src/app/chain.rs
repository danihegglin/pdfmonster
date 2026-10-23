//! The chains screen: line monsters up, drop in PDFs, get them back done.

use std::{path::PathBuf, sync::Arc, time::Instant};

use gpui::{
    ClickEvent, Context, ExternalPaths, FocusHandle, FontWeight, Image, PathPromptOptions, SharedString,
    Window, div, img, prelude::*, px, rgb,
};

use super::{
    Open, button, file_name, is_pdf, pack::Kind, plural, small_button, status_bar, theme, toolbar_with,
};
use crate::{
    chain::{self, Chain, Step},
    human_size, pdf, squish,
};

pub struct Chains {
    pub focus: FocusHandle,
    portraits: Vec<(Kind, Arc<Image>)>,
    chain: Chain,
    /// Files that went through the chain, newest first.
    runs: Vec<Run>,
    running: usize,
    status: SharedString,
}

struct Run {
    input: PathBuf,
    chain: Chain,
    result: Option<anyhow::Result<Done>>,
}

struct Done {
    output: PathBuf,
    before: u64,
    after: u64,
    log: Vec<String>,
    millis: u128,
}

fn kind(step: Step) -> Kind {
    match step {
        Step::Munch => Kind::Munch,
        Step::Peek => Kind::Peek,
        Step::Squish(_) => Kind::Squish,
        Step::Hush => Kind::Hush,
    }
}

impl Chains {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let kinds = [Kind::Munch, Kind::Peek, Kind::Squish, Kind::Hush];
        Chains {
            focus: cx.focus_handle(),
            portraits: kinds.into_iter().map(|k| (k, k.portrait())).collect(),
            chain: Chain::classic(),
            runs: Vec::new(),
            running: 0,
            status: "Line monsters up, then drop PDFs here. Each file is saved next to the original.".into(),
        }
    }

    fn portrait(&self, kind: Kind) -> Arc<Image> {
        self.portraits.iter().find(|(k, _)| *k == kind).map(|(_, p)| p.clone()).expect("chainable monster")
    }

    /// Sends each PDF through the current chain and saves it as `NAME.chained.pdf`.
    pub fn run(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if self.chain.0.is_empty() {
            self.status = "Add a monster to the chain first.".into();
            cx.notify();
            return;
        }
        for input in paths.into_iter().filter(|p| is_pdf(p)) {
            let chain = self.chain.clone();
            self.runs.insert(0, Run { input: input.clone(), chain: chain.clone(), result: None });
            self.running += 1;
            cx.spawn(async move |this, cx| {
                let task_input = input.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        let start = Instant::now();
                        let before = std::fs::metadata(&task_input)?.len();
                        let (bytes, log) = chain::run(pdf::open(&task_input)?, &chain, &|_, _| {})?;
                        let output = chain::unused(chain::default_output(&task_input));
                        std::fs::write(&output, &bytes)
                            .map_err(|e| anyhow::anyhow!("could not write {}: {e}", output.display()))?;
                        anyhow::Ok(Done {
                            output,
                            before,
                            after: bytes.len() as u64,
                            log,
                            millis: start.elapsed().as_millis(),
                        })
                    })
                    .await;
                this.update(cx, |this, cx| {
                    this.running -= 1;
                    if let Some(run) = this.runs.iter_mut().find(|r| r.input == input && r.result.is_none()) {
                        run.result = Some(result);
                    }
                    this.status = this.summary();
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        self.status = self.summary();
        cx.notify();
    }

    fn summary(&self) -> SharedString {
        if self.running > 0 {
            return format!("Chaining {} file{}…", self.running, plural(self.running)).into();
        }
        let done = self.runs.iter().filter(|r| matches!(r.result, Some(Ok(_)))).count();
        let failed = self.runs.iter().filter(|r| matches!(r.result, Some(Err(_)))).count();
        match (done, failed) {
            (0, 0) => "Line monsters up, then drop PDFs here. Each file is saved next to the original.".into(),
            (done, 0) => format!("{done} file{} done. Drop more any time.", plural(done)).into(),
            (done, failed) => format!("{done} done, {failed} failed.").into(),
        }
    }

    fn open(&mut self, _: &Open, _: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Run chain".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                this.update(cx, |this, cx| this.run(paths, cx)).ok();
            }
        })
        .detach();
    }

    fn edit(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Vec<Step>)) {
        change(&mut self.chain.0);
        cx.notify();
    }

    fn render_step(&self, ix: usize, step: Step, cx: &mut Context<Self>) -> gpui::AnyElement {
        let color = kind(step).species().color;
        let job = match step {
            // Squish's level is picked by clicking it, so say so.
            Step::Squish(level) => format!("shrinks it · {} ▾", level.label()),
            step => step.job(),
        };
        div()
            .id(("step", ix))
            .relative()
            .w(px(140.))
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .p_3()
            .rounded_xl()
            .bg(rgb(theme::CARD))
            .border_2()
            .border_color(rgb(color))
            .when(matches!(step, Step::Squish(_)), |d| {
                d.cursor_pointer().hover(|d| d.bg(rgb(theme::CARD_HOVER))).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        this.edit(cx, |steps| {
                            if let Step::Squish(level) = &mut steps[ix] {
                                let all = squish::Level::ALL;
                                let next = all.iter().position(|l| l == level).map_or(0, |i| (i + 1) % all.len());
                                *level = all[next];
                            }
                        })
                    },
                ))
            })
            .child(
                div().absolute().top_1().right_1().child(
                    small_button(("remove", ix), "✕").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.edit(cx, |steps| {
                            steps.remove(ix);
                        })
                    })),
                ),
            )
            .child(img(self.portrait(kind(step))).w(px(64.)).h(px(66.)))
            .child(div().font_weight(FontWeight::BOLD).child(step.name()))
            .child(div().text_xs().text_center().text_color(rgb(color)).child(job))
            .into_any_element()
    }

    fn render_run(&self, ix: usize, run: &Run) -> gpui::AnyElement {
        let (headline, color, lines): (String, u32, Vec<String>) = match &run.result {
            None => ("working…".into(), theme::YELLOW, Vec::new()),
            Some(Err(err)) => (format!("{err:#}"), theme::PINK, Vec::new()),
            Some(Ok(done)) => (
                format!(
                    "→ {} · {} → {} · {} ms",
                    file_name(&done.output),
                    human_size(done.before),
                    human_size(done.after),
                    done.millis
                ),
                theme::MINT,
                done.log.clone(),
            ),
        };
        div()
            .id(("run", ix))
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .rounded_lg()
            .bg(rgb(theme::CARD))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(div().font_weight(FontWeight::BOLD).child(file_name(&run.input)))
                    .child(div().text_color(rgb(color)).truncate().child(headline)),
            )
            .child(div().text_xs().text_color(rgb(theme::MUTED)).child(run.chain.to_string()))
            .children(lines.into_iter().map(|l| div().text_sm().text_color(rgb(theme::MUTED)).child(l)))
            .into_any_element()
    }
}

impl Render for Chains {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut chain = Vec::new();
        for (ix, &step) in self.chain.0.iter().enumerate() {
            if ix > 0 {
                chain.push(div().text_2xl().text_color(rgb(theme::MUTED)).child("→").into_any_element());
            }
            chain.push(self.render_step(ix, step, cx));
        }
        if chain.is_empty() {
            chain.push(
                div()
                    .text_color(rgb(theme::MUTED))
                    .child("No monsters yet. Add some below.")
                    .into_any_element(),
            );
        }
        let adders: Vec<_> = Step::ALL
            .into_iter()
            .enumerate()
            .map(|(ix, step)| {
                let taken = self.chain.0.iter().any(|s| kind(*s) == kind(step));
                button(["add-munch", "add-peek", "add-squish", "add-hush"][ix], format!("+ {}", step.name()), false)
                    .when(taken, |b| b.opacity(0.4))
                    .when(!taken, |b| {
                        b.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.edit(cx, |steps| steps.push(step))
                        }))
                    })
                    .into_any_element()
            })
            .collect();
        let runs: Vec<_> = self.runs.iter().enumerate().map(|(ix, run)| self.render_run(ix, run)).collect();
        let heading = |text: &'static str| div().text_xs().font_weight(FontWeight::BOLD).text_color(rgb(theme::MUTED)).child(text);

        let body = div()
            .id("chains-body")
            .flex_1()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_4()
            .p_6()
            .child(heading("YOUR CHAIN"))
            .child(div().flex().flex_wrap().items_center().gap_3().children(chain))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .children(adders)
                    .child(
                        button("classic", "Reset to Munch → Peek → Squish", false).on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                this.chain = Chain::classic();
                                cx.notify();
                            },
                        )),
                    ),
            )
            .child(heading("DROP PDFS ANYWHERE ON THIS SCREEN"))
            .when(runs.is_empty(), |d| {
                d.child(
                    div()
                        .p_6()
                        .rounded_lg()
                        .border_2()
                        .border_dashed()
                        .border_color(rgb(theme::BORDER))
                        .text_color(rgb(theme::MUTED))
                        .text_center()
                        .child("Each file goes through the chain in order and is saved as NAME.chained.pdf next to it."),
                )
            })
            .child(div().flex().flex_col().gap_2().children(runs));

        let icon = div()
            .flex()
            .children([Kind::Munch, Kind::Peek, Kind::Squish].map(|k| img(self.portrait(k)).w(px(24.)).h(px(25.))));
        let buttons = vec![
            button("open", "Open…", self.runs.is_empty())
                .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.open(&Open, w, cx)))
                .into_any_element(),
        ];

        div()
            .key_context("Chains")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::open))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                if paths.paths().iter().any(|p| is_pdf(p)) {
                    this.run(paths.paths().to_vec(), cx);
                } else {
                    this.status = "Only PDFs, please. We're picky eaters.".into();
                    cx.notify();
                }
            }))
            .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(rgb(0x221c33)))
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar_with(icon, "Chains", self.chain.to_string(), buttons))
            .child(body)
            .child(status_bar(self.status.clone(), self.running > 0, Some("click Squish to change its level · esc: all monsters")))
    }
}
