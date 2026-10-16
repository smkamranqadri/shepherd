//! A PDF tab's pages, inside the file viewer (`ui::viewer`). Every page has its place from the start (its size is
//! read with the file), so the scroll is steady while pages are drawn; the widest page fits the width until a zoom.
//!
//! The file is read, checked against `files::pdf`'s budgets and drawn in a worker process (`files::pdf_worker`),
//! one per view and never more, driven by a thread of the view's that asks it for the pages on screen and one each
//! side, the nearest first; a page that scrolls away before it is drawn is cancelled. Each request has its time: a
//! worker past it, or one that crashed, is killed, its page says so, and a new worker takes the next pages; after
//! two crashes running none is started again and the view offers the file to its default app. Drawn pages are kept
//! (`pdf::Cache`, by page and zoom bucket; the wanted ones never go) until the tab goes out of sight, when they and
//! the worker are let go; the zoom and the place stay with the tab's view, and closing the tab kills the worker.
//!
//! Zoom is Cmd/Ctrl with +, − and 0 (fit the width) while the view has the keyboard (`bindings::PDF_VIEW`), and
//! Cmd/Ctrl with the scroll wheel, about the pointer. While a zoom moves the pages show their old drawing stretched;
//! they are drawn again once it has been still for `SETTLE`.
use crate::bindings::{PDF_VIEW, PdfZoomIn, PdfZoomOut, PdfZoomReset};
use crate::files::content::human_size;
use crate::files::pdf::{self, Cache, Doc, Layout, PageInfo, PageProblem, Refusal};
use crate::files::pdf_worker::{Fault, Faults, Killer, Request, Response, Timeouts, Worker};
use crate::theme::Theme;
use futures::StreamExt;
use futures::channel::{mpsc as async_mpsc, oneshot};
use gpui::{
    AnyElement, App, ClickEvent, Context, DispatchPhase, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement,
    Render, RenderImage, ScrollHandle, ScrollWheelEvent, SharedString, StatefulInteractiveElement, Styled, Task, Window, canvas, div, point, prelude::*, px,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use tokio::runtime::Handle;

/// How long a zoom must be still before the pages are drawn at it.
const SETTLE: Duration = Duration::from_millis(150);
/// How soon a view shown again looks once more for its last worker to have gone, before it starts the next.
const RECHECK: Duration = Duration::from_millis(50);
/// The zoom, as a share of fitting the width.
const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 8.;
const ZOOM_STEP: f32 = 1.25;
/// An arrow key's scroll, in logical pixels.
const LINE: f32 = 48.;
/// The digits a page number may have in the go-to field.
const MAX_DIGITS: usize = 6;

/// A page for the worker to draw, at `scale` (device pixels per point), kept under `bucket`.
struct Job {
    page: usize,
    bucket: i32,
    scale: f32,
    cancel: Arc<AtomicBool>,
}

/// How a page came back.
enum Outcome {
    /// As gpui's BGRA image.
    Image(Arc<RenderImage>),
    /// The engine could not draw it.
    Failed,
    /// The worker took longer than `Timeouts::render` and was killed.
    TooLong,
    /// The worker crashed on it.
    Crashed,
}

enum Event {
    Drawn { page: usize, bucket: i32, outcome: Outcome },
    /// The worker crashed `Faults::LIMIT` times running: no more pages.
    GaveUp,
}

/// What opening the file came to.
enum Opening {
    Ready(Doc),
    Refused(Refusal),
    TimedOut,
    Crashed,
}

/// The view's hold on its driver and worker: `stop` asks the driver to end, `killer` ends the worker now, and
/// `alive` stays set until the driver has ended (and with it the worker).
#[derive(Clone, Default)]
struct Control {
    stop: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    killer: Arc<Mutex<Option<Killer>>>,
}

impl Control {
    fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(killer) = self.killer.lock().unwrap_or_else(|e| e.into_inner()).take() {
            killer.kill();
        }
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    fn alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }
}

/// Clears `alive` when the driver ends, however it ends.
struct Alive(Arc<AtomicBool>);

impl Drop for Alive {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

/// The driver, on a thread of the view's: starts a worker (`spawn`) on `path`, says what opening it came to, then
/// asks it for each job not cancelled, one at a time, until the view lets go of `jobs` or stops it. A worker killed
/// for a page past its time, or one that crashed, is replaced for the next job; a time past fails only its page, and
/// after `Faults::LIMIT` crashes running (a restart that fails is one) the driver gives up.
fn drive(
    path: PathBuf,
    spawn: impl Fn(Duration) -> Result<Worker, Fault>,
    timeouts: Timeouts,
    control: Control,
    opened: oneshot::Sender<Opening>,
    jobs: mpsc::Receiver<Job>,
    events: async_mpsc::UnboundedSender<Event>,
) {
    let _alive = Alive(control.alive.clone());
    let start = || -> Result<(Worker, Result<Doc, Refusal>), Fault> {
        let mut worker = spawn(timeouts.open)?;
        *control.killer.lock().unwrap_or_else(|e| e.into_inner()) = Some(worker.killer());
        if control.stopped() {
            return Err(Fault::Crashed);
        }
        match worker.request(&Request::Open(path.clone()), timeouts.open)? {
            Response::Opened(opened) => Ok((worker, opened)),
            _ => Err(Fault::Crashed),
        }
    };
    let mut worker = match start() {
        Ok((worker, Ok(doc))) => {
            let _ = opened.send(Opening::Ready(doc));
            Some(worker)
        }
        Ok((_, Err(refusal))) => {
            let _ = opened.send(Opening::Refused(refusal));
            return;
        }
        Err(Fault::TimedOut) => {
            let _ = opened.send(Opening::TimedOut);
            return;
        }
        Err(Fault::Crashed | Fault::Mismatch) => {
            let _ = opened.send(Opening::Crashed);
            return;
        }
    };
    let mut faults = Faults::default();
    for job in jobs {
        if control.stopped() {
            return;
        }
        if job.cancel.load(Ordering::Relaxed) {
            continue;
        }
        let result = match worker.as_mut() {
            Some(current) => current.request(&Request::Render(job.page, job.scale), timeouts.render),
            // the file opened again as before: the pages go on; a restart that fails is a crash
            None => match start() {
                Ok((mut fresh, Ok(_))) => {
                    let result = fresh.request(&Request::Render(job.page, job.scale), timeouts.render);
                    worker = Some(fresh);
                    result
                }
                _ => Err(Fault::Crashed),
            },
        };
        if result.is_err() && control.stopped() {
            return;
        }
        let (outcome, give_up) = match result {
            Ok(Response::Image(mut rgba)) => {
                for pixel in rgba.pixels_mut() {
                    pixel.0.swap(0, 2);
                }
                (Outcome::Image(Arc::new(RenderImage::new(vec![image::Frame::new(rgba)]))), faults.note(Ok(())))
            }
            Ok(_) => (Outcome::Failed, faults.note(Ok(()))),
            Err(fault) => {
                worker = None;
                (if fault == Fault::TimedOut { Outcome::TooLong } else { Outcome::Crashed }, faults.note(Err(fault)))
            }
        };
        if (!job.cancel.load(Ordering::Relaxed) || !matches!(outcome, Outcome::Image(_))) && events.unbounded_send(Event::Drawn { page: job.page, bucket: job.bucket, outcome }).is_err() {
            return;
        }
        if give_up {
            let _ = events.unbounded_send(Event::GaveUp);
            return;
        }
    }
}

enum State {
    Loading,
    Ready(Doc),
    Refused(Refusal),
    NotResponding,
    /// The worker crashed opening it, or `Faults::LIMIT` times running.
    CouldNotShow,
}

pub struct PdfView {
    focus: FocusHandle,
    rt: Handle,
    path: PathBuf,
    state: State,
    /// The driver's queue, while the tab shows.
    worker: Option<mpsc::Sender<Job>>,
    /// The last driver's: a new one starts only once it is no longer alive.
    control: Option<Control>,
    /// Bumped by every driver started: what an older one sends is dropped.
    generation: u64,
    opening: Option<Task<()>>,
    receiving: Option<Task<()>>,
    recheck: Option<Task<()>>,
    /// A share of fitting the width: 1 fits it.
    zoom: f32,
    scroll: ScrollHandle,
    /// Last frame's layout: what the place is kept by when the next one differs (a zoom, a resize).
    layout: Option<Layout>,
    /// The point of the viewport a zoom is about (the pointer's), until the next layout.
    zoom_at: Option<(f32, f32)>,
    cache: Cache<Arc<RenderImage>>,
    /// Images let go, for the window's atlas to drop on the next frame.
    retired: Vec<Arc<RenderImage>>,
    /// The pages asked of the worker and not yet back, by (page, bucket), with their cancel flags.
    in_flight: HashMap<(usize, i32), Arc<AtomicBool>>,
    /// The pages wanted now (on screen and one each side, at the zoom drawn): never dropped from the cache.
    wanted: HashSet<(usize, i32)>,
    /// Pages that did not come back drawn, and what their place says.
    failed: HashMap<usize, &'static str>,
    /// A zoom is moving: pages are not drawn until it settles.
    zooming: bool,
    settle: Option<Task<()>>,
    /// The go-to-page field's digits, while it is open.
    goto: Option<String>,
}

impl Focusable for PdfView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Drop for PdfView {
    /// The tab closed: its worker goes now, not when its driver next looks.
    fn drop(&mut self) {
        self.stop();
    }
}

impl PdfView {
    pub fn new(path: PathBuf, rt: Handle, cx: &mut Context<Self>) -> PdfView {
        let mut view = PdfView {
            focus: cx.focus_handle(),
            rt,
            path,
            state: State::Loading,
            worker: None,
            control: None,
            generation: 0,
            opening: None,
            receiving: None,
            recheck: None,
            zoom: 1.,
            scroll: ScrollHandle::new(),
            layout: None,
            zoom_at: None,
            cache: Cache::new(pdf::CACHE_BYTES),
            retired: Vec::new(),
            in_flight: HashMap::new(),
            wanted: HashSet::new(),
            failed: HashMap::new(),
            zooming: false,
            settle: None,
            goto: None,
        };
        view.start(cx);
        view
    }

    /// Starts a driver and its worker on the file, unless the last one is still going (it was just stopped: the
    /// view looks again in a moment): it reads and checks the file again (it may have changed while the tab was out
    /// of sight), then draws what is asked.
    fn start(&mut self, cx: &mut Context<Self>) {
        if self.control.as_ref().is_some_and(Control::alive) {
            if self.recheck.is_none() {
                self.recheck = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(RECHECK).await;
                    let _ = this.update(cx, |v, cx| {
                        v.recheck = None;
                        cx.notify();
                    });
                }));
            }
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        let (jobs, queue) = mpsc::channel();
        let (opened_tx, opened) = oneshot::channel();
        let (events_tx, mut events) = async_mpsc::unbounded();
        let control = Control::default();
        control.alive.store(true, Ordering::Relaxed);
        let (path, driver) = (self.path.clone(), control.clone());
        let spawn = |timeout| Worker::spawn(Worker::command().map_err(|_| Fault::Crashed)?, timeout);
        let started = std::thread::Builder::new().name("pdf-pages".into()).spawn(move || drive(path, spawn, Timeouts::default(), driver, opened_tx, queue, events_tx));
        if started.is_err() {
            self.state = State::CouldNotShow;
            return;
        }
        self.control = Some(control);
        self.worker = Some(jobs);
        self.opening = Some(cx.spawn(async move |this, cx| {
            let state = match opened.await {
                Ok(Opening::Ready(doc)) => State::Ready(doc),
                Ok(Opening::Refused(refusal)) => State::Refused(refusal),
                Ok(Opening::TimedOut) => State::NotResponding,
                // the driver went without a word, or the worker fell over on the file
                Ok(Opening::Crashed) | Err(_) => State::CouldNotShow,
            };
            let _ = this.update(cx, |v, cx| v.opened(generation, state, cx));
        }));
        self.receiving = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = events.next().await {
                if this.update(cx, |v, cx| v.event(generation, event, cx)).is_err() {
                    return;
                }
            }
        }));
    }

    fn opened(&mut self, generation: u64, state: State, cx: &mut Context<Self>) {
        if generation != self.generation {
            return;
        }
        if !matches!(state, State::Ready(_)) {
            self.stop();
        }
        self.state = state;
        cx.notify();
    }

    fn event(&mut self, generation: u64, event: Event, cx: &mut Context<Self>) {
        let Event::Drawn { page, bucket, outcome } = event else {
            if generation == self.generation {
                self.stop();
                self.state = State::CouldNotShow;
                cx.notify();
            }
            return;
        };
        let key = (page, bucket);
        if generation != self.generation || self.in_flight.remove(&key).is_none() {
            if let Outcome::Image(image) = outcome {
                self.retired.push(image);
            }
            return;
        }
        match outcome {
            Outcome::Image(image) => {
                let size = image.size(0);
                let bytes = (size.width.0.max(0) as usize) * (size.height.0.max(0) as usize) * 4;
                let gone = self.cache.insert(key, image, bytes, &self.wanted);
                self.retired.extend(gone);
            }
            Outcome::Failed => {
                self.failed.insert(page, "This page could not be drawn");
            }
            Outcome::TooLong => {
                self.failed.insert(page, "This page took too long");
            }
            Outcome::Crashed => {
                self.failed.insert(page, "This page could not be shown");
            }
        }
        cx.notify();
    }

    /// The driver is told to end, the worker is killed now, and what it was asked is cancelled.
    fn stop(&mut self) {
        for cancel in self.in_flight.values() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.in_flight.clear();
        self.wanted.clear();
        if let Some(control) = &self.control {
            control.stop();
        }
        (self.worker, self.opening, self.receiving) = (None, None, None);
        self.generation += 1;
    }

    /// Out of sight: the drawn pages leave the window's atlas, the worker goes; the zoom and the place stay, and
    /// the pages are drawn again when the tab shows.
    pub fn hide(&mut self, window: Option<&mut Window>) {
        self.stop();
        self.failed.clear();
        let images = self.cache.clear();
        match window {
            Some(window) => {
                for image in images.into_iter().chain(self.retired.drain(..)) {
                    let _ = window.drop_image(image);
                }
            }
            None => self.retired.extend(images),
        }
    }

    /// The zoom set to `zoom` (a share of fitting the width), about `at` in the viewport (its middle when not given):
    /// the pages show stretched now and are drawn at it once it settles.
    fn zoom_to(&mut self, zoom: f32, at: Option<(f32, f32)>, cx: &mut Context<Self>) {
        let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        if (zoom - self.zoom).abs() < 1e-4 || !matches!(self.state, State::Ready(_)) {
            return;
        }
        let size = self.scroll.bounds().size;
        self.zoom_at = Some(at.unwrap_or((f32::from(size.width) / 2., f32::from(size.height) / 2.)));
        self.zoom = zoom;
        self.zooming = true;
        self.settle = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SETTLE).await;
            let _ = this.update(cx, |v, cx| {
                v.zooming = false;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Asks the driver for `pages` in `order` (the visible ones first) at `scale` device pixels per point: the ones
    /// neither drawn nor asked for already, every frame (one dropped or failed while still wanted is asked again);
    /// what was asked before and is no longer wanted is cancelled. The cache grows to hold the wanted pages.
    fn request(&mut self, pages: &[PageInfo], order: impl Iterator<Item = usize>, scale: f32) {
        let jobs: Vec<(usize, i32, f32)> = order
            .filter(|i| pages[*i].problem.is_none() && !self.failed.contains_key(i))
            .map(|i| {
                let (bucket, scale) = pdf::render_bucket(pages[i].size, scale);
                (i, bucket, scale)
            })
            .collect();
        self.wanted = jobs.iter().map(|(i, b, _)| (*i, *b)).collect();
        let bytes: f32 = jobs.iter().map(|(i, _, s)| pages[*i].size.0 * pages[*i].size.1 * s * s * 4.).sum();
        self.cache.fit(bytes.min(usize::MAX as f32) as usize);
        let wanted = &self.wanted;
        self.in_flight.retain(|key, cancel| {
            let keep = wanted.contains(key);
            if !keep {
                cancel.store(true, Ordering::Relaxed);
            }
            keep
        });
        let Some(worker) = &self.worker else { return };
        for (page, bucket, scale) in jobs {
            if self.cache.contains((page, bucket)) || self.in_flight.contains_key(&(page, bucket)) {
                continue;
            }
            let cancel = Arc::new(AtomicBool::new(false));
            if worker.send(Job { page, bucket, scale, cancel: cancel.clone() }).is_ok() {
                self.in_flight.insert((page, bucket), cancel);
            }
        }
    }

    fn go_to_page(&mut self, page: usize, cx: &mut Context<Self>) {
        let Some(layout) = &self.layout else { return };
        let Some(rect) = layout.rects.get(page.min(layout.rects.len().saturating_sub(1))) else { return };
        let max = self.scroll.max_offset();
        let y = (rect[1] - pdf::PAD).clamp(0., f32::from(max.y).max(0.));
        self.scroll.set_offset(point(self.scroll.offset().x, px(-y)));
        cx.notify();
    }

    fn open_externally(&mut self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let job = self.rt.spawn(async move { crate::platform::launch(&[crate::platform::OPEN.into(), path.into()]).await });
        cx.spawn(async move |_, cx| {
            if let Ok(Err(e)) = job.await {
                cx.update(|cx| crate::ui::toast::error(cx, e));
            }
        })
        .detach();
    }

    fn key(&mut self, e: &KeyDownEvent, cx: &mut Context<Self>) {
        let (key, m) = (e.keystroke.key.as_str(), &e.keystroke.modifiers);
        if let Some(digits) = self.goto.as_mut() {
            match key {
                "escape" => self.goto = None,
                "enter" => {
                    let page = digits.parse::<usize>().ok().filter(|p| *p > 0);
                    self.goto = None;
                    if let Some(page) = page {
                        self.go_to_page(page - 1, cx);
                    }
                }
                "backspace" => {
                    digits.pop();
                }
                _ if !m.modified() && key.len() == 1 && key.as_bytes()[0].is_ascii_digit() => {
                    if digits.len() < MAX_DIGITS {
                        digits.push_str(key);
                    }
                }
                _ => return,
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if m.modified() && !(m.shift && key == "space") {
            return;
        }
        let (offset, max) = (self.scroll.offset(), self.scroll.max_offset());
        let page = f32::from(self.scroll.bounds().size.height) - LINE;
        let (x, y) = (-f32::from(offset.x), -f32::from(offset.y));
        let (x, y) = match key {
            "up" => (x, y - LINE),
            "down" => (x, y + LINE),
            "left" => (x - LINE, y),
            "right" => (x + LINE, y),
            "pageup" => (x, y - page),
            "space" if m.shift => (x, y - page),
            "pagedown" | "space" => (x, y + page),
            "home" => (x, 0.),
            "end" => (x, f32::MAX),
            _ => return,
        };
        let (x, y) = (x.clamp(0., f32::from(max.x).max(0.)), y.clamp(0., f32::from(max.y).max(0.)));
        self.scroll.set_offset(point(px(-x), px(-y)));
        cx.stop_propagation();
        cx.notify();
    }

    /// A message in place of the pages, with the file offered to its default app.
    fn notice(&self, theme: &Theme, title: impl Into<SharedString>, hint: impl Into<SharedString>, cx: &mut Context<Self>) -> AnyElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .font_family(theme.font_ui.clone())
            .child(div().text_color(theme.text).child(title.into()))
            .child(div().text_size(px(12.)).text_color(theme.text_muted).child(hint.into()))
            .child(button(theme, "pdf-notice-open", "Open externally").mt(px(8.)).on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.open_externally(cx))))
            .into_any_element()
    }

    fn refused(&self, theme: &Theme, refusal: &Refusal, cx: &mut Context<Self>) -> AnyElement {
        let (title, hint): (&str, String) = match refusal {
            Refusal::TooLarge(size) => ("This PDF is too large to show", format!("It is {}; the viewer shows PDFs up to {}.", human_size(*size), human_size(pdf::MAX_PDF))),
            Refusal::Encrypted => ("This PDF is password-protected", "The viewer does not open encrypted PDFs.".into()),
            Refusal::Broken => ("This PDF can't be shown", "It could not be read as a PDF.".into()),
            Refusal::Empty => ("This PDF has no pages", String::new()),
            Refusal::TooManyObjects => ("This PDF is too complex to show", format!("It has more than {} images and forms.", pdf::MAX_XOBJECTS)),
            Refusal::Unreadable(why) => ("Could not show this file", why.clone()),
        };
        self.notice(theme, title, hint, cx)
    }

    /// The page shown ("3 / 12", a click to go to another), the zoom, and the file offered to its default app.
    fn toolbar(&self, theme: &Theme, shown: usize, total: usize, current: usize, scale: f32, cx: &mut Context<Self>) -> AnyElement {
        let indicator = match &self.goto {
            Some(digits) => div()
                .id("pdf-goto")
                .px(px(8.))
                .h(px(22.))
                .min_w(px(64.))
                .flex()
                .items_center()
                .rounded(px(5.))
                .border_1()
                .border_color(theme.text_muted)
                .text_color(theme.text)
                .child(format!("{digits}▏ / {shown}")),
            None => button(theme, "pdf-page", format!("{} / {shown}", current + 1)).tooltip(crate::shell::widgets::tip("Go to page", None)).on_click(cx.listener(
                |v, _: &ClickEvent, window, cx| {
                    v.goto = Some(String::new());
                    window.focus(&v.focus, cx);
                    cx.notify();
                },
            )),
        };
        let note = (total > shown).then(|| div().min_w_0().truncate().text_color(theme.text_faint).child(format!("Showing the first {shown} of {total} pages")));
        // as a share of the page's own size, a point a pixel at 96 per inch
        let percent = (scale * 72. / 96. * 100.).round();
        div()
            .h(px(32.))
            .flex_none()
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(4.))
            .border_b_1()
            .border_color(theme.border)
            .font_family(theme.font_ui.clone())
            .text_size(px(12.))
            .child(indicator)
            .children(note)
            .child(div().flex_1())
            .child(button(theme, "pdf-zoom-out", "−").tooltip(crate::shell::widgets::tip("Zoom out", None)).on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.zoom_to(v.zoom / ZOOM_STEP, None, cx))))
            .child(
                button(theme, "pdf-zoom-fit", format!("{percent}%"))
                    .min_w(px(52.))
                    .justify_center()
                    .tooltip(crate::shell::widgets::tip("Fit width", None))
                    .on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.zoom_to(1., None, cx))),
            )
            .child(button(theme, "pdf-zoom-in", "+").tooltip(crate::shell::widgets::tip("Zoom in", None)).on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.zoom_to(v.zoom * ZOOM_STEP, None, cx))))
            .child(div().w(px(4.)))
            .child(button(theme, "pdf-open", "Open externally").on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.open_externally(cx))))
            .into_any_element()
    }

    fn pages(&mut self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let State::Ready(doc) = &self.state else { return div().into_any_element() };
        let (pages, total) = (doc.pages.clone(), doc.total);
        let bounds = self.scroll.bounds();
        let measured = bounds.size.width > px(0.);
        let (vw, vh) = if measured { (f32::from(bounds.size.width), f32::from(bounds.size.height)) } else { (800., 600.) };
        let sizes: Vec<(f32, f32)> = pages.iter().map(|p| p.size).collect();
        let layout = Layout::new(&sizes, pdf::fit_width(&sizes, vw) * self.zoom, vw);
        let offset = self.scroll.offset();
        let (mut x, mut y) = (-f32::from(offset.x), -f32::from(offset.y));
        // the layout changed (a zoom, a resize): the point the zoom is about, else the top's middle, stays put
        if let Some(old) = self.layout.as_ref().filter(|old| old.scale != layout.scale || old.width != layout.width) {
            let (fx, fy) = self.zoom_at.take().unwrap_or((vw / 2., 0.));
            let (nx, ny) = layout.point(old.anchor(x + fx, y + fy));
            x = (nx - fx).clamp(0., (layout.width - vw).max(0.));
            y = (ny - fy).clamp(0., (layout.height - vh).max(0.));
            self.scroll.set_offset(point(px(-x), px(-y)));
        }
        let scale_factor = window.scale_factor();
        let visible = layout.visible(y, y + vh);
        let around = pdf::around(visible.clone(), 1, pages.len());
        if measured && !self.zooming {
            let order = visible.clone().chain(around.clone().filter(|i| !visible.contains(i)));
            self.request(&pages, order, layout.scale * scale_factor);
        }
        let current = if y <= 0. { 0 } else { layout.page_at(y + vh / 2.) };
        let elements: Vec<AnyElement> = pages
            .iter()
            .enumerate()
            .map(|(i, page)| {
                let [px_, py, w, h] = layout.rects[i];
                let image = around.contains(&i).then(|| self.cache.nearest(i, pdf::render_bucket(page.size, layout.scale * scale_factor).0)).flatten();
                let note = match page.problem {
                    Some(PageProblem::BadSize) => Some("This page's size is not valid".to_string()),
                    Some(PageProblem::TooManyPixels(pixels)) => Some(format!("This page's images are too large to show ({} MP)", pixels / 1_000_000)),
                    Some(PageProblem::TooDeep) => Some("This page is nested too deeply to show".into()),
                    Some(PageProblem::TooComplex) => Some("This page is too complex to show".into()),
                    None if self.failed.contains_key(&i) => self.failed.get(&i).map(|note| note.to_string()),
                    None => None,
                };
                div()
                    .absolute()
                    .left(px(px_))
                    .top(px(py))
                    .w(px(w))
                    .h(px(h))
                    .bg(gpui::white())
                    .shadow_sm()
                    .children(image.map(|(_, image)| gpui::img(image).size_full().object_fit(gpui::ObjectFit::Fill)))
                    .children(note.map(|note| {
                        div().size_full().p(px(12.)).flex().items_center().justify_center().text_size(px(12.)).text_color(gpui::black().opacity(0.55)).child(note)
                    }))
                    .into_any_element()
            })
            .collect();
        let toolbar = self.toolbar(theme, pages.len(), total, current, layout.scale, cx);
        let (content_w, content_h) = (layout.width, layout.height);
        self.layout = Some(layout);
        // Cmd/Ctrl with the wheel zooms about the pointer, before the pages scroll
        let this = cx.entity().downgrade();
        let wheel = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let this = this.clone();
                window.on_mouse_event(move |e: &ScrollWheelEvent, phase, _, cx| {
                    let primary = if cfg!(target_os = "macos") { e.modifiers.platform } else { e.modifiers.control };
                    if phase != DispatchPhase::Capture || !primary || !bounds.contains(&e.position) {
                        return;
                    }
                    let dy = f32::from(e.delta.pixel_delta(px(20.)).y);
                    let at = (f32::from(e.position.x - bounds.origin.x), f32::from(e.position.y - bounds.origin.y));
                    let _ = this.update(cx, |v, cx| v.zoom_to(v.zoom * (dy / 200.).exp(), Some(at), cx));
                    cx.stop_propagation();
                });
            },
        )
        .absolute()
        .size_full();
        // a viewport measured again (first frame, a resize) lays the pages out again on the next
        let scroll = self.scroll.clone();
        let laid_out_at = (vw, vh, measured);
        let remeasure = canvas(
            move |_, _, _| (),
            move |_, _, window, _| {
                let size = scroll.bounds().size;
                let now = (f32::from(size.width), f32::from(size.height), size.width > px(0.));
                if now.2 && now != laid_out_at {
                    window.request_animation_frame();
                }
            },
        )
        .absolute()
        .size_0();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .bg(theme.wash(0.04))
                    .child(
                        div()
                            .id("pdf-pages")
                            .size_full()
                            .overflow_scroll()
                            .track_scroll(&self.scroll)
                            .child(div().relative().w(px(content_w)).h(px(content_h)).children(elements)),
                    )
                    .child(wheel)
                    .child(remeasure)
                    .child(crate::ui::scrollbar("pdf-scrollbar", &self.scroll, theme)),
            )
            .into_any_element()
    }
}

/// A small text button, as the viewer's notices have.
fn button(theme: &Theme, id: &'static str, label: impl Into<SharedString>) -> gpui::Stateful<gpui::Div> {
    let hover = theme.hover;
    div()
        .id(id)
        .px(px(8.))
        .h(px(22.))
        .flex()
        .flex_none()
        .items_center()
        .rounded(px(5.))
        .border_1()
        .border_color(theme.border)
        .text_color(theme.text)
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .child(label.into())
}

impl Render for PdfView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for image in self.retired.drain(..) {
            let _ = window.drop_image(image);
        }
        // back in sight: the file is read again (a worker that was opening it went too)
        if self.worker.is_none() && matches!(self.state, State::Loading | State::Ready(_)) {
            self.start(cx);
        }
        let theme = &Theme::of(cx).clone();
        let body = match &self.state {
            State::Loading => div().into_any_element(),
            State::Ready(_) => self.pages(theme, window, cx),
            State::Refused(refusal) => {
                let refusal = refusal.clone();
                self.refused(theme, &refusal, cx)
            }
            State::NotResponding => self.notice(theme, "Not responding", "Reading this PDF is taking too long.", cx),
            State::CouldNotShow => self.notice(theme, "This PDF could not be shown", "Drawing its pages failed.", cx),
        };
        div()
            .id("pdf-view")
            .track_focus(&self.focus)
            .key_context(PDF_VIEW)
            .on_key_down(cx.listener(|v, e: &KeyDownEvent, _, cx| v.key(e, cx)))
            .on_action(cx.listener(|v, _: &PdfZoomIn, _, cx| v.zoom_to(v.zoom * ZOOM_STEP, None, cx)))
            .on_action(cx.listener(|v, _: &PdfZoomOut, _, cx| v.zoom_to(v.zoom / ZOOM_STEP, None, cx)))
            .on_action(cx.listener(|v, _: &PdfZoomReset, _, cx| v.zoom_to(1., None, cx)))
            .on_mouse_down(MouseButton::Left, cx.listener(|v, _, window, cx| window.focus(&v.focus, cx)))
            .size_full()
            .child(body)
    }
}
