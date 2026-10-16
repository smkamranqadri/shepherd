//! The PDF worker: Shepherd run again as `shepherd --pdf-worker` (never shown anywhere), which reads, checks and draws
//! one PDF for a PDF tab, so a file that inflates to gigabytes, draws forever or crashes the parser takes only the
//! worker with it. Before it reads anything the worker caps its own memory on every OS (`Capped`, the allocator: past
//! `MAX_MEMORY` live it aborts), on unix its address space too (`RLIMIT_AS`) and its priority; it watches itself
//! (`Watch`): it ends when its parent goes (and on Linux the kernel kills it then, `PR_SET_PDEATHSIG`) or when one
//! request runs past twice its time. The tab gives each request a time (`Timeouts`) and kills the worker past it.
//! On Linux the tab starts `/proc/self/exe`, its own binary even after a package upgrade replaced the file, and the
//! two say their protocol version first (`VERSION`): a worker of another version is refused.
//!
//! The two speak frames over the worker's stdin and stdout: a little-endian `u32` length, then a tag byte and the
//! body. A frame is at most `MAX_FRAME` (a page of `pdf::MAX_RENDER_PIXELS` in RGBA and its header), so a worker gone
//! wrong cannot make the tab allocate more.
use super::pdf::{self, Doc, PageInfo, PageProblem, Refusal, RgbaImage};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::alloc::{GlobalAlloc, Layout, System};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

/// The argument that runs Shepherd as the PDF worker.
pub const ARG: &str = "--pdf-worker";
/// The worker's memory (live allocations, and its address space on unix): a 64 MB file, a page's images
/// (`pdf::MAX_PAGE_IMAGE_PIXELS` as RGBA), the page drawn and hayro's own, with room.
pub const MAX_MEMORY: u64 = 1536 << 20;
/// The protocol's version, which the worker says first and the tab says with every file it opens.
pub const VERSION: u8 = 1;
/// The stack of the worker's thread that parses and draws (deeply nested content recurses).
const STACK: usize = 64 << 20;
/// The largest frame either side reads.
pub const MAX_FRAME: usize = pdf::MAX_RENDER_PIXELS as usize * 4 + 64;

/// How long the tab waits for the worker: to read and check the file, and to draw one page.
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    pub open: Duration,
    pub render: Duration,
}

impl Default for Timeouts {
    fn default() -> Timeouts {
        Timeouts { open: Duration::from_secs(10), render: Duration::from_secs(5) }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Open(PathBuf),
    PageSize(usize),
    /// A page at a scale, in device pixels per point.
    Render(usize, f32),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Response {
    /// The worker's first frame: its protocol version.
    Hello(u8),
    Opened(Result<Doc, Refusal>),
    Size(f32, f32),
    Image(RgbaImage),
    /// The request could not be done (no file open, no such page, the page would not draw): why.
    Failed(String),
}

/// What went wrong with the worker itself; it has been killed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    TimedOut,
    /// Its protocol version is not this one's (an older or newer Shepherd).
    Mismatch,
    /// It exited, closed its pipes or said something that is not a frame.
    Crashed,
}

/// Counts a worker's faults: one that crashes twice running is not started again. A request past its time fails
/// only its own page; a success starts the count over.
#[derive(Debug, Default)]
pub struct Faults {
    crashes: usize,
}

impl Faults {
    /// Crashes running after which the tab gives up.
    pub const LIMIT: usize = 2;

    /// `result` of a request noted; true when the tab should give up.
    pub fn note(&mut self, result: Result<(), Fault>) -> bool {
        match result {
            Ok(()) => self.crashes = 0,
            Err(Fault::TimedOut) => {}
            Err(Fault::Crashed | Fault::Mismatch) => self.crashes += 1,
        }
        self.crashes >= Faults::LIMIT
    }
}

// ---- the memory cap ----

/// The allocator, the system's: once `cap_memory` has set a cap (the worker does, first thing) it counts what is
/// live and aborts the process past the cap; until then (the app itself, always) it only passes through.
pub struct Capped;

#[global_allocator]
static ALLOCATOR: Capped = Capped;

/// The cap in bytes, 0 for none.
static CAP: AtomicIsize = AtomicIsize::new(0);
/// What is live since the cap was set (frees of what came before can take it under 0).
static LIVE: AtomicIsize = AtomicIsize::new(0);

/// Caps this process's live allocations at `bytes`: one past it aborts the process.
pub fn cap_memory(bytes: u64) {
    CAP.store(isize::try_from(bytes).unwrap_or(isize::MAX), Ordering::Relaxed);
}

/// Whether `grow` more bytes may be live under `cap` with `live` already, counting them in when they may.
fn admit(live: &AtomicIsize, cap: isize, grow: usize) -> bool {
    let grow = isize::try_from(grow).unwrap_or(isize::MAX);
    let after = live.fetch_add(grow, Ordering::Relaxed).saturating_add(grow);
    if after > cap {
        live.fetch_sub(grow, Ordering::Relaxed);
        return false;
    }
    true
}

fn grow(size: usize) {
    let cap = CAP.load(Ordering::Relaxed);
    if cap != 0 && !admit(&LIVE, cap, size) {
        // past the cap: the tab sees a crash and the page says so
        std::process::abort();
    }
}

fn shrink(size: usize) {
    if CAP.load(Ordering::Relaxed) != 0 {
        LIVE.fetch_sub(isize::try_from(size).unwrap_or(isize::MAX), Ordering::Relaxed);
    }
}

// SAFETY: every call goes to `System` with the same arguments; the counting around it allocates nothing.
unsafe impl GlobalAlloc for Capped {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        grow(layout.size());
        // SAFETY: as `GlobalAlloc::alloc`'s caller promised.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        grow(layout.size());
        // SAFETY: as `GlobalAlloc::alloc_zeroed`'s caller promised.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: as `GlobalAlloc::dealloc`'s caller promised.
        unsafe { System.dealloc(ptr, layout) };
        shrink(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() {
            grow(new_size - layout.size());
        }
        // SAFETY: as `GlobalAlloc::realloc`'s caller promised.
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        if new_size < layout.size() && !moved.is_null() {
            shrink(layout.size() - new_size);
        }
        moved
    }
}

// ---- the watchdog ----

/// The request the worker is on, and when it must be done by (twice its time): a request past that, one hayro never
/// finishes, ends the worker even if the tab is gone or stuck.
#[derive(Default)]
pub struct Watch {
    /// Milliseconds since `epoch` by which the request must be answered; 0 when idle.
    due: AtomicU64,
}

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

fn now_ms() -> u64 {
    epoch().elapsed().as_millis() as u64
}

impl Watch {
    fn begin(&self, time: Duration) {
        self.due.store(now_ms().saturating_add(2 * time.as_millis() as u64).max(1), Ordering::Relaxed);
    }

    fn end(&self) {
        self.due.store(0, Ordering::Relaxed);
    }

    /// Whether the request on, at `now` (milliseconds since the epoch), is past twice its time.
    fn overdue(&self, now: u64) -> bool {
        let due = self.due.load(Ordering::Relaxed);
        due != 0 && now > due
    }
}

// ---- frames ----

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

fn write_frame(out: &mut impl Write, tag: u8, body: &[u8]) -> io::Result<()> {
    let len = u32::try_from(body.len() + 1).map_err(|_| invalid("frame too large"))?;
    out.write_all(&len.to_le_bytes())?;
    out.write_all(&[tag])?;
    out.write_all(body)?;
    out.flush()
}

/// The next frame's tag and body; `UnexpectedEof` at a clean end.
fn read_frame(input: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut len = [0; 4];
    input.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(invalid("bad frame length"));
    }
    let mut frame = vec![0; len];
    input.read_exact(&mut frame)?;
    let body = frame.split_off(1);
    Ok((frame[0], body))
}

/// A frame's body, read in order.
struct Body<'a>(&'a [u8]);

impl Body<'_> {
    fn take(&mut self, n: usize) -> io::Result<&[u8]> {
        if self.0.len() < n {
            return Err(invalid("short frame"));
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().expect("4 bytes")))
    }

    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().expect("8 bytes")))
    }

    fn f32(&mut self) -> io::Result<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().expect("4 bytes")))
    }

    fn bytes(&mut self) -> io::Result<&[u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }

    fn string(&mut self) -> io::Result<String> {
        Ok(String::from_utf8_lossy(self.bytes()?).into_owned())
    }

    fn end(&self) -> io::Result<()> {
        if self.0.is_empty() { Ok(()) } else { Err(invalid("long frame")) }
    }
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend((bytes.len() as u32).to_le_bytes());
    out.extend(bytes);
}

/// A path as bytes and back: its bytes on unix, its UTF-16 units on Windows.
fn path_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str().encode_wide().flat_map(u16::to_le_bytes).collect()
    }
}

fn path_from(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let wide: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        PathBuf::from(std::ffi::OsString::from_wide(&wide))
    }
}

pub fn write_request(out: &mut impl Write, request: &Request) -> io::Result<()> {
    match request {
        Request::Open(path) => {
            let mut body = vec![VERSION];
            put_bytes(&mut body, &path_bytes(path));
            write_frame(out, 1, &body)
        }
        Request::PageSize(page) => write_frame(out, 2, &(*page as u32).to_le_bytes()),
        Request::Render(page, scale) => {
            let mut body = (*page as u32).to_le_bytes().to_vec();
            body.extend(scale.to_le_bytes());
            write_frame(out, 3, &body)
        }
    }
}

pub fn read_request(input: &mut impl Read) -> io::Result<Request> {
    let (tag, body) = read_frame(input)?;
    let mut b = Body(&body);
    let request = match tag {
        1 => {
            if b.u8()? != VERSION {
                return Err(invalid("another protocol version"));
            }
            Request::Open(path_from(b.bytes()?))
        }
        2 => Request::PageSize(b.u32()? as usize),
        3 => Request::Render(b.u32()? as usize, b.f32()?),
        _ => return Err(invalid("unknown request")),
    };
    b.end()?;
    Ok(request)
}

fn problem_tag(problem: Option<PageProblem>) -> (u8, u64) {
    match problem {
        None => (0, 0),
        Some(PageProblem::BadSize) => (1, 0),
        Some(PageProblem::TooManyPixels(pixels)) => (2, pixels),
        Some(PageProblem::TooDeep) => (3, 0),
        Some(PageProblem::TooComplex) => (4, 0),
    }
}

pub fn write_response(out: &mut impl Write, response: &Response) -> io::Result<()> {
    let mut body = Vec::new();
    let tag = match response {
        Response::Hello(version) => {
            body.push(*version);
            0
        }
        Response::Opened(Ok(doc)) => {
            body.extend((doc.total as u32).to_le_bytes());
            body.extend((doc.pages.len() as u32).to_le_bytes());
            for page in &doc.pages {
                let (tag, value) = problem_tag(page.problem);
                body.extend(page.size.0.to_le_bytes());
                body.extend(page.size.1.to_le_bytes());
                body.push(tag);
                body.extend(value.to_le_bytes());
            }
            1
        }
        Response::Opened(Err(refusal)) => {
            match refusal {
                Refusal::TooLarge(size) => {
                    body.push(1);
                    body.extend(size.to_le_bytes());
                }
                Refusal::Encrypted => body.push(2),
                Refusal::Broken => body.push(3),
                Refusal::Empty => body.push(4),
                Refusal::TooManyObjects => body.push(5),
                Refusal::Unreadable(why) => {
                    body.push(6);
                    put_bytes(&mut body, why.as_bytes());
                }
            }
            2
        }
        Response::Size(w, h) => {
            body.extend(w.to_le_bytes());
            body.extend(h.to_le_bytes());
            3
        }
        Response::Image(image) => {
            body.extend(image.width().to_le_bytes());
            body.extend(image.height().to_le_bytes());
            body.extend_from_slice(image.as_raw());
            4
        }
        Response::Failed(why) => {
            put_bytes(&mut body, why.as_bytes());
            5
        }
    };
    write_frame(out, tag, &body)
}

pub fn read_response(input: &mut impl Read) -> io::Result<Response> {
    let (tag, body) = read_frame(input)?;
    let mut b = Body(&body);
    let response = match tag {
        0 => Response::Hello(b.u8()?),
        1 => {
            let total = b.u32()? as usize;
            let n = b.u32()? as usize;
            if n > pdf::MAX_PAGES || n > total {
                return Err(invalid("too many pages"));
            }
            let mut pages = Vec::with_capacity(n);
            for _ in 0..n {
                let size = (b.f32()?, b.f32()?);
                let (tag, value) = (b.u8()?, b.u64()?);
                let problem = match tag {
                    0 => None,
                    1 => Some(PageProblem::BadSize),
                    2 => Some(PageProblem::TooManyPixels(value)),
                    3 => Some(PageProblem::TooDeep),
                    4 => Some(PageProblem::TooComplex),
                    _ => return Err(invalid("unknown page problem")),
                };
                // a size the view can lay out: the worker checked it, the view does not take its word
                let size = if pdf::page_box_ok(size.0, size.1) { size } else { (612., 792.) };
                pages.push(PageInfo { size, problem });
            }
            Response::Opened(Ok(Doc { pages, total }))
        }
        2 => Response::Opened(Err(match b.u8()? {
            1 => Refusal::TooLarge(b.u64()?),
            2 => Refusal::Encrypted,
            3 => Refusal::Broken,
            4 => Refusal::Empty,
            5 => Refusal::TooManyObjects,
            6 => Refusal::Unreadable(b.string()?),
            _ => return Err(invalid("unknown refusal")),
        })),
        3 => Response::Size(b.f32()?, b.f32()?),
        4 => {
            let (w, h) = (b.u32()?, b.u32()?);
            let side = 1..=pdf::MAX_RENDER_SIDE as u32;
            if !side.contains(&w) || !side.contains(&h) {
                return Err(invalid("bad image size"));
            }
            let len = (w as usize).checked_mul(h as usize).and_then(|p| p.checked_mul(4)).ok_or_else(|| invalid("image too large"))?;
            let pixels = b.take(len)?.to_vec();
            Response::Image(RgbaImage::from_raw(w, h, pixels).ok_or_else(|| invalid("bad image"))?)
        }
        5 => Response::Failed(b.string()?),
        _ => return Err(invalid("unknown response")),
    };
    b.end()?;
    Ok(response)
}

// ---- the worker's side ----

/// Runs Shepherd as the PDF worker: caps and watches itself, then answers requests on stdin until it closes.
pub fn serve() {
    #[cfg(unix)]
    // SAFETY: getppid has no preconditions.
    let parent = unsafe { libc::getppid() };
    cap_memory(MAX_MEMORY);
    if limit_self().is_err() {
        std::process::exit(2);
    }
    static WATCH: OnceLock<Watch> = OnceLock::new();
    let watch = WATCH.get_or_init(Watch::default);
    let watching = std::thread::Builder::new().name("pdf-watchdog".into()).spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(250));
            // SAFETY: getppid has no preconditions.
            #[cfg(unix)]
            if unsafe { libc::getppid() } != parent {
                std::process::abort();
            }
            if watch.overdue(now_ms()) {
                std::process::abort();
            }
        }
    });
    if watching.is_err() {
        std::process::exit(2);
    }
    let answered = std::thread::Builder::new().name("pdf-worker".into()).stack_size(STACK).spawn(move || {
        let (stdin, stdout) = (io::stdin(), io::stdout());
        serve_on(&mut stdin.lock(), &mut io::BufWriter::new(stdout.lock()), watch)
    });
    let ok = answered.ok().and_then(|t| t.join().ok()).is_some_and(|r| r.is_ok());
    std::process::exit(if ok { 0 } else { 1 });
}

/// Answers requests from `input` on `output` until `input` ends (`Ok`) or a frame is not one (`Err`).
pub fn serve_on(input: &mut impl Read, output: &mut impl Write, watch: &Watch) -> io::Result<()> {
    let mut engine: Option<Box<dyn pdf::PdfEngine>> = None;
    let never = AtomicBool::new(false);
    let times = Timeouts::default();
    write_response(output, &Response::Hello(VERSION))?;
    loop {
        let request = match read_request(input) {
            Ok(request) => request,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        watch.begin(if matches!(request, Request::Open(_)) { times.open } else { times.render });
        let response = match request {
            Request::Open(path) => {
                engine = None;
                match read(&path).and_then(pdf::open) {
                    Ok((opened, doc)) => {
                        engine = Some(opened);
                        Response::Opened(Ok(doc))
                    }
                    Err(refusal) => Response::Opened(Err(refusal)),
                }
            }
            Request::PageSize(page) => match &engine {
                Some(engine) if page < engine.pages() => {
                    let (w, h) = engine.page_size(page);
                    Response::Size(w, h)
                }
                _ => Response::Failed(format!("no page {page}")),
            },
            Request::Render(page, scale) => match &engine {
                Some(engine) => match engine.render(page, scale, &never) {
                    Ok(image) => Response::Image(image),
                    Err(e) => Response::Failed(format!("{e:#}")),
                },
                None => Response::Failed("no file open".into()),
            },
        };
        write_response(output, &response)?;
        watch.end();
    }
}

/// The file's bytes, at most `pdf::MAX_PDF` (one byte more says it grew past it).
fn read(path: &Path) -> Result<Vec<u8>, Refusal> {
    let unreadable = |e: io::Error| {
        Refusal::Unreadable(match e.kind() {
            io::ErrorKind::NotFound => "This file no longer exists".into(),
            io::ErrorKind::PermissionDenied => "Permission denied".into(),
            _ => e.to_string(),
        })
    };
    let file = std::fs::File::open(path).map_err(unreadable)?;
    let size = file.metadata().map_err(unreadable)?.len();
    if size > pdf::MAX_PDF {
        return Err(Refusal::TooLarge(size));
    }
    let mut bytes = Vec::new();
    file.take(pdf::MAX_PDF + 1).read_to_end(&mut bytes).map_err(unreadable)?;
    if bytes.len() as u64 > pdf::MAX_PDF {
        return Err(Refusal::TooLarge(bytes.len() as u64));
    }
    Ok(bytes)
}

/// The worker's limits on unix, set before it reads anything: its address space at `MAX_MEMORY` (or the hard limit,
/// when that is lower), a lower priority than the app's, and on Linux death with its parent.
#[cfg(unix)]
fn limit_self() -> io::Result<()> {
    let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: `limit` is a valid `struct rlimit` for getrlimit to fill.
    if unsafe { libc::getrlimit(libc::RLIMIT_AS, &mut limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let cap = libc::rlim_t::try_from(MAX_MEMORY).unwrap_or(libc::rlim_t::MAX);
    let cap = if limit.rlim_max == libc::RLIM_INFINITY { cap } else { cap.min(limit.rlim_max) };
    limit.rlim_cur = cap;
    // SAFETY: `limit` is a valid `struct rlimit`.
    if unsafe { libc::setrlimit(libc::RLIMIT_AS, &limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: plain calls without pointers; nice's -1 may be a valid priority, so its result is not an error here.
    unsafe {
        libc::nice(10);
        #[cfg(target_os = "linux")]
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Windows has no address-space limit here: the allocator's cap and the times hold the worker.
#[cfg(not(unix))]
fn limit_self() -> io::Result<()> {
    Ok(())
}

// ---- the tab's side ----

/// A way to kill a worker from another thread (the tab, when it hides or closes): it only sends the kill and never
/// waits; the worker's driver reaps it (`reap`, when its `Worker` goes).
#[derive(Clone)]
pub struct Killer(Arc<Mutex<Option<Child>>>);

impl Killer {
    pub fn kill(&self) {
        if let Some(child) = self.0.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            let _ = child.kill();
        }
    }

    /// Kills the worker and waits for it to exit, without holding the lock while it waits.
    fn reap(&self) {
        let child = self.0.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// The worker's process id, until it is reaped.
    pub fn pid(&self) -> Option<u32> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(Child::id)
    }
}

/// A running worker. Its replies are read on a thread of their own, so each request waits at most its time; past
/// it, or when the worker crashes, it is killed and reaped. Dropped, it is killed and reaped.
pub struct Worker {
    stdin: ChildStdin,
    replies: mpsc::Receiver<io::Result<Response>>,
    child: Killer,
}

impl Worker {
    /// Shepherd itself, as the worker (without a console window on Windows). On Linux, the binary running now by
    /// `/proc/self/exe`, even when a package upgrade has replaced the file at its path.
    pub fn command() -> io::Result<Command> {
        #[cfg(target_os = "linux")]
        let exe = PathBuf::from("/proc/self/exe");
        #[cfg(not(target_os = "linux"))]
        let exe = std::env::current_exe()?;
        let mut command = crate::process::command(exe);
        command.arg(ARG);
        Ok(command)
    }

    /// Starts `command` as a worker (its stdin and stdout are the frames, its stderr goes nowhere) and waits up to
    /// `timeout` for its hello: one of another protocol version is refused.
    pub fn spawn(mut command: Command, timeout: Duration) -> Result<Worker, Fault> {
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = command.spawn().map_err(|_| Fault::Crashed)?;
        let (Some(stdin), Some(mut stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Fault::Crashed);
        };
        let child = Killer(Arc::new(Mutex::new(Some(child))));
        let (tx, replies) = mpsc::channel();
        let reading = std::thread::Builder::new().name("pdf-replies".into()).spawn(move || {
            loop {
                let reply = read_response(&mut stdout);
                let end = reply.is_err();
                if tx.send(reply).is_err() || end {
                    return;
                }
            }
        });
        let mut worker = Worker { stdin, replies, child };
        if reading.is_err() {
            return Err(Fault::Crashed);
        }
        match worker.wait(timeout)? {
            Response::Hello(VERSION) => Ok(worker),
            _ => {
                worker.child.reap();
                Err(Fault::Mismatch)
            }
        }
    }

    pub fn killer(&self) -> Killer {
        self.child.clone()
    }

    /// Sends `request` and waits up to `timeout` for the answer; a fault kills and reaps the worker.
    pub fn request(&mut self, request: &Request, timeout: Duration) -> Result<Response, Fault> {
        if write_request(&mut self.stdin, request).is_err() {
            self.child.reap();
            return Err(Fault::Crashed);
        }
        self.wait(timeout)
    }

    fn wait(&mut self, timeout: Duration) -> Result<Response, Fault> {
        let fault = match self.replies.recv_timeout(timeout) {
            Ok(Ok(response)) => return Ok(response),
            Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => Fault::Crashed,
            Err(mpsc::RecvTimeoutError::Timeout) => Fault::TimedOut,
        };
        self.child.reap();
        Err(fault)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.child.reap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip_request(request: Request) {
        let mut bytes = Vec::new();
        write_request(&mut bytes, &request).unwrap();
        assert_eq!(read_request(&mut bytes.as_slice()).unwrap(), request);
    }

    fn round_trip_response(response: Response) {
        let mut bytes = Vec::new();
        write_response(&mut bytes, &response).unwrap();
        assert_eq!(read_response(&mut bytes.as_slice()).unwrap(), response);
    }

    #[test]
    fn requests_and_responses_survive_the_pipe() {
        round_trip_request(Request::Open(PathBuf::from("/tmp/a b/ü.pdf")));
        round_trip_request(Request::PageSize(7));
        round_trip_request(Request::Render(3, 2.5));
        round_trip_response(Response::Hello(VERSION));
        let pages = vec![
            PageInfo { size: (612., 792.), problem: None },
            PageInfo { size: (100., 50.), problem: Some(PageProblem::TooManyPixels(40_000_000)) },
            PageInfo { size: (612., 792.), problem: Some(PageProblem::BadSize) },
            PageInfo { size: (612., 792.), problem: Some(PageProblem::TooDeep) },
            PageInfo { size: (612., 792.), problem: Some(PageProblem::TooComplex) },
        ];
        round_trip_response(Response::Opened(Ok(Doc { pages, total: 300 })));
        for refusal in [Refusal::TooLarge(1 << 30), Refusal::Encrypted, Refusal::Broken, Refusal::Empty, Refusal::TooManyObjects, Refusal::Unreadable("Permission denied".into())] {
            round_trip_response(Response::Opened(Err(refusal)));
        }
        round_trip_response(Response::Size(1.5, 2.5));
        round_trip_response(Response::Image(RgbaImage::from_fn(3, 2, |x, y| image::Rgba([x as u8, y as u8, 7, 255]))));
        round_trip_response(Response::Failed("no page 9".into()));
    }

    fn image_frame(w: u32, h: u32, len: usize) -> Vec<u8> {
        let mut frame = Vec::new();
        let mut body = w.to_le_bytes().to_vec();
        body.extend(h.to_le_bytes());
        body.extend(vec![0; len]);
        write_frame(&mut frame, 4, &body).unwrap();
        frame
    }

    #[test]
    fn a_bad_frame_is_an_error_not_an_allocation() {
        // a length past the cap is refused before anything is read into it
        let mut huge = ((MAX_FRAME + 1) as u32).to_le_bytes().to_vec();
        huge.push(4);
        assert!(read_response(&mut huge.as_slice()).is_err());
        // an image whose pixels are not all there
        assert!(read_response(&mut image_frame(2, 2, 15).as_slice()).is_err());
        // an image with no pixels, or a side past the largest drawn
        assert!(read_response(&mut image_frame(0, 5, 0).as_slice()).is_err());
        assert!(read_response(&mut image_frame(pdf::MAX_RENDER_SIDE as u32 + 1, 1, (pdf::MAX_RENDER_SIDE as usize + 1) * 4).as_slice()).is_err());
        assert!(read_response(&mut image_frame(1, 1, 4).as_slice()).is_ok());
        // more pages than a document shows
        let mut many = Vec::new();
        let mut body = 1000u32.to_le_bytes().to_vec();
        body.extend(1000u32.to_le_bytes());
        write_frame(&mut many, 1, &body).unwrap();
        assert!(read_response(&mut many.as_slice()).is_err());
        assert_eq!(read_request(&mut [].as_slice()).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn a_request_of_another_protocol_version_is_refused() {
        let mut open = Vec::new();
        write_request(&mut open, &Request::Open(PathBuf::from("a.pdf"))).unwrap();
        // the version is the body's first byte, after the length and the tag
        open[5] = VERSION + 1;
        assert!(read_request(&mut open.as_slice()).is_err());
        // and the worker ends on it, after its hello
        let mut output = Vec::new();
        assert!(serve_on(&mut open.as_slice(), &mut output, &Watch::default()).is_err());
        assert_eq!(read_response(&mut output.as_slice()).unwrap(), Response::Hello(VERSION));
    }

    #[test]
    fn the_worker_opens_draws_and_answers_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one.pdf");
        std::fs::write(&path, crate::files::pdf::tests::one_page_pdf("0 0 100 50", "1 0 0 rg 0 0 50 50 re f")).unwrap();
        let mut input = Vec::new();
        for request in [Request::Render(0, 1.), Request::Open(path), Request::PageSize(0), Request::PageSize(1), Request::Render(0, 2.)] {
            write_request(&mut input, &request).unwrap();
        }
        let mut output = Vec::new();
        let watch = Watch::default();
        serve_on(&mut input.as_slice(), &mut output, &watch).unwrap();
        assert!(!watch.overdue(u64::MAX), "idle once answered");
        let mut replies = output.as_slice();
        assert_eq!(read_response(&mut replies).unwrap(), Response::Hello(VERSION));
        assert_eq!(read_response(&mut replies).unwrap(), Response::Failed("no file open".into()));
        let Response::Opened(Ok(doc)) = read_response(&mut replies).unwrap() else { panic!("not opened") };
        assert_eq!((doc.pages.len(), doc.total, doc.pages[0].size), (1, 1, (100., 50.)));
        assert_eq!(read_response(&mut replies).unwrap(), Response::Size(100., 50.));
        assert_eq!(read_response(&mut replies).unwrap(), Response::Failed("no page 1".into()));
        let Response::Image(image) = read_response(&mut replies).unwrap() else { panic!("no image") };
        assert_eq!(image.dimensions(), (200, 100));
        assert_eq!(image.get_pixel(50, 50).0, [255, 0, 0, 255]);
        assert!(replies.is_empty());
    }

    #[test]
    fn a_missing_file_is_refused_in_words() {
        let mut input = Vec::new();
        write_request(&mut input, &Request::Open(PathBuf::from("/no/such/file.pdf"))).unwrap();
        let mut output = Vec::new();
        serve_on(&mut input.as_slice(), &mut output, &Watch::default()).unwrap();
        let mut replies = output.as_slice();
        assert_eq!(read_response(&mut replies).unwrap(), Response::Hello(VERSION));
        assert_eq!(read_response(&mut replies).unwrap(), Response::Opened(Err(Refusal::Unreadable("This file no longer exists".into()))));
    }

    #[test]
    fn the_watchdog_ends_a_request_past_twice_its_time() {
        let watch = Watch::default();
        assert!(!watch.overdue(u64::MAX), "idle");
        watch.begin(Duration::from_secs(5));
        let now = now_ms();
        assert!(!watch.overdue(now + 9_000));
        assert!(watch.overdue(now + 10_100));
        watch.end();
        assert!(!watch.overdue(now + 10_100));
    }

    #[test]
    fn crashes_running_give_up_and_times_do_not() {
        let mut faults = Faults::default();
        assert!(!faults.note(Err(Fault::TimedOut)));
        assert!(!faults.note(Err(Fault::TimedOut)));
        assert!(!faults.note(Err(Fault::TimedOut)), "only its own page fails");
        assert!(!faults.note(Err(Fault::Crashed)));
        assert!(!faults.note(Ok(())), "a success starts over");
        assert!(!faults.note(Err(Fault::Crashed)));
        assert!(!faults.note(Err(Fault::TimedOut)));
        assert!(faults.note(Err(Fault::Mismatch)), "two crashes running");
    }

    #[test]
    fn the_allocator_counts_against_its_cap() {
        let live = AtomicIsize::new(0);
        assert!(admit(&live, 100, 60));
        assert!(!admit(&live, 100, 41), "past the cap");
        assert_eq!(live.load(Ordering::Relaxed), 60, "a refused one is not counted");
        assert!(admit(&live, 100, 40));
        live.fetch_sub(100, Ordering::Relaxed);
        assert!(!admit(&live, 100, usize::MAX / 4));
    }

    /// Run by `a_worker_past_its_memory_cap_is_killed` in a process of its own: caps itself and allocates past it.
    #[test]
    #[ignore]
    fn memory_cap_child() {
        if std::env::var_os("SHEPHERD_CAP_CHILD").is_none() {
            return;
        }
        cap_memory(64 << 20);
        let under = vec![1u8; 16 << 20];
        println!("under the cap: {}", under.len());
        let over = vec![1u8; 128 << 20];
        println!("over the cap: {}", over.len());
    }

    #[test]
    fn a_worker_past_its_memory_cap_is_killed() {
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "files::pdf_worker::tests::memory_cap_child", "--ignored", "--nocapture", "--test-threads", "1"])
            .env("SHEPHERD_CAP_CHILD", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("under the cap"), "{stdout}");
        assert!(!stdout.contains("over the cap"), "{stdout}");
        assert!(!out.status.success());
        #[cfg(unix)]
        assert_eq!(std::os::unix::process::ExitStatusExt::signal(&out.status), Some(libc::SIGABRT));
    }

    /// A worker that says hello as version `version`, then reads its requests and never answers.
    #[cfg(unix)]
    fn silent(version: u8) -> Command {
        let mut sh = Command::new("sh");
        sh.args(["-c", &format!("printf '\\002\\000\\000\\000\\000\\{version:03o}'; cat > /dev/null")]);
        sh
    }

    #[cfg(unix)]
    fn gone(pid: u32) -> bool {
        // SAFETY: signal 0 only asks whether the process is there.
        unsafe { libc::kill(pid as i32, 0) != 0 }
    }

    #[test]
    #[cfg(unix)]
    fn a_worker_that_takes_too_long_is_killed() {
        let mut worker = Worker::spawn(silent(VERSION), Duration::from_secs(5)).unwrap();
        let killer = worker.killer();
        let pid = killer.pid().unwrap();
        let started = Instant::now();
        assert_eq!(worker.request(&Request::Render(0, 1.), Duration::from_millis(200)), Err(Fault::TimedOut));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(killer.pid().is_none() && gone(pid), "killed and reaped");
        // the next request finds it gone
        assert_eq!(worker.request(&Request::PageSize(0), Duration::from_millis(200)), Err(Fault::Crashed));
    }

    #[test]
    #[cfg(unix)]
    fn a_worker_of_another_version_is_refused() {
        assert_eq!(Worker::spawn(silent(VERSION + 8), Duration::from_secs(5)).err(), Some(Fault::Mismatch));
        // one that never says hello
        let mut mute = Command::new("sh");
        mute.args(["-c", "cat > /dev/null"]);
        assert_eq!(Worker::spawn(mute, Duration::from_millis(200)).err(), Some(Fault::TimedOut));
    }

    #[test]
    #[cfg(unix)]
    fn a_worker_that_exits_or_babbles_is_a_crash() {
        let mut exits = Command::new("sh");
        exits.args(["-c", "exit 3"]);
        assert_eq!(Worker::spawn(exits, Duration::from_secs(5)).err(), Some(Fault::Crashed));
        let mut babble = {
            let mut sh = Command::new("sh");
            sh.args(["-c", &format!("printf '\\002\\000\\000\\000\\000\\{VERSION:03o}'; head -c 4 > /dev/null; printf 'hello, world'")]);
            Worker::spawn(sh, Duration::from_secs(5)).unwrap()
        };
        assert_eq!(babble.request(&Request::PageSize(0), Duration::from_secs(5)), Err(Fault::Crashed));
        assert!(babble.killer().pid().is_none());
    }

    #[test]
    #[cfg(unix)]
    fn a_kill_from_the_tab_does_not_wait_and_the_drop_reaps() {
        let worker = Worker::spawn(silent(VERSION), Duration::from_secs(5)).unwrap();
        let killer = worker.killer();
        let pid = killer.pid().unwrap();
        killer.kill();
        // sent, not reaped: the driver's to do
        assert_eq!(killer.pid(), Some(pid));
        drop(worker);
        assert!(killer.pid().is_none() && gone(pid));
    }
}
