//! A PDF tab's pages, without gpui: the engine that draws them (hayro, pure Rust, behind `PdfEngine` so another
//! can take its place), the budgets a file must meet before a page is drawn, the cache of drawn pages, and where the
//! pages sit in the view. The engine runs in the PDF worker process (`files::pdf_worker`), which holds the hard
//! limits (memory, time); the rest is plain arithmetic for the view.
//!
//! The budgets are a first line in front of those limits: the file is at most `MAX_PDF`, the first `MAX_PAGES` pages
//! show, each page's box is finite, non-zero and at most `MAX_PAGE_SIDE` points a side, the images a page draws are
//! at most `MAX_PAGE_IMAGE_PIXELS` by their `/Width` and `/Height` (or `/W` and `/H`) each time they are drawn
//! (through forms, tiling patterns, soft-mask groups and Type 3 glyphs, inline images too), forms nest at most
//! `MAX_NESTING` deep, a page makes at most `MAX_DRAWS` draws and runs at most `MAX_OPERATORS` operators (a form's
//! counted as often as it is drawn; a tiling pattern as often as its tiles cover the page, a Type 3 glyph as often as
//! its font's characters are shown), the scan inflates at most `MAX_STREAM_DECODED` of a stream and
//! `MAX_SCAN_DECODED` in all, the file has at most `MAX_XOBJECTS` images and forms, and a page is drawn at most
//! `MAX_RENDER_PIXELS`. A file that needs a password is refused. hayro builds the whole page tree as it opens a
//! file, so the page count is not capped before that: the worker's memory limit holds it.
use anyhow::{Result, anyhow, bail};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::content::TypedIter;
use hayro::hayro_syntax::content::ops::TypedInstruction;
use hayro::hayro_syntax::object::{Array, Dict, Name, Object, ObjectIdentifier, Stream};
use hayro::hayro_syntax::page::{Page, Resources};
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub use image::RgbaImage;

/// The largest PDF the viewer reads.
pub const MAX_PDF: u64 = 64 << 20;
/// The pages shown; a longer file shows these with a note.
pub const MAX_PAGES: usize = 200;
/// A page box's longest side in points: 200 inches, the largest page PDF itself allows (its user unit aside).
pub const MAX_PAGE_SIDE: f32 = 14_400.;
/// The pixels of the images one page draws, by their headers (32 MP is 128 MB decoded as RGBA).
pub const MAX_PAGE_IMAGE_PIXELS: u64 = 32_000_000;
/// How deep forms may nest inside forms.
pub const MAX_NESTING: usize = 16;
/// The images and forms a file may hold.
pub const MAX_XOBJECTS: usize = 10_000;
/// The images and forms a page draws, a form's own draws counted as often as the form is drawn.
pub const MAX_DRAWS: u64 = 100_000;
/// The operators a page runs, a form's counted as often as it is drawn.
pub const MAX_OPERATORS: u64 = 5_000_000;
/// The forms, patterns and glyphs the scan of one page enters.
const MAX_VISITS: u64 = 100_000;
/// The bytes the scan inflates of one stream, and of the whole file: past them the pages left are too complex.
pub const MAX_STREAM_DECODED: usize = 64 << 20;
pub const MAX_SCAN_DECODED: usize = 256 << 20;
/// The most pixels a drawn page has (16 MP is 64 MB of RGBA), and its longest side; a larger zoom draws the page
/// at this and stretches it.
pub const MAX_RENDER_PIXELS: f32 = 16_000_000.;
pub const MAX_RENDER_SIDE: f32 = 8192.;
/// The drawn pages kept, in bytes of RGBA.
pub const CACHE_BYTES: usize = 128 << 20;
/// Zoom buckets per doubling of the scale: a page is drawn at its bucket's scale (at most 9% sharper than asked) and
/// kept under it, so a zoom that moves a little reuses what is drawn.
const BUCKETS_PER_OCTAVE: f32 = 8.;

/// What draws a PDF's pages. Made and used on one thread (the view's worker): it need not be `Send`.
pub trait PdfEngine {
    /// The pages in the file, all of them (the view shows up to `MAX_PAGES`).
    fn pages(&self) -> usize;
    /// Page `i`'s size in points, rotation applied.
    fn page_size(&self, i: usize) -> (f32, f32);
    /// Page `i` drawn on white at `scale` pixels per point (clamped by `clamp_scale`); `cancel` set before or while
    /// it draws drops it.
    fn render(&self, i: usize, scale: f32, cancel: &AtomicBool) -> Result<RgbaImage>;
}

/// Why a PDF is not shown at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Its size in bytes, over `MAX_PDF`.
    TooLarge(u64),
    /// It needs a password, or its encryption is one hayro does not read.
    Encrypted,
    /// It is not a PDF hayro can read.
    Broken,
    /// It has no pages.
    Empty,
    /// More than `MAX_XOBJECTS` images and forms.
    TooManyObjects,
    /// It could not be read from the disk: why, in words.
    Unreadable(String),
}

/// Why one page is not drawn: its place shows the note instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageProblem {
    /// Its box is not finite, empty, or over `MAX_PAGE_SIDE`.
    BadSize,
    /// Its images' pixels, over `MAX_PAGE_IMAGE_PIXELS`.
    TooManyPixels(u64),
    /// Its forms nest past `MAX_NESTING`, or in a loop.
    TooDeep,
    /// Over `MAX_DRAWS` draws or `MAX_OPERATORS` operators.
    TooComplex,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageInfo {
    /// Its size in points, rotation applied (a page with a bad box takes a Letter page's place).
    pub size: (f32, f32),
    pub problem: Option<PageProblem>,
}

/// A PDF as the view lays it out: its first `MAX_PAGES` pages, and how many it has in all.
#[derive(Debug, Clone, PartialEq)]
pub struct Doc {
    pub pages: Vec<PageInfo>,
    pub total: usize,
}

/// A US Letter page, in points: where a page with a bad box goes.
const LETTER: (f32, f32) = (612., 792.);

/// Reads `bytes` as a PDF and checks it against the budgets, page by page: the engine to draw it with, and its
/// pages. Runs on the worker; a panic in the parser is a broken file.
pub fn open(bytes: Vec<u8>) -> Result<(Box<dyn PdfEngine>, Doc), Refusal> {
    if bytes.len() as u64 > MAX_PDF {
        return Err(Refusal::TooLarge(bytes.len() as u64));
    }
    let opened = catch_unwind(AssertUnwindSafe(|| -> Result<(HayroEngine, Doc), Refusal> {
        let pdf = match Pdf::new(Arc::new(bytes)) {
            Ok(pdf) => pdf,
            Err(LoadPdfError::Decryption(_)) => return Err(Refusal::Encrypted),
            Err(LoadPdfError::Invalid) => return Err(Refusal::Broken),
        };
        let doc = scan(&pdf)?;
        Ok((HayroEngine { pdf, settings: InterpreterSettings::default() }, doc))
    }));
    let (engine, doc) = opened.map_err(|_| Refusal::Broken)??;
    Ok((Box::new(engine), doc))
}

/// The budgets, read off the file before any page is drawn: the images and forms it holds in all, then each shown
/// page's box, and what its content draws.
fn scan(pdf: &Pdf) -> Result<Doc, Refusal> {
    scan_with(pdf, &mut Scan::default())
}

fn scan_with(pdf: &Pdf, scan: &mut Scan) -> Result<Doc, Refusal> {
    let pages = pdf.pages();
    if pages.is_empty() {
        return Err(Refusal::Empty);
    }
    let mut xobjects = 0;
    for object in pdf.objects() {
        if let Object::Stream(stream) = object
            && (is(stream.dict(), b"Subtype", b"Image") || is(stream.dict(), b"Subtype", b"Form"))
        {
            xobjects += 1;
            if xobjects > MAX_XOBJECTS {
                return Err(Refusal::TooManyObjects);
            }
        }
    }
    let shown = pages
        .iter()
        .take(MAX_PAGES)
        .map(|page| {
            let crop = page.intersected_crop_box();
            let (w, h) = (crop.width() as f32, crop.height() as f32);
            if !page_box_ok(w, h) {
                return PageInfo { size: LETTER, problem: Some(PageProblem::BadSize) };
            }
            let size = page.render_dimensions();
            let mut walk = Walk { area: f64::from(w) * f64::from(h), ..Walk::default() };
            let problem = match scan.page_content(page).and_then(|content| walk.content(scan, &content, page.resources(), 1, 0, true)) {
                Err(problem) => Some(problem),
                Ok(()) if walk.pixels > MAX_PAGE_IMAGE_PIXELS => Some(PageProblem::TooManyPixels(walk.pixels)),
                Ok(()) => None,
            };
            PageInfo { size, problem }
        })
        .collect();
    Ok(Doc { pages: shown, total: pages.len() })
}

/// A page box's sides (points) as the budget takes them: finite, non-zero and at most `MAX_PAGE_SIDE`.
pub fn page_box_ok(w: f32, h: f32) -> bool {
    [w, h].iter().all(|s| s.is_finite() && *s >= 1. && *s <= MAX_PAGE_SIDE)
}

fn is(dict: &Dict<'_>, key: &[u8], value: &[u8]) -> bool {
    dict.get::<Name<'_>>(key).is_some_and(|n| n.as_ref() == value)
}

/// What one content stream does, read once: its operators, its inline images' pixels, how often it draws each
/// named image or form, and how many characters it shows in each named font.
#[derive(Default)]
struct Summary {
    operators: u64,
    inline_pixels: u64,
    draws: Vec<(Vec<u8>, u64)>,
    shown: Vec<(Vec<u8>, u64)>,
}

impl Summary {
    /// Reads `data` as far as `MAX_OPERATORS`: past that the page is over budget anyway.
    fn of(data: &[u8]) -> Summary {
        let mut summary = Summary::default();
        let mut draws: HashMap<Vec<u8>, u64> = HashMap::new();
        let mut shown: HashMap<Vec<u8>, u64> = HashMap::new();
        let mut font: Vec<u8> = Vec::new();
        let mut ops = TypedIter::new(data);
        while let Some(op) = ops.next() {
            summary.operators += 1;
            if summary.operators > MAX_OPERATORS {
                break;
            }
            let chars = match op {
                TypedInstruction::XObject(x) => {
                    *draws.entry(x.0.as_ref().to_vec()).or_default() += 1;
                    0
                }
                TypedInstruction::InlineImage(image) => {
                    summary.inline_pixels = summary.inline_pixels.saturating_add(pixels(image.0.dict()));
                    0
                }
                TypedInstruction::TextFont(f) => {
                    font = f.0.as_ref().to_vec();
                    0
                }
                TypedInstruction::ShowText(s) => s.0.as_bytes().len(),
                TypedInstruction::NextLineAndShowText(s) => s.0.as_bytes().len(),
                TypedInstruction::ShowTextWithParameters(s) => s.2.as_bytes().len(),
                // the strings among the kerning numbers
                TypedInstruction::ShowTexts(parts) => parts.0.iter::<Object<'_>>().map(|o| if let Object::String(s) = o { s.as_bytes().len() } else { 0 }).sum(),
                _ => 0,
            };
            if chars > 0 {
                *shown.entry(font.clone()).or_default() += chars as u64;
            }
        }
        summary.draws = draws.into_iter().collect();
        summary.shown = shown.into_iter().collect();
        summary
    }
}

/// An image's pixels by its header: `/Width` and `/Height`, or an inline image's `/W` and `/H`.
fn pixels(dict: &Dict<'_>) -> u64 {
    let side = |long: &[u8], short: &[u8]| {
        dict.get::<f64>(long).or_else(|| dict.get::<f64>(short)).filter(|v| v.is_finite() && *v > 0.).map_or(0, |v| v.min(u32::MAX as f64) as u64)
    };
    side(b"Width", b"W").saturating_mul(side(b"Height", b"H"))
}

/// What the scan keeps for the whole file: each stream's summary by object id (a form, pattern or glyph shared by
/// many pages is read once), and the bytes it has inflated. The inflated bytes themselves are dropped once summed
/// up: hayro's own page content cache is not used, so the scan leaves little behind.
#[derive(Default)]
struct Scan {
    summaries: HashMap<ObjectIdentifier, Rc<Summary>>,
    decoded: usize,
    /// Streams inflated, for the tests.
    decodes: usize,
}

impl Scan {
    /// `stream` inflated and summed up, within `MAX_STREAM_DECODED` and what is left of `MAX_SCAN_DECODED`; a stream
    /// that does not inflate is empty.
    fn decode(&mut self, stream: &Stream<'_>) -> Result<Summary, PageProblem> {
        if self.decoded > MAX_SCAN_DECODED {
            return Err(PageProblem::TooComplex);
        }
        let Ok(data) = stream.decoded() else { return Ok(Summary::default()) };
        self.decodes += 1;
        self.decoded = self.decoded.saturating_add(data.len());
        if data.len() > MAX_STREAM_DECODED || self.decoded > MAX_SCAN_DECODED {
            return Err(PageProblem::TooComplex);
        }
        Ok(Summary::of(&data))
    }

    fn summary(&mut self, stream: &Stream<'_>) -> Result<Rc<Summary>, PageProblem> {
        let id = stream.obj_id();
        if let Some(summary) = self.summaries.get(&id) {
            return Ok(summary.clone());
        }
        let summary = Rc::new(self.decode(stream)?);
        // a direct stream has no id of its own to be found by again
        if id != ObjectIdentifier::new(0, 0) {
            self.summaries.insert(id, summary.clone());
        }
        Ok(summary)
    }

    /// A page's content streams, read from its `/Contents` as one (operators may run from one into the next).
    fn page_content(&mut self, page: &Page<'_>) -> Result<Summary, PageProblem> {
        let raw = page.raw();
        let streams: Vec<Stream<'_>> = match raw.get::<Stream<'_>>(b"Contents") {
            Some(stream) => vec![stream],
            None => raw.get::<Array<'_>>(b"Contents").map(|a| a.iter::<Stream<'_>>().collect()).unwrap_or_default(),
        };
        let mut joined = Vec::new();
        for stream in streams {
            if self.decoded > MAX_SCAN_DECODED {
                return Err(PageProblem::TooComplex);
            }
            let Ok(data) = stream.decoded() else { continue };
            self.decodes += 1;
            self.decoded = self.decoded.saturating_add(data.len());
            if data.len() > MAX_STREAM_DECODED || self.decoded > MAX_SCAN_DECODED {
                return Err(PageProblem::TooComplex);
            }
            joined.extend_from_slice(&data);
            joined.push(b' ');
        }
        Ok(Summary::of(&joined))
    }
}

/// One page's drawing, walked from its content: every image's pixels each time it is drawn (its soft mask's too),
/// every form entered as often as it is drawn (its draws and operators multiplied), every tiling pattern as often as
/// its tiles cover the page, every Type 3 glyph as often as its font's characters are shown, and soft-mask groups
/// once per use of their resources. A form inside itself is a loop.
#[derive(Default)]
struct Walk {
    /// The page's area in points, which a tiling pattern's tiles are counted over.
    area: f64,
    pixels: u64,
    draws: u64,
    operators: u64,
    visits: u64,
    open: Vec<ObjectIdentifier>,
}

impl Walk {
    /// A content stream drawn `weight` times at `depth`, with `resources`; `extras` walks the patterns and soft
    /// masks in them (not again for a form that took its parent's).
    fn content(&mut self, scan: &mut Scan, summary: &Summary, resources: &Resources<'_>, weight: u64, depth: usize, extras: bool) -> Result<(), PageProblem> {
        self.operators = self.operators.saturating_add(summary.operators.saturating_mul(weight));
        if self.operators > MAX_OPERATORS {
            return Err(PageProblem::TooComplex);
        }
        self.pixels = self.pixels.saturating_add(summary.inline_pixels.saturating_mul(weight));
        for (name, count) in &summary.draws {
            let weight = weight.saturating_mul(*count);
            self.draws = self.draws.saturating_add(weight);
            if self.draws > MAX_DRAWS {
                return Err(PageProblem::TooComplex);
            }
            // a form drawing one it is inside (hayro will not even resolve one inside itself)
            if resources.x_objects.get_ref(name).is_some_and(|r| self.open.contains(&r.into())) {
                return Err(PageProblem::TooDeep);
            }
            let Some(stream) = resources.x_objects.get::<Stream<'_>>(name) else { continue };
            if is(stream.dict(), b"Subtype", b"Image") {
                self.image(stream.dict(), weight);
            } else if is(stream.dict(), b"Subtype", b"Form") {
                self.form(scan, &stream, resources, weight, depth)?;
            }
        }
        self.glyphs(scan, summary, resources, weight, depth)?;
        if extras { self.extras(scan, resources, weight, depth) } else { Ok(()) }
    }

    fn image(&mut self, dict: &Dict<'_>, weight: u64) {
        let mut each = pixels(dict);
        if let Some(mask) = dict.get::<Stream<'_>>(b"SMask") {
            each = each.saturating_add(pixels(mask.dict()));
        }
        self.pixels = self.pixels.saturating_add(each.saturating_mul(weight));
    }

    /// A form (or a pattern, a soft-mask group, a glyph: content streams with resources) drawn `weight` times inside
    /// content at `depth`, which takes its parent's resources when it has none of its own.
    fn form(&mut self, scan: &mut Scan, stream: &Stream<'_>, parent: &Resources<'_>, weight: u64, depth: usize) -> Result<(), PageProblem> {
        let id = stream.obj_id();
        if self.open.contains(&id) || depth + 1 > MAX_NESTING {
            return Err(PageProblem::TooDeep);
        }
        self.visits += 1;
        if self.visits > MAX_VISITS {
            return Err(PageProblem::TooComplex);
        }
        let summary = scan.summary(stream)?;
        let own = stream.dict().get::<Dict<'_>>(b"Resources").map(Resources::new);
        self.open.push(id);
        let walked = self.content(scan, &summary, own.as_ref().unwrap_or(parent), weight, depth + 1, own.is_some());
        self.open.pop();
        walked
    }

    /// The Type 3 fonts `summary` shows characters in: each glyph of the font as often as the font's characters
    /// are shown (any of them may be any glyph), with the font's resources, else these.
    fn glyphs(&mut self, scan: &mut Scan, summary: &Summary, resources: &Resources<'_>, weight: u64, depth: usize) -> Result<(), PageProblem> {
        for (name, chars) in &summary.shown {
            let Some(font) = resources.fonts.get::<Dict<'_>>(name).filter(|f| is(f, b"Subtype", b"Type3")) else { continue };
            let Some(procs) = font.get::<Dict<'_>>(b"CharProcs") else { continue };
            let own = font.get::<Dict<'_>>(b"Resources").map(Resources::new);
            let weight = weight.saturating_mul(*chars);
            let names: Vec<Name<'_>> = procs.keys().collect();
            for glyph in names {
                if let Some(glyph) = procs.get::<Stream<'_>>(glyph.as_ref()) {
                    self.form(scan, &glyph, own.as_ref().unwrap_or(resources), weight, depth)?;
                }
            }
        }
        Ok(())
    }

    /// What `resources` draw besides their XObjects and glyphs: tiling patterns (type 1, content streams), each as
    /// often as its tiles cover the page, and the groups of soft masks in graphics states.
    fn extras(&mut self, scan: &mut Scan, resources: &Resources<'_>, weight: u64, depth: usize) -> Result<(), PageProblem> {
        let names: Vec<Name<'_>> = resources.patterns.keys().collect();
        for name in names {
            if let Some(pattern) = resources.patterns.get::<Stream<'_>>(name.as_ref())
                && pattern.dict().get::<u8>(b"PatternType") == Some(1)
            {
                let tiles = tiles(pattern.dict(), self.area);
                self.form(scan, &pattern, resources, weight.saturating_mul(tiles), depth)?;
            }
        }
        let names: Vec<Name<'_>> = resources.ext_g_states.keys().collect();
        for name in names {
            let group = resources.ext_g_states.get::<Dict<'_>>(name.as_ref()).and_then(|gs| gs.get::<Dict<'_>>(b"SMask")).and_then(|mask| mask.get::<Stream<'_>>(b"G"));
            if let Some(group) = group {
                self.form(scan, &group, resources, weight, depth)?;
            }
        }
        Ok(())
    }
}

/// How many tiles of a tiling pattern (`/XStep` by `/YStep`, through its `/Matrix`) cover `area` points: at least
/// one; a tile under a point square counts as one.
fn tiles(pattern: &Dict<'_>, area: f64) -> u64 {
    let step = |key: &[u8]| pattern.get::<f64>(key).filter(|v| v.is_finite()).map_or(1., f64::abs);
    let det = pattern
        .get::<Array<'_>>(b"Matrix")
        .map(|m| m.iter::<f64>().take(4).collect::<Vec<f64>>())
        .filter(|m| m.len() == 4 && m.iter().all(|v| v.is_finite()))
        .map_or(1., |m| (m[0] * m[3] - m[1] * m[2]).abs());
    let tile = (step(b"XStep") * step(b"YStep") * det).max(1.);
    (area / tile).ceil().clamp(1., 1e15) as u64
}

/// hayro's engine. A render cache is made for each page (a shared one saves under a tenth of the time, and would
/// borrow from the file it is kept beside).
struct HayroEngine {
    pdf: Pdf,
    settings: InterpreterSettings,
}

impl PdfEngine for HayroEngine {
    fn pages(&self) -> usize {
        self.pdf.pages().len()
    }

    fn page_size(&self, i: usize) -> (f32, f32) {
        self.pdf.pages().get(i).map_or(LETTER, |p| p.render_dimensions())
    }

    fn render(&self, i: usize, scale: f32, cancel: &AtomicBool) -> Result<RgbaImage> {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        let page = self.pdf.pages().get(i).ok_or_else(|| anyhow!("no page {i}"))?;
        let scale = clamp_scale(page.render_dimensions(), scale);
        let pixmap = catch_unwind(AssertUnwindSafe(|| {
            let cache = RenderCache::new();
            let pixmap = PixmapSettings { x_scale: scale, y_scale: scale, bg_color: WHITE };
            hayro::render(page, &cache, &self.settings, &RenderSettings::default(), &pixmap)
        }))
        .map_err(|_| anyhow!("page {i} could not be drawn"))?;
        // hayro has no way to stop a page midway: one cancelled while drawing is dropped here
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        let (w, h) = (u32::from(pixmap.width()), u32::from(pixmap.height()));
        // drawn on white, so every pixel is opaque and premultiplied is straight
        let rgba = pixmap.take_rgba8(hayro::vello_cpu::peniko::ImageAlphaType::AlphaPremultiplied);
        RgbaImage::from_raw(w, h, rgba).ok_or_else(|| anyhow!("page {i} came out the wrong size"))
    }
}

/// `scale` (pixels per point) as far as a page of `size` points may be drawn: at most `MAX_RENDER_PIXELS` and
/// `MAX_RENDER_SIDE` a side, and at least a pixel a side.
pub fn clamp_scale(size: (f32, f32), scale: f32) -> f32 {
    let (w, h) = (size.0.max(1.), size.1.max(1.));
    let scale = if scale.is_finite() { scale.max(0.) } else { 1. };
    let cap = (MAX_RENDER_PIXELS / (w * h)).sqrt().min(MAX_RENDER_SIDE / w.max(h));
    scale.min(cap).max(1. / w.min(h))
}

/// The zoom bucket `scale` falls in, rounded up.
pub fn bucket(scale: f32) -> i32 {
    (scale.max(1e-3).log2() * BUCKETS_PER_OCTAVE).ceil() as i32
}

pub fn bucket_scale(bucket: i32) -> f32 {
    (bucket as f32 / BUCKETS_PER_OCTAVE).exp2()
}

/// The bucket a page of `size` points is drawn in at `scale`, and the scale it is drawn at: `scale`'s bucket, or
/// the largest the page's budget allows (`clamp_scale`).
pub fn render_bucket(size: (f32, f32), scale: f32) -> (i32, f32) {
    let wanted = bucket(scale);
    let cap = clamp_scale(size, f32::MAX);
    if bucket_scale(wanted) <= cap {
        (wanted, bucket_scale(wanted))
    } else {
        // the cap's own bucket, rounded down so its scale stays within the budget
        let b = (cap.log2() * BUCKETS_PER_OCTAVE).floor() as i32;
        (b, bucket_scale(b).min(cap))
    }
}

/// The drawn pages, keyed by (page, zoom bucket), least recently used going first past the budget in bytes; the
/// pages the view wants now never go (the budget grows to hold them, `fit`).
pub struct Cache<T> {
    floor: usize,
    budget: usize,
    used: usize,
    tick: u64,
    entries: HashMap<(usize, i32), Entry<T>>,
}

struct Entry<T> {
    value: T,
    bytes: usize,
    used_at: u64,
}

impl<T: Clone> Cache<T> {
    pub fn new(budget: usize) -> Cache<T> {
        Cache { floor: budget, budget, used: 0, tick: 0, entries: HashMap::new() }
    }

    /// The budget, made the larger of the one it was made with and `wanted` bytes (the pages the view wants now, as
    /// drawn).
    pub fn fit(&mut self, wanted: usize) {
        self.budget = self.floor.max(wanted);
    }

    pub fn budget(&self) -> usize {
        self.budget
    }

    pub fn used(&self) -> usize {
        self.used
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, key: (usize, i32)) -> bool {
        self.entries.contains_key(&key)
    }

    /// Page `page` as drawn in `bucket`, else in the nearest bucket it is kept in (a zoom shows the old drawing
    /// stretched until the new one lands), and the bucket it is in; counts as a use.
    pub fn nearest(&mut self, page: usize, bucket: i32) -> Option<(i32, T)> {
        self.tick += 1;
        let tick = self.tick;
        let key = self.entries.keys().filter(|(p, _)| *p == page).min_by_key(|(_, b)| (b - bucket).abs())?.to_owned();
        let entry = self.entries.get_mut(&key)?;
        entry.used_at = tick;
        Some((key.1, entry.value.clone()))
    }

    /// Keeps `value` (`bytes` of it) under `key`; returns what went to make room, and what it replaced. Nothing in
    /// `wanted` goes to make room, and a wanted value is kept even past the budget; one not wanted and over the whole
    /// budget is not kept: it is returned.
    pub fn insert(&mut self, key: (usize, i32), value: T, bytes: usize, wanted: &HashSet<(usize, i32)>) -> Vec<T> {
        let mut gone = Vec::new();
        if bytes > self.budget && !wanted.contains(&key) {
            gone.push(value);
            return gone;
        }
        if let Some(old) = self.entries.remove(&key) {
            self.used -= old.bytes;
            gone.push(old.value);
        }
        while self.used + bytes > self.budget {
            let Some(oldest) = self.entries.iter().filter(|(k, _)| !wanted.contains(k)).min_by_key(|(_, e)| e.used_at).map(|(k, _)| *k) else { break };
            let entry = self.entries.remove(&oldest).expect("listed");
            self.used -= entry.bytes;
            gone.push(entry.value);
        }
        self.tick += 1;
        self.used += bytes;
        self.entries.insert(key, Entry { value, bytes, used_at: self.tick });
        gone
    }

    /// Everything kept, let go.
    pub fn clear(&mut self) -> Vec<T> {
        self.used = 0;
        self.entries.drain().map(|(_, e)| e.value).collect()
    }
}

/// Space around the pages and between them, in logical pixels.
pub const PAD: f32 = 16.;
pub const GAP: f32 = 12.;

/// The scale (logical pixels per point) that fits the widest of `pages` (points) into `width` with `PAD` each side.
pub fn fit_width(pages: &[(f32, f32)], width: f32) -> f32 {
    let widest = pages.iter().map(|p| p.0).fold(1., f32::max);
    ((width - PAD * 2.).max(40.) / widest).max(1e-3)
}

/// Where each page sits at a scale, in logical pixels of the scrolled content: stacked, `GAP` apart, each centred
/// in the wider of the viewport and the widest page.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub scale: f32,
    /// (x, y, width, height) of each page.
    pub rects: Vec<[f32; 4]>,
    pub width: f32,
    pub height: f32,
}

/// A point in the content as a page, and where in it (fractions of its width and height): what stays put when the
/// layout changes (a zoom, a resize).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    pub page: usize,
    pub fx: f32,
    pub fy: f32,
}

impl Layout {
    pub fn new(pages: &[(f32, f32)], scale: f32, viewport_width: f32) -> Layout {
        let widest = pages.iter().map(|p| p.0 * scale).fold(0., f32::max);
        let width = (widest + PAD * 2.).max(viewport_width);
        let mut y = PAD;
        let rects = pages
            .iter()
            .map(|&(w, h)| {
                let (w, h) = (w * scale, h * scale);
                let rect = [(width - w) / 2., y, w, h];
                y += h + GAP;
                rect
            })
            .collect();
        let height = if pages.is_empty() { PAD * 2. } else { y - GAP + PAD };
        Layout { scale, rects, width, height }
    }

    /// The pages any part of which is between `top` and `bottom` (content pixels).
    pub fn visible(&self, top: f32, bottom: f32) -> Range<usize> {
        let start = self.rects.partition_point(|r| r[1] + r[3] < top);
        let end = self.rects.partition_point(|r| r[1] <= bottom);
        start..end.max(start)
    }

    /// The page at height `y` (content pixels), or the nearest one.
    pub fn page_at(&self, y: f32) -> usize {
        self.rects.partition_point(|r| r[1] + r[3] + GAP / 2. < y).min(self.rects.len().saturating_sub(1))
    }

    pub fn anchor(&self, x: f32, y: f32) -> Anchor {
        let page = self.page_at(y);
        let Some(&[px, py, w, h]) = self.rects.get(page) else { return Anchor { page: 0, fx: 0., fy: 0. } };
        Anchor { page, fx: (x - px) / w.max(1.), fy: (y - py) / h.max(1.) }
    }

    pub fn point(&self, anchor: Anchor) -> (f32, f32) {
        let Some(&[px, py, w, h]) = self.rects.get(anchor.page.min(self.rects.len().saturating_sub(1))) else { return (0., 0.) };
        (px + anchor.fx * w, py + anchor.fy * h)
    }
}

/// `range` and `margin` more each side, within `0..len`.
pub fn around(range: Range<usize>, margin: usize, len: usize) -> Range<usize> {
    range.start.saturating_sub(margin).min(len)..(range.end + margin).min(len)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A PDF of `objects` (object 1 the catalog), with the cross-reference table worked out and `trailer` added to
    /// the trailer.
    fn pdf(objects: &[String], trailer: &str) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for o in offsets {
            out.extend(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend(format!("trailer\n<< /Size {} /Root 1 0 R {trailer} >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
        out
    }

    fn stream(dict: &str, data: &str) -> String {
        format!("<< {dict} /Length {} >>\nstream\n{data}\nendstream", data.len())
    }

    /// A one-page PDF (objects 1–4: catalog, pages, page, contents) whose page has `media` box and `resources`;
    /// `more` are objects 5 on.
    /// A one-page PDF with `media` box drawing `content`, for the worker's tests.
    pub(crate) fn one_page_pdf(media: &str, content: &str) -> Vec<u8> {
        one_page(media, "", content, &[])
    }

    fn one_page(media: &str, resources: &str, content: &str, more: &[String]) -> Vec<u8> {
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [{media}] /Resources << {resources} >> /Contents 4 0 R >>"),
            stream("", content),
        ];
        objects.extend(more.iter().cloned());
        pdf(&objects, "")
    }

    fn image(w: u32, h: u32) -> String {
        stream(&format!("/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceGray /BitsPerComponent 8"), "")
    }

    fn doc(bytes: Vec<u8>) -> Result<Doc, Refusal> {
        open(bytes).map(|(_, doc)| doc)
    }

    #[test]
    fn a_page_draws_at_its_scale_on_white() {
        let bytes = one_page("0 0 100 50", "", "1 0 0 rg 0 0 50 50 re f", &[]);
        let (engine, doc) = open(bytes).unwrap();
        assert_eq!(engine.pages(), 1);
        assert_eq!(engine.page_size(0), (100., 50.));
        assert_eq!(doc, Doc { pages: vec![PageInfo { size: (100., 50.), problem: None }], total: 1 });
        let image = engine.render(0, 2., &AtomicBool::new(false)).unwrap();
        assert_eq!(image.dimensions(), (200, 100));
        // the red square is the left half (PDF's y goes up, so the bottom left), the rest is the white page
        assert_eq!(image.get_pixel(50, 50).0, [255, 0, 0, 255]);
        assert_eq!(image.get_pixel(150, 50).0, [255, 255, 255, 255]);
        assert!(engine.render(0, 2., &AtomicBool::new(true)).is_err(), "cancelled before it starts");
        assert!(engine.render(1, 2., &AtomicBool::new(false)).is_err());
    }

    fn form(resources: &str, content: &str) -> String {
        stream(&format!("/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources << {resources} >>"), content)
    }

    #[test]
    fn a_page_s_images_count_each_time_they_are_drawn() {
        // the page draws image 5 and form 6; the form draws image 5 again and image 7
        let fm = form("/XObject << /A 5 0 R /B 7 0 R >>", "/A Do /B Do");
        let bytes = one_page("0 0 612 792", "/XObject << /Im 5 0 R /Fm 6 0 R >>", "/Im Do /Fm Do", &[image(3000, 3000), fm, image(3000, 1000)]);
        assert_eq!(doc(bytes).unwrap().pages[0].problem, None, "21 MP");
        let fm = form("/XObject << /A 5 0 R /B 7 0 R >>", "/A Do /B Do");
        let bytes = one_page("0 0 612 792", "/XObject << /Im 5 0 R /Fm 6 0 R >>", "/Im Do /Fm Do", &[image(4000, 3000), fm, image(4000, 2001)]);
        // image 5 twice (12 MP each) and image 7 (8 MP and a row)
        assert_eq!(doc(bytes).unwrap().pages[0].problem, Some(PageProblem::TooManyPixels(32_004_000)));
        // an image in the resources that nothing draws is not counted
        let bytes = one_page("0 0 612 792", "/XObject << /Im 5 0 R >>", "", &[image(9000, 9000)]);
        assert_eq!(doc(bytes).unwrap().pages[0].problem, None);
    }

    #[test]
    fn a_form_drawn_again_and_again_counts_every_time() {
        // a 1 MP image in a form the page draws 40 times
        let fm = form("/XObject << /I 6 0 R >>", "/I Do");
        let content = "/Fm Do ".repeat(40);
        let bytes = one_page("0 0 612 792", "/XObject << /Fm 5 0 R >>", &content, &[fm, image(1000, 1000)]);
        assert_eq!(doc(bytes).unwrap().pages[0].problem, Some(PageProblem::TooManyPixels(40_000_000)));
        // a form that draws another 1000 times, drawn 1000 times: a million draws
        let outer = form("/XObject << /B 6 0 R >>", &"/B Do ".repeat(1000));
        let inner = form("", "0 0 1 1 re f");
        let bytes = one_page("0 0 612 792", "/XObject << /A 5 0 R >>", &"/A Do ".repeat(1000), &[outer, inner]);
        assert_eq!(doc(bytes).unwrap().pages[0].problem, Some(PageProblem::TooComplex));
    }

    #[test]
    fn short_keys_and_inline_images_count() {
        let short = stream("/Type /XObject /Subtype /Image /W 6000 /H 6000 /ColorSpace /DeviceGray /BitsPerComponent 8", "");
        let bytes = one_page("0 0 612 792", "/XObject << /Im 5 0 R >>", "/Im Do", &[short]);
        assert_eq!(doc(bytes).unwrap().pages[0].problem, Some(PageProblem::TooManyPixels(36_000_000)));
        let inline = "q BI /W 6000 /H 6000 /CS /G /BPC 8 ID x EI Q";
        let bytes = one_page("0 0 612 792", "", inline, &[]);
        assert_eq!(doc(bytes).unwrap().pages[0].problem, Some(PageProblem::TooManyPixels(36_000_000)));
        let small = "q BI /W 10 /H 10 /CS /G /BPC 8 ID x EI Q";
        assert_eq!(doc(one_page("0 0 612 792", "", small, &[])).unwrap().pages[0].problem, None);
    }

    #[test]
    fn patterns_soft_masks_and_type3_glyphs_are_walked() {
        // a tiling pattern (one tile the page's size) drawing a 36 MP image
        let pattern = stream("/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10] /XStep 612 /YStep 792 /Resources << /XObject << /I 6 0 R >> >>", "/I Do");
        let bytes = one_page("0 0 612 792", "/Pattern << /P 5 0 R >>", "", &[pattern, image(6000, 6000)]);
        assert_eq!(doc(bytes).unwrap().pages[0].problem, Some(PageProblem::TooManyPixels(36_000_000)));
        // a soft mask's group drawing it
        let group = form("/XObject << /I 6 0 R >>", "/I Do");
        let bytes = one_page("0 0 612 792", "/ExtGState << /G << /SMask << /S /Luminosity /G 5 0 R >> >> >>", "", &[group, image(6000, 6000)]);
        assert_eq!(doc(bytes).unwrap().pages[0].problem, Some(PageProblem::TooManyPixels(36_000_000)));
        // a Type 3 glyph drawing it, with the font's resources
        let font = "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1 1] /FontMatrix [1 0 0 1 0 0] /CharProcs << /a 6 0 R >> /Encoding << /Differences [97 /a] >> /FirstChar 97 /LastChar 97 /Widths [1] /Resources << /XObject << /I 7 0 R >> >> >>".to_string();
        let bytes = one_page("0 0 612 792", "/Font << /T3 5 0 R >>", "BT /T3 1 Tf (a) Tj ET", &[font.clone(), stream("", "1 0 d0 /I Do"), image(6000, 6000)]);
        assert_eq!(doc(bytes).unwrap().pages[0].problem, Some(PageProblem::TooManyPixels(36_000_000)));
        // a Type 3 font nothing is shown in draws nothing
        let bytes = one_page("0 0 612 792", "/Font << /T3 5 0 R >>", "", &[font, stream("", "1 0 d0 /I Do"), image(6000, 6000)]);
        assert_eq!(doc(bytes).unwrap().pages[0].problem, None);
    }

    #[test]
    fn a_tiling_pattern_counts_as_often_as_its_tiles_cover_the_page() {
        // 10 × 10 point tiles of a 1 MP image over a Letter page: 4848 of them
        let tiled = |step: &str| {
            let pattern = stream(&format!("/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10] {step} /Resources << /XObject << /I 6 0 R >> >>"), "/I Do");
            one_page("0 0 612 792", "/Pattern << /P 5 0 R >>", "", &[pattern, image(100, 100)])
        };
        assert_eq!(doc(tiled("/XStep 10 /YStep 10")).unwrap().pages[0].problem, Some(PageProblem::TooManyPixels(48_480_000)));
        assert_eq!(doc(tiled("/XStep 612 /YStep 792")).unwrap().pages[0].problem, None, "one tile");
        // the pattern's matrix scales its tiles
        assert_eq!(doc(tiled("/XStep 10 /YStep 10 /Matrix [10 0 0 10 0 0]")).unwrap().pages[0].problem, None, "49 tiles");
        assert_eq!(tiles(&Dict::empty(), 100.), 100, "no steps: point tiles");
    }

    #[test]
    fn a_type3_glyph_counts_as_often_as_its_font_s_characters_are_shown() {
        let font = "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1 1] /FontMatrix [1 0 0 1 0 0] /CharProcs << /a 6 0 R >> /Encoding << /Differences [97 /a] >> /FirstChar 97 /LastChar 97 /Widths [1] /Resources << /XObject << /I 7 0 R >> >> >>".to_string();
        let page = |text: &str| one_page("0 0 612 792", "/Font << /T3 5 0 R /F 8 0 R >>", text, &[font.clone(), stream("", "1 0 d0 /I Do"), image(1000, 1000), "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into()]);
        assert_eq!(doc(page("BT /T3 1 Tf (aaaaaaaaaa) Tj ET")).unwrap().pages[0].problem, None, "10 MP");
        let shown = format!("BT /T3 1 Tf [({}) 5 ({})] TJ ET", "a".repeat(20), "a".repeat(20));
        assert_eq!(doc(page(&shown)).unwrap().pages[0].problem, Some(PageProblem::TooManyPixels(40_000_000)));
        // characters in another font are not the Type 3 font's
        let other = format!("BT /F 1 Tf ({}) Tj ET", "a".repeat(100));
        assert_eq!(doc(page(&other)).unwrap().pages[0].problem, None);
    }

    #[test]
    fn a_form_shared_by_pages_is_read_once() {
        let count = 3;
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            format!("<< /Type /Pages /Kids [{}] /Count {count} >>", (0..count).map(|i| format!("{} 0 R", i + 5)).collect::<Vec<_>>().join(" ")),
            stream("", "/Fm Do"),
            form("", "0 0 1 1 re f"),
        ];
        objects.extend((0..count).map(|_| "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /XObject << /Fm 4 0 R >> >> /Contents 3 0 R >>".to_string()));
        let pdf = Pdf::new(Arc::new(pdf(&objects, ""))).unwrap();
        let mut scan = Scan::default();
        let doc = scan_with(&pdf, &mut scan).unwrap();
        assert!(doc.pages.iter().all(|p| p.problem.is_none()));
        // each page's content, and the form once
        assert_eq!((scan.decodes, scan.summaries.len()), (count + 1, 1));
    }

    #[test]
    fn the_scan_stops_inflating_past_its_budget() {
        let mut scan = Scan { decoded: MAX_SCAN_DECODED + 1, ..Scan::default() };
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_string(),
            stream("", "0 0 1 1 re f"),
        ];
        let pdf = Pdf::new(Arc::new(pdf(&objects, ""))).unwrap();
        assert_eq!(scan_with(&pdf, &mut scan).unwrap().pages[0].problem, Some(PageProblem::TooComplex));
        assert_eq!(scan.decodes, 0, "nothing more is inflated");
    }

    #[test]
    fn an_image_s_soft_mask_counts_too() {
        let masked = stream("/Type /XObject /Subtype /Image /Width 4000 /Height 4000 /ColorSpace /DeviceGray /BitsPerComponent 8 /SMask 6 0 R", "");
        let bytes = one_page("0 0 612 792", "/XObject << /Im 5 0 R >>", "/Im Do", &[masked.clone(), image(4000, 4000)]);
        assert_eq!(doc(bytes).unwrap().pages[0].problem, None, "32 MP is within the budget");
        let bytes = one_page("0 0 612 792", "/XObject << /Im 5 0 R >>", "/Im Do", &[masked, image(4000, 4001)]);
        assert_eq!(doc(bytes).unwrap().pages[0].problem, Some(PageProblem::TooManyPixels(32_004_000)));
    }

    /// Forms 5, 6, … each drawing the next, `depth` of them.
    fn nested(depth: usize) -> Vec<String> {
        (0..depth).map(|i| if i + 1 < depth { form(&format!("/XObject << /F {} 0 R >>", 6 + i), "/F Do") } else { form("", "") }).collect()
    }

    #[test]
    fn forms_may_nest_sixteen_deep() {
        let bytes = one_page("0 0 612 792", "/XObject << /F 5 0 R >>", "/F Do", &nested(MAX_NESTING));
        assert_eq!(doc(bytes).unwrap().pages[0].problem, None);
        let bytes = one_page("0 0 612 792", "/XObject << /F 5 0 R >>", "/F Do", &nested(MAX_NESTING + 1));
        assert_eq!(doc(bytes).unwrap().pages[0].problem, Some(PageProblem::TooDeep));
    }

    #[test]
    fn a_form_inside_itself_is_too_deep() {
        let looped = form("/XObject << /Me 5 0 R >>", "/Me Do");
        let bytes = one_page("0 0 612 792", "/XObject << /F 5 0 R >>", "/F Do", &[looped]);
        assert_eq!(doc(bytes).unwrap().pages[0].problem, Some(PageProblem::TooDeep));
    }

    #[test]
    fn a_page_box_must_be_finite_non_zero_and_within_the_side() {
        let bad = |media: &str| doc(one_page(media, "", "", &[])).unwrap().pages[0];
        assert_eq!(bad("0 0 14400 100").problem, None);
        assert_eq!(bad("0 0 20000 100"), PageInfo { size: LETTER, problem: Some(PageProblem::BadSize) });
        assert_eq!(bad("0 0 0 792").problem, Some(PageProblem::BadSize));
        assert!(!page_box_ok(f32::INFINITY, 10.));
        assert!(!page_box_ok(f32::NAN, 10.));
        assert!(!page_box_ok(0.5, 10.));
        assert!(page_box_ok(1., MAX_PAGE_SIDE));
    }

    #[test]
    fn the_first_two_hundred_pages_show() {
        let count = MAX_PAGES + 5;
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            format!("<< /Type /Pages /Kids [{}] /Count {count} >>", (0..count).map(|i| format!("{} 0 R", i + 4)).collect::<Vec<_>>().join(" ")),
            stream("", ""),
        ];
        objects.extend((0..count).map(|_| "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 3 0 R >>".to_string()));
        let doc = doc(pdf(&objects, "")).unwrap();
        assert_eq!((doc.pages.len(), doc.total), (MAX_PAGES, count));
    }

    #[test]
    fn a_file_with_too_many_images_and_forms_is_refused() {
        let images: Vec<String> = (0..=MAX_XOBJECTS).map(|_| image(1, 1)).collect();
        assert_eq!(doc(one_page("0 0 612 792", "", "", &images)), Err(Refusal::TooManyObjects));
    }

    #[test]
    fn an_encrypted_file_is_refused_and_a_broken_one_said_so() {
        let id = "<00112233445566778899aabbccddeeff>";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_string(),
            // the standard handler, with an owner and user key no empty password opens
            format!("<< /Filter /Standard /V 2 /R 3 /Length 128 /P -4 /O <{}> /U <{}> >>", "ab".repeat(32), "cd".repeat(32)),
        ];
        let bytes = pdf(&objects, &format!("/Encrypt 4 0 R /ID [{id} {id}]"));
        assert_eq!(doc(bytes), Err(Refusal::Encrypted));
        assert_eq!(doc(b"%PDF-1.7\nnot a pdf".to_vec()), Err(Refusal::Broken));
        assert_eq!(doc(Vec::new()), Err(Refusal::Broken));
        assert_eq!(doc(pdf(&["<< /Type /Catalog /Pages 2 0 R >>".into(), "<< /Type /Pages /Kids [] /Count 0 >>".into()], "")), Err(Refusal::Empty));
    }

    #[test]
    fn the_render_scale_stays_within_sixteen_megapixels() {
        let letter = (612., 792.);
        assert_eq!(clamp_scale(letter, 2.), 2.);
        let cap = clamp_scale(letter, 100.);
        assert!((612. * cap) * (792. * cap) <= MAX_RENDER_PIXELS * 1.0001);
        assert!((612. * cap) * (792. * cap) > MAX_RENDER_PIXELS * 0.999);
        // a long strip is held by its side
        let strip = (14_400., 100.);
        assert!(14_400. * clamp_scale(strip, 10.) <= MAX_RENDER_SIDE);
        // and a page is never drawn under a pixel a side, nor at a scale that is not a number
        assert_eq!(clamp_scale((100., 100.), 0.), 0.01);
        assert_eq!(clamp_scale(letter, f32::NAN), 1.);
    }

    #[test]
    fn zoom_buckets_round_up_and_the_budget_caps_them() {
        assert_eq!(bucket(1.), 0);
        assert_eq!(bucket(2.), 8);
        assert!(bucket_scale(bucket(1.3)) >= 1.3 && bucket_scale(bucket(1.3)) < 1.3 * 1.091);
        assert_eq!(render_bucket((612., 792.), 2.), (8, 2.));
        let (b, scale) = render_bucket((612., 792.), 50.);
        assert!(scale <= clamp_scale((612., 792.), 50.));
        assert_eq!(render_bucket((612., 792.), 80.), (b, scale), "past the cap every zoom is one bucket");
    }

    #[test]
    fn the_cache_drops_the_least_recently_used_past_its_budget() {
        let none = HashSet::new();
        let mut cache = Cache::new(100);
        assert!(cache.insert((0, 0), "a", 40, &none).is_empty());
        assert!(cache.insert((1, 0), "b", 40, &none).is_empty());
        // a use of page 0 makes page 1 the oldest
        assert_eq!(cache.nearest(0, 0), Some((0, "a")));
        assert_eq!(cache.insert((2, 0), "c", 40, &none), vec!["b"]);
        assert_eq!((cache.len(), cache.used()), (2, 80));
        assert!(cache.contains((0, 0)) && cache.contains((2, 0)));
        // the same key again replaces it
        assert_eq!(cache.insert((2, 0), "c2", 30, &none), vec!["c"]);
        assert_eq!(cache.used(), 70);
        // one over the whole budget is not kept
        assert_eq!(cache.insert((3, 0), "huge", 101, &none), vec!["huge"]);
        assert_eq!(cache.used(), 70);
        // making room for a large one takes as many as it needs
        let mut gone = cache.insert((4, 0), "d", 100, &none);
        gone.sort();
        assert_eq!(gone, vec!["a", "c2"]);
        assert_eq!(cache.clear(), vec!["d"]);
        assert!(cache.is_empty() && cache.used() == 0);
    }

    #[test]
    fn the_cache_never_drops_a_wanted_page() {
        let mut cache = Cache::new(100);
        let wanted: HashSet<(usize, i32)> = [(0, 0), (1, 0)].into();
        cache.insert((0, 0), "a", 60, &wanted);
        // past the budget, but wanted: both stay
        assert!(cache.insert((1, 0), "b", 60, &wanted).is_empty());
        assert_eq!(cache.used(), 120);
        // one not wanted goes first, and one not wanted that cannot fit is not kept
        assert_eq!(cache.insert((2, 0), "c", 101, &wanted), vec!["c"]);
        cache.fit(200);
        assert_eq!(cache.budget(), 200);
        assert!(cache.insert((2, 0), "c", 70, &wanted).is_empty());
        // the oldest is page 0, but only the unwanted page 2 may go
        assert_eq!(cache.insert((3, 0), "d", 30, &wanted), vec!["c"]);
        assert!(cache.contains((0, 0)) && cache.contains((1, 0)) && cache.contains((3, 0)));
        // a wanted page larger than the whole budget is kept too
        cache.fit(0);
        assert_eq!(cache.budget(), 100);
        let big: HashSet<(usize, i32)> = [(5, 0)].into();
        let mut gone = cache.insert((5, 0), "e", 150, &big);
        gone.sort();
        assert_eq!(gone, vec!["a", "b", "d"]);
        assert!(cache.contains((5, 0)));
    }

    #[test]
    fn a_zoom_shows_the_nearest_bucket_drawn() {
        let mut cache = Cache::new(CACHE_BYTES);
        let none = HashSet::new();
        cache.insert((3, 4), 'x', 10, &none);
        cache.insert((3, 12), 'y', 10, &none);
        cache.insert((4, 9), 'z', 10, &none);
        assert_eq!(cache.nearest(3, 9), Some((12, 'y')));
        assert_eq!(cache.nearest(3, 4), Some((4, 'x')));
        assert_eq!(cache.nearest(5, 4), None);
    }

    #[test]
    fn the_visible_pages_are_the_ones_the_viewport_touches() {
        let pages = vec![(100., 100.); 10];
        let layout = Layout::new(&pages, 1., 300.);
        // pages at 16, 128, 240, … (100 tall, 12 apart), centred in the 300 px viewport
        assert_eq!(layout.rects[1], [100., 128., 100., 100.]);
        assert_eq!(layout.height, 16. + 10. * 100. + 9. * 12. + 16.);
        assert_eq!(layout.visible(0., 100.), 0..1);
        assert_eq!(layout.visible(120., 250.), 1..3, "the gap before page 1 shows none of page 0");
        assert_eq!(layout.visible(200., 210.), 1..2);
        assert_eq!(layout.visible(5000., 6000.), 10..10);
        assert_eq!(around(layout.visible(120., 250.), 1, 10), 0..4);
        assert_eq!(around(9..10, 1, 10), 8..10);
        assert_eq!(around(10..10, 1, 10), 9..10);
        assert_eq!(layout.page_at(240.), 2);
        assert_eq!(layout.page_at(1e9), 9);
    }

    #[test]
    fn a_zoom_keeps_the_point_under_it() {
        let pages = vec![(612., 792.), (792., 612.), (612., 792.)];
        let fit = fit_width(&pages, 800.);
        assert_eq!(fit, (800. - PAD * 2.) / 792.);
        let before = Layout::new(&pages, fit, 800.);
        let anchor = before.anchor(400., 1000.);
        assert_eq!(anchor.page, 1);
        let after = Layout::new(&pages, fit * 2., 800.);
        let (x, y) = after.point(anchor);
        let back = after.anchor(x, y);
        assert_eq!(back.page, 1);
        assert!((back.fx - anchor.fx).abs() < 1e-4 && (back.fy - anchor.fy).abs() < 1e-4);
        assert!(after.width > 800., "wider than the viewport: it scrolls across");
    }
}
