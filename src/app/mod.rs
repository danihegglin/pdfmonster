//! The gpui front end: a home screen with the whole pack, and one screen per monster.

mod chain;
mod hush;
mod munch;
mod pack;
mod peek;
mod snip;
mod squish;
mod stitch;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use gpui::{
    AnyView, App, Application, Bounds, ClickEvent, Context, Entity, ExternalPaths, FocusHandle,
    FontWeight, Image, KeyBinding, Menu, MenuItem, PathPromptOptions, RenderImage, SharedString,
    Stateful, TitlebarOptions, Window, WindowBounds, WindowOptions, actions, div, img, prelude::*,
    px, rgb, size,
};
use image::{Frame, GrayImage, RgbaImage};

use pack::{Kind, PACK};

actions!(pdfmonster, [Open, Save, ToggleOriginal, GoHome, Quit]);

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

pub fn run(initial: Vec<PathBuf>) {
    Application::new().run(move |cx: &mut App| {
        cx.activate(true);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([
            KeyBinding::new("secondary-o", Open, None),
            KeyBinding::new("secondary-s", Save, None),
            KeyBinding::new("space", ToggleOriginal, Some("Munch")),
            KeyBinding::new("escape", GoHome, None),
            KeyBinding::new("secondary-q", Quit, None),
        ]);
        cx.set_menus(vec![
            Menu {
                name: "pdfmonster".into(),
                items: vec![MenuItem::action("Quit pdfmonster", Quit)],
            },
            Menu {
                name: "File".into(),
                items: vec![MenuItem::action("Open…", Open), MenuItem::action("Save…", Save)],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("All Monsters", GoHome),
                    MenuItem::action("Toggle Before / After", ToggleOriginal),
                ],
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
                let mut home = Home {
                    focus: cx.focus_handle(),
                    portraits: PACK.iter().map(|k| k.portrait()).collect(),
                    tools: Vec::new(),
                    active: None,
                    status: "Drop a PDF on a monster to feed it. Anywhere else: one file goes to Munch, several to Stitch.".into(),
                };
                // Watched folders keep working from the moment the app opens.
                if !crate::watch::load().is_empty() {
                    home.tools.push(Tool::new(Screen::Chains, cx));
                    home.status = "Watching folders for new PDFs. See Chain them up.".into();
                }
                match initial.len() {
                    0 => window.focus(&home.focus),
                    n => home.feed(if n == 1 { Kind::Munch } else { Kind::Stitch }, initial, window, cx),
                }
                home
            })
        })
        .expect("failed to open window");
    });
}

/// What the window is showing, besides the home screen.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Monster(Kind),
    Chains,
}

impl From<Kind> for Screen {
    fn from(kind: Kind) -> Self {
        Screen::Monster(kind)
    }
}

/// One open monster screen.
enum Tool {
    Munch(Entity<munch::Munch>),
    Stitch(Entity<stitch::Stitch>),
    Snip(Entity<snip::Snip>),
    Squish(Entity<squish::Squish>),
    Hush(Entity<hush::Hush>),
    Peek(Entity<peek::Peek>),
    Chains(Entity<chain::Chains>),
}

impl Tool {
    fn new(screen: Screen, cx: &mut App) -> Self {
        match screen {
            Screen::Monster(Kind::Munch) => Tool::Munch(cx.new(munch::Munch::new)),
            Screen::Monster(Kind::Stitch) => Tool::Stitch(cx.new(stitch::Stitch::new)),
            Screen::Monster(Kind::Snip) => Tool::Snip(cx.new(snip::Snip::new)),
            Screen::Monster(Kind::Squish) => Tool::Squish(cx.new(squish::Squish::new)),
            Screen::Monster(Kind::Hush) => Tool::Hush(cx.new(hush::Hush::new)),
            Screen::Monster(Kind::Peek) => Tool::Peek(cx.new(peek::Peek::new)),
            Screen::Chains => Tool::Chains(cx.new(chain::Chains::new)),
        }
    }

    fn screen(&self) -> Screen {
        match self {
            Tool::Munch(_) => Kind::Munch.into(),
            Tool::Stitch(_) => Kind::Stitch.into(),
            Tool::Snip(_) => Kind::Snip.into(),
            Tool::Squish(_) => Kind::Squish.into(),
            Tool::Hush(_) => Kind::Hush.into(),
            Tool::Peek(_) => Kind::Peek.into(),
            Tool::Chains(_) => Screen::Chains,
        }
    }

    fn feed(&self, paths: Vec<PathBuf>, cx: &mut App) {
        match self {
            Tool::Munch(view) => view.update(cx, |view, cx| {
                if let Some(path) = paths.into_iter().next() {
                    view.load(path, cx);
                }
            }),
            Tool::Stitch(view) => view.update(cx, |view, cx| view.add(paths, cx)),
            Tool::Snip(view) => view.update(cx, |view, cx| {
                if let Some(path) = paths.into_iter().next() {
                    view.load(path, cx);
                }
            }),
            Tool::Squish(view) => view.update(cx, |view, cx| {
                if let Some(path) = paths.into_iter().next() {
                    view.load(path, cx);
                }
            }),
            Tool::Hush(view) => view.update(cx, |view, cx| {
                if let Some(path) = paths.into_iter().next() {
                    view.load(path, cx);
                }
            }),
            Tool::Peek(view) => view.update(cx, |view, cx| {
                if let Some(path) = paths.into_iter().next() {
                    view.load(path, cx);
                }
            }),
            Tool::Chains(view) => view.update(cx, |view, cx| view.run(paths, cx)),
        }
    }

    fn focus(&self, window: &mut Window, cx: &App) {
        match self {
            Tool::Munch(view) => window.focus(&view.read(cx).focus),
            Tool::Stitch(view) => window.focus(&view.read(cx).focus),
            Tool::Snip(view) => window.focus(&view.read(cx).focus),
            Tool::Squish(view) => window.focus(&view.read(cx).focus),
            Tool::Hush(view) => window.focus(&view.read(cx).focus),
            Tool::Peek(view) => window.focus(&view.read(cx).focus),
            Tool::Chains(view) => window.focus(&view.read(cx).focus),
        }
    }

    fn view(&self) -> AnyView {
        match self {
            Tool::Munch(view) => view.clone().into(),
            Tool::Stitch(view) => view.clone().into(),
            Tool::Snip(view) => view.clone().into(),
            Tool::Squish(view) => view.clone().into(),
            Tool::Hush(view) => view.clone().into(),
            Tool::Peek(view) => view.clone().into(),
            Tool::Chains(view) => view.clone().into(),
        }
    }
}

struct Home {
    focus: FocusHandle,
    portraits: Vec<Arc<Image>>,
    /// Monsters that have been opened, kept so their work survives a trip home.
    tools: Vec<Tool>,
    active: Option<Screen>,
    status: SharedString,
}

impl Home {
    /// Switches to a monster's screen, waking it up if needed.
    fn show(&mut self, screen: impl Into<Screen>, window: &mut Window, cx: &mut Context<Self>) -> &Tool {
        let screen = screen.into();
        let ix = match self.tools.iter().position(|t| t.screen() == screen) {
            Some(ix) => ix,
            None => {
                self.tools.push(Tool::new(screen, cx));
                self.tools.len() - 1
            }
        };
        self.tools[ix].focus(window, cx);
        self.active = Some(screen);
        cx.notify();
        &self.tools[ix]
    }

    fn feed(&mut self, screen: impl Into<Screen>, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = paths.into_iter().filter(|p| is_pdf(p)).collect();
        if paths.is_empty() {
            self.status = "Only PDFs, please. We're picky eaters.".into();
            cx.notify();
            return;
        }
        self.show(screen, window, cx).feed(paths, cx);
    }

    fn go_home(&mut self, _: &GoHome, window: &mut Window, cx: &mut Context<Self>) {
        self.active = None;
        window.focus(&self.focus);
        cx.notify();
    }

    /// ⌘O on the home screen opens a file in Munch.
    fn open(&mut self, _: &Open, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Straighten".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                this.update_in(cx, |this, window, cx| this.feed(Kind::Munch, paths, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn render_tile(&self, ix: usize, kind: Kind, cx: &mut Context<Self>) -> impl IntoElement {
        let s = kind.species();
        div()
            .id(("tile", ix))
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .p_4()
            .rounded_xl()
            .bg(rgb(theme::CARD))
            .border_2()
            .border_color(rgb(theme::BORDER))
            .child(img(self.portraits[ix].clone()).w(px(104.)).h(px(108.)))
            .child(div().text_lg().font_weight(FontWeight::BOLD).child(s.name))
            .child(div().text_color(rgb(s.color)).font_weight(FontWeight::MEDIUM).child(s.job))
            .child(
                div()
                    .text_xs()
                    .text_center()
                    .text_color(rgb(theme::MUTED))
                    .child(s.blurb),
            )
            .map(|tile| {
                if s.ready {
                    tile.cursor_pointer()
                        .hover(|t| t.bg(rgb(theme::CARD_HOVER)).border_color(rgb(s.color)))
                        .drag_over::<ExternalPaths>(move |t, _, _, _| t.border_color(rgb(s.color)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.show(kind, window, cx);
                        }))
                        .on_drop(cx.listener(move |this, paths: &ExternalPaths, window, cx| {
                            this.feed(kind, paths.paths().to_vec(), window, cx)
                        }))
                } else {
                    tile.opacity(0.5).child(
                        div()
                            .mt_1()
                            .px_2()
                            .rounded_md()
                            .bg(rgb(theme::BORDER))
                            .text_xs()
                            .child("hatching soon"),
                    )
                }
            })
    }

    fn render_home(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tiles: Vec<_> = PACK
            .iter()
            .enumerate()
            .map(|(ix, &kind)| self.render_tile(ix, kind, cx).into_any_element())
            .collect();
        div()
            .id("home")
            .flex_1()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .items_center()
            .gap_6()
            .p_8()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .child(div().text_3xl().font_weight(FontWeight::BOLD).child("Meet the pack"))
                    .child(
                        div()
                            .text_color(rgb(theme::MUTED))
                            .child("One monster per chore. Pick one, drop in a PDF, done."),
                    ),
            )
            .child(div().w(px(820.)).grid().grid_cols(3).gap_4().children(tiles))
            .child(self.render_chain_banner(cx))
    }

    /// The way into chains, below the pack.
    fn render_chain_banner(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut faces = Vec::new();
        for (ix, kind) in [Kind::Munch, Kind::Peek, Kind::Squish].into_iter().enumerate() {
            if ix > 0 {
                faces.push(div().text_color(rgb(theme::MUTED)).child("→").into_any_element());
            }
            faces.push(img(self.portrait(kind)).w(px(40.)).h(px(41.)).into_any_element());
        }
        div()
            .id("chains")
            .w(px(820.))
            .flex()
            .items_center()
            .gap_4()
            .p_4()
            .rounded_xl()
            .bg(rgb(theme::CARD))
            .border_2()
            .border_color(rgb(theme::BORDER))
            .cursor_pointer()
            .hover(|t| t.bg(rgb(theme::CARD_HOVER)).border_color(rgb(theme::ACCENT)))
            .drag_over::<ExternalPaths>(|t, _, _, _| t.border_color(rgb(theme::ACCENT)))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.show(Screen::Chains, window, cx);
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.feed(Screen::Chains, paths.paths().to_vec(), window, cx)
            }))
            .child(div().flex().items_center().gap_1().children(faces))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().text_lg().font_weight(FontWeight::BOLD).child("Chain them up"))
                    .child(div().text_color(rgb(theme::MUTED)).child(
                        "Munch straightens a scan, Peek makes it searchable, Squish shrinks it. One drop, three jobs.",
                    )),
            )
            .child(button("build-chain", "Build a chain", false))
    }

    fn portrait(&self, kind: Kind) -> Arc<Image> {
        let ix = PACK.iter().position(|&k| k == kind).expect("every monster is in the pack");
        self.portraits[ix].clone()
    }
}

impl Render for Home {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self
            .active
            .and_then(|screen| self.tools.iter().find(|t| t.screen() == screen));

        let root = div()
            .key_context("Home")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::go_home))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::BG))
            .text_color(rgb(theme::TEXT))
            .text_sm();

        match active {
            Some(tool) => root.child(tool.view()),
            None => root
                .on_action(cx.listener(Self::open))
                .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                    // One scan goes to Munch; a pile of them is clearly a job for Stitch.
                    let pdfs = paths.paths().iter().filter(|p| is_pdf(p)).count();
                    let kind = if pdfs > 1 { Kind::Stitch } else { Kind::Munch };
                    this.feed(kind, paths.paths().to_vec(), window, cx)
                }))
                .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(rgb(0x221c33)))
                .child(self.render_home(cx))
                .child(status_bar(self.status.clone(), false, None)),
        }
    }
}

/// The bar at the top of every monster's screen: back to the pack, who's working, and its buttons.
fn toolbar(
    portrait: Arc<Image>,
    kind: Kind,
    title: String,
    buttons: impl IntoIterator<Item = gpui::AnyElement>,
) -> impl IntoElement {
    toolbar_with(img(portrait).w(px(34.)).h(px(35.)), kind.species().name, title, buttons)
}

/// [`toolbar`] for screens that aren't one monster's.
fn toolbar_with(
    icon: impl IntoElement,
    name: &'static str,
    title: String,
    buttons: impl IntoIterator<Item = gpui::AnyElement>,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2()
        .bg(rgb(theme::PANEL))
        .border_b_1()
        .border_color(rgb(theme::BORDER))
        .child(
            button("home", "← Pack", false)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(GoHome), cx)),
        )
        .child(icon)
        .child(div().text_lg().font_weight(FontWeight::BOLD).child(name))
        .child(div().text_color(rgb(theme::MUTED)).truncate().child(title))
        .child(div().flex_1())
        .children(buttons)
}

/// What a monster shows before it has been fed.
fn empty_state(
    portrait: Arc<Image>,
    title: &'static str,
    subtitle: &'static str,
    action: impl IntoElement,
) -> impl IntoElement {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_4()
        .child(img(portrait).w(px(160.)).h(px(165.)))
        .child(div().text_2xl().font_weight(FontWeight::BOLD).child(title))
        .child(div().text_color(rgb(theme::MUTED)).child(subtitle))
        .child(action)
}

fn status_bar(status: SharedString, busy: bool, hint: Option<&'static str>) -> impl IntoElement {
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
        .child(div().size(px(8.)).rounded_full().bg(rgb(if busy { theme::YELLOW } else { theme::PINK })))
        .child(div().text_color(rgb(if busy { theme::YELLOW } else { theme::MUTED })).child(status))
        .child(div().flex_1())
        .when_some(hint, |d, hint| d.child(div().text_color(rgb(theme::MUTED)).child(hint)))
}

fn button(id: impl Into<gpui::ElementId>, label: impl Into<SharedString>, primary: bool) -> Stateful<gpui::Div> {
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

fn render_image(gray: &GrayImage) -> Arc<RenderImage> {
    // Gray pixels are identical in RGBA and BGRA, so no channel swizzle is needed.
    let rgba = RgbaImage::from_fn(gray.width(), gray.height(), |x, y| {
        let v = gray.get_pixel(x, y).0[0];
        image::Rgba([v, v, v, 255])
    });
    Arc::new(RenderImage::new(vec![Frame::new(rgba)]))
}

/// Like [`render_image`], for colour bitmaps.
fn render_rgba(rgba: &RgbaImage) -> Arc<RenderImage> {
    let mut bgra = rgba.clone();
    for pixel in bgra.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Arc::new(RenderImage::new(vec![Frame::new(bgra)]))
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn is_pdf(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}
