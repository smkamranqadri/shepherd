//! What the viewer makes of a file: text (markdown among it), an image, or something it will not show (binary, too
//! large), told from its name, its size and its first bytes. Every read is bounded: a text file over `MAX_TEXT` is
//! never read whole, an image over `MAX_IMAGE` bytes or `MAX_PIXELS` pixels (as its header says) is not decoded, so a stray multi-GB log
//! or a 30k × 30k PNG cannot take Shepherd's memory with it.
use std::io::Read;
use std::path::Path;

/// The most text the viewer reads: 5 MB, a 100k-line source file and then some (VS Code's own large-file line).
pub const MAX_TEXT: u64 = 5_000_000;
/// The largest image file the viewer hands to gpui.
pub const MAX_IMAGE: u64 = 32 << 20;
/// The most pixels an image may have once decoded (16 MP is 64 MB of RGBA), and the longest side.
pub const MAX_PIXELS: u64 = 16_000_000;
pub const MAX_SIDE: u32 = 8192;
/// The most lines the viewer takes (4 bytes of index each): 5 MB of empty lines would otherwise be 5M of them.
pub const MAX_LINES: usize = 1_000_000;
/// The largest SVG the viewer hands to gpui, which parses it whole (usvg) before drawing it.
pub const MAX_SVG: u64 = 4 << 20;
/// gpui draws an SVG at twice its size (`SMOOTH_SVG_SCALE_FACTOR`): its pixel budget is a quarter of a bitmap's.
const SVG_SCALE: u32 = 2;
/// How much of the file's head decides text or binary: git's own sniff (`buffer_is_binary`) reads 8000 bytes.
const HEAD: usize = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Text,
    Markdown,
    Image,
    /// Shown by a placeholder until the viewer draws PDFs itself (M4b), never handed to an outside app.
    Pdf,
    Binary,
    /// Its size in bytes: over `MAX_TEXT` for text, `MAX_IMAGE` for an image or a PDF.
    TooLarge(u64),
}

/// The formats gpui decodes, by extension (it sniffs the bytes itself; the name only picks the viewer).
pub fn is_image(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    matches!(ext.as_deref(), Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "svg" | "ico"))
}

pub fn is_pdf(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

pub fn is_markdown(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    matches!(ext.as_deref(), Some("md" | "markdown" | "mdown" | "mkd" | "mdx"))
}

/// What `path` is, from its `size` and the first bytes read of it (`head`, up to 8 KiB): an image or a PDF by its
/// name, else
/// binary when its head has a NUL or is not UTF-8 (a character cut at the end of the head is fine), else text.
pub fn kind_of(path: &Path, head: &[u8], size: u64) -> FileKind {
    if is_image(path) {
        return if size > MAX_IMAGE { FileKind::TooLarge(size) } else { FileKind::Image };
    }
    if is_pdf(path) {
        return if size > MAX_IMAGE { FileKind::TooLarge(size) } else { FileKind::Pdf };
    }
    let head = &head[..head.len().min(HEAD)];
    let utf8 = match std::str::from_utf8(head) {
        Ok(_) => true,
        // `error_len() == None`: the head stops inside a character
        Err(e) => e.error_len().is_none(),
    };
    if head.contains(&0) || !utf8 {
        return FileKind::Binary;
    }
    if size > MAX_TEXT {
        FileKind::TooLarge(size)
    } else if is_markdown(path) {
        FileKind::Markdown
    } else {
        FileKind::Text
    }
}

/// A text file: one string and where its lines start, not a `String` per line (a 5 MB file of short lines would
/// take several times that in allocations).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Text {
    /// The text as read, without its byte order mark; line ends kept.
    pub text: String,
    /// Each line's first byte in `text`. A final line end does not start another (empty) line; an empty file has
    /// one line.
    starts: Vec<u32>,
    /// Some line ended in CRLF (the header tags the file with it).
    pub crlf: bool,
    /// It started with a UTF-8 byte order mark.
    pub bom: bool,
    /// Its most common line end, which an edit is saved with.
    pub ending: super::save::LineEnding,
    /// It ends with a line end.
    pub final_newline: bool,
    /// It has more than one kind of line end (a save makes them all `ending`).
    pub mixed: bool,
}

impl Text {
    pub fn len(&self) -> usize {
        self.starts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.starts.is_empty()
    }

    /// Line `ix`, without its line end (`\n` or `\r\n`); empty past the last.
    pub fn line(&self, ix: usize) -> &str {
        let Some(&start) = self.starts.get(ix) else { return "" };
        let end = match self.starts.get(ix + 1) {
            Some(&next) => next as usize - 1,
            None => self.text.len() - usize::from(self.text.ends_with('\n')),
        };
        let line = &self.text[start as usize..end.max(start as usize)];
        line.strip_suffix('\r').unwrap_or(line)
    }

    pub fn lines(&self) -> impl Iterator<Item = &str> {
        (0..self.len()).map(|ix| self.line(ix))
    }
}

/// `bytes` as UTF-8 text; `None` when they are not UTF-8.
pub fn decode(bytes: &[u8]) -> Option<Text> {
    let bom = bytes.starts_with(b"\xEF\xBB\xBF");
    let text = std::str::from_utf8(if bom { &bytes[3..] } else { bytes }).ok()?.to_string();
    let mut starts = vec![0u32];
    starts.extend(text.bytes().enumerate().filter(|(i, b)| *b == b'\n' && i + 1 < text.len()).map(|(i, _)| i as u32 + 1));
    let crlf = text.contains("\r\n");
    let format = super::save::line_format(text.as_bytes());
    Some(Text { text, starts, crlf, bom, ending: format.ending, final_newline: format.final_newline, mixed: format.mixed })
}

/// A file as the viewer shows it, read by `load`.
#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    Text(Text),
    Markdown(Text),
    /// A bitmap within the budget, and its size in pixels (from its header).
    Image(u32, u32),
    /// An SVG within the budget, and its size in CSS pixels (its `width`/`height`, else its `viewBox`).
    Svg(u32, u32),
    /// A PDF within `MAX_IMAGE`; only its head was read.
    Pdf,
    Binary,
    TooLarge(u64),
    /// An image whose pixels are over `MAX_PIXELS` or a side over `MAX_SIDE`.
    TooManyPixels(u32, u32),
    /// Text of more than `MAX_LINES` lines.
    TooManyLines(usize),
    /// An image whose header does not say its size: it is not handed to a decoder that would find out by decoding.
    UnknownSize,
    /// A folder, gone, unreadable: what the viewer says instead.
    Failed(String),
}

/// Reads `path` for the viewer (blocking: the runtime's blocking pool runs it, under a timeout). Only the head is
/// read of a file that turns out binary or too large, and never more than `MAX_TEXT` + 1 bytes of any.
pub fn load(path: &Path) -> Content {
    let failed = |e: std::io::Error| {
        Content::Failed(match e.kind() {
            std::io::ErrorKind::NotFound => "This file no longer exists".into(),
            std::io::ErrorKind::PermissionDenied => "Permission denied".into(),
            _ => e.to_string(),
        })
    };
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return failed(e),
    };
    if meta.is_dir() {
        return Content::Failed("This is a folder".into());
    }
    if !meta.is_file() {
        // a FIFO or a device would block the read, or never end it
        return Content::Binary;
    }
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return failed(e),
    };
    let mut head = Vec::with_capacity(HEAD);
    if let Err(e) = (&mut file).take(HEAD as u64).read_to_end(&mut head) {
        return failed(e);
    }
    match kind_of(path, &head, meta.len()) {
        FileKind::Binary => Content::Binary,
        FileKind::Pdf => Content::Pdf,
        FileKind::TooLarge(n) => Content::TooLarge(n),
        FileKind::Image if is_svg(path) => {
            if meta.len() > MAX_SVG {
                return Content::TooLarge(meta.len());
            }
            match svg_size(&head) {
                Some((w, h)) if over_budget(w.saturating_mul(SVG_SCALE), h.saturating_mul(SVG_SCALE)) => Content::TooManyPixels(w, h),
                Some((w, h)) => Content::Svg(w, h),
                None => Content::UnknownSize,
            }
        }
        // the header's word, for every format gpui reads (the `image` crate here has only PNG's)
        FileKind::Image => match imagesize::size(path) {
            Ok(size) => {
                let (w, h) = (u32::try_from(size.width).unwrap_or(u32::MAX), u32::try_from(size.height).unwrap_or(u32::MAX));
                if over_budget(w, h) { Content::TooManyPixels(w, h) } else { Content::Image(w, h) }
            }
            Err(_) => Content::UnknownSize,
        },
        kind @ (FileKind::Text | FileKind::Markdown) => {
            let mut bytes = head;
            // the file may have grown since its size was read: one byte over the cap says so
            if let Err(e) = (&mut file).take(MAX_TEXT + 1 - bytes.len() as u64).read_to_end(&mut bytes) {
                return failed(e);
            }
            if bytes.len() as u64 > MAX_TEXT {
                return Content::TooLarge(bytes.len() as u64);
            }
            match decode(&bytes) {
                Some(text) if text.len() > MAX_LINES => Content::TooManyLines(text.len()),
                Some(text) if kind == FileKind::Markdown => Content::Markdown(text),
                Some(text) => Content::Text(text),
                None => Content::Binary,
            }
        }
    }
}

fn is_svg(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("svg"))
}

fn over_budget(w: u32, h: u32) -> bool {
    u64::from(w) * u64::from(h) > MAX_PIXELS || w.max(h) > MAX_SIDE || w == 0 || h == 0
}

/// An SVG's size from its root tag in `head`: `width` and `height` in pixels (bare or `px`), else its `viewBox`'s,
/// as usvg sizes it. Other units, percentages or a tag past the head: unknown.
pub fn svg_size(head: &[u8]) -> Option<(u32, u32)> {
    let text = String::from_utf8_lossy(head);
    let tag = text.find("<svg").map(|at| &text[at..])?;
    let tag = &tag[..tag.find('>')?];
    let attr = |name: &str| {
        let mut rest = tag;
        loop {
            let at = rest.find(name)?;
            let before = rest[..at].chars().next_back();
            rest = &rest[at + name.len()..];
            // a whole attribute name (not `stroke-width`), then `=` and a quoted value
            if before.is_some_and(char::is_whitespace)
                && let Some(value) = rest.trim_start().strip_prefix('=')
            {
                let value = value.trim_start();
                let quote = value.chars().next().filter(|q| *q == '"' || *q == '\'')?;
                let value = &value[1..];
                return Some(value[..value.find(quote)?].trim().to_string());
            }
        }
    };
    let px = |v: String| v.strip_suffix("px").unwrap_or(&v).trim().parse::<f64>().ok().filter(|n| n.is_finite() && *n > 0.);
    let size = |w: f64, h: f64| Some((w.ceil().min(f64::from(u32::MAX)) as u32, h.ceil().min(f64::from(u32::MAX)) as u32));
    if let (Some(w), Some(h)) = (attr("width").and_then(px), attr("height").and_then(px)) {
        return size(w, h);
    }
    let view: Vec<f64> = attr("viewBox")?.split(|c: char| c.is_whitespace() || c == ',').filter(|p| !p.is_empty()).filter_map(|p| p.parse().ok()).collect();
    match view[..] {
        [_, _, w, h] if w.is_finite() && h.is_finite() && w > 0. && h > 0. => size(w, h),
        _ => None,
    }
}

/// `bytes` as "1.2 MB" for the too-large message.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1000. && unit < UNITS.len() - 1 {
        size /= 1000.;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{size:.1} {}", UNITS[unit]) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn kind_comes_from_the_name_the_size_and_the_head() {
        let rs = PathBuf::from("a.rs");
        assert_eq!(kind_of(&rs, b"fn main() {}", 12), FileKind::Text);
        assert_eq!(kind_of(&rs, b"ab\0cd", 5), FileKind::Binary);
        assert_eq!(kind_of(&rs, b"\xff\xfe", 2), FileKind::Binary);
        // a character the head cut in two is still text
        assert_eq!(kind_of(&rs, "aé".as_bytes().split_at(2).0, 3), FileKind::Text);
        assert_eq!(kind_of(&rs, b"x", MAX_TEXT), FileKind::Text);
        assert_eq!(kind_of(&rs, b"x", MAX_TEXT + 1), FileKind::TooLarge(MAX_TEXT + 1));
        assert_eq!(kind_of(Path::new("README.md"), b"# hi", 4), FileKind::Markdown);
        assert_eq!(kind_of(Path::new("shot.PNG"), b"\x89PNG\0", 5), FileKind::Image);
        assert_eq!(kind_of(Path::new("shot.png"), b"", MAX_IMAGE + 1), FileKind::TooLarge(MAX_IMAGE + 1));
        // a PDF's head is binary, its name says what it is
        assert_eq!(kind_of(Path::new("paper.PDF"), b"%PDF-1.7\n\xe2\xe3\0", 13), FileKind::Pdf);
        assert_eq!(kind_of(Path::new("paper.pdf"), b"", MAX_IMAGE + 1), FileKind::TooLarge(MAX_IMAGE + 1));
        // a NUL past the head is not looked for
        let mut head = vec![b'a'; HEAD];
        head.push(0);
        assert_eq!(kind_of(&rs, &head, head.len() as u64), FileKind::Text);
    }

    #[test]
    fn decode_drops_the_bom_and_the_last_line_end_and_notes_crlf() {
        let lines = |t: &Text| t.lines().map(String::from).collect::<Vec<_>>();
        let t = decode(b"\xEF\xBB\xBFa\r\nb\r\n").unwrap();
        assert_eq!((lines(&t), t.crlf, t.bom), (vec!["a".to_string(), "b".to_string()], true, true));
        let t = decode(b"a\nb").unwrap();
        assert_eq!((lines(&t), t.crlf, t.bom), (vec!["a".to_string(), "b".to_string()], false, false));
        assert_eq!(lines(&decode(b"a\n\n").unwrap()), ["a", ""]);
        assert_eq!(lines(&decode(b"\n").unwrap()), [""]);
        assert_eq!(lines(&decode(b"").unwrap()), [""]);
        assert_eq!(decode(b"x\ny").unwrap().line(7), "");
        assert!(decode(b"\xff").is_none());
    }

    #[test]
    fn decode_records_how_lines_end() {
        use super::super::save::LineEnding;
        let format = |t: Text| (t.ending, t.bom, t.final_newline, t.mixed);
        assert_eq!(format(decode(b"\xEF\xBB\xBFa\r\nb\r\n").unwrap()), (LineEnding::Crlf, true, true, false));
        assert_eq!(format(decode(b"a\nb").unwrap()), (LineEnding::Lf, false, false, false));
        assert_eq!(format(decode(b"a\rb\r").unwrap()), (LineEnding::Cr, false, true, false));
        assert_eq!(format(decode(b"a\r\nb\r\nc\n").unwrap()), (LineEnding::Crlf, false, true, true));
        assert_eq!(format(decode(b"").unwrap()), (LineEnding::Lf, false, false, false));
    }

    #[test]
    fn load_reads_text_whole_and_stops_at_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "one\ntwo\n").unwrap();
        assert_eq!(load(&path), Content::Text(decode(b"one\ntwo\n").unwrap()));
        std::fs::write(&path, vec![b'a'; MAX_TEXT as usize + 1]).unwrap();
        assert_eq!(load(&path), Content::TooLarge(MAX_TEXT + 1));
        std::fs::write(&path, "\n".repeat(MAX_LINES + 1)).unwrap();
        assert_eq!(load(&path), Content::TooManyLines(MAX_LINES + 1));
        std::fs::write(&path, b"a\0b").unwrap();
        assert_eq!(load(&path), Content::Binary);
        let md = dir.path().join("b.md");
        std::fs::write(&md, "# t\n").unwrap();
        assert!(matches!(load(&md), Content::Markdown(_)));
        let pdf = dir.path().join("c.pdf");
        std::fs::write(&pdf, b"%PDF-1.4\n\0").unwrap();
        assert_eq!(load(&pdf), Content::Pdf);
        assert_eq!(load(dir.path()), Content::Failed("This is a folder".into()));
        assert_eq!(load(&dir.path().join("gone")), Content::Failed("This file no longer exists".into()));
    }

    #[test]
    fn an_image_over_the_pixel_budget_is_not_shown() {
        let dir = tempfile::tempdir().unwrap();
        let small = dir.path().join("s.png");
        image::RgbaImage::new(4, 3).save(&small).unwrap();
        assert_eq!(load(&small), Content::Image(4, 3));
        // not only PNGs: the header says the size of every format gpui reads
        let jpeg = dir.path().join("j.jpg");
        std::fs::write(&jpeg, jpeg_header(MAX_SIDE + 1, 2)).unwrap();
        assert_eq!(load(&jpeg), Content::TooManyPixels(MAX_SIDE + 1, 2));
        let junk = dir.path().join("x.webp");
        std::fs::write(&junk, b"not an image").unwrap();
        assert_eq!(load(&junk), Content::UnknownSize);
        // one side over the cap is enough: only the header is read
        let wide = dir.path().join("w.png");
        image::GrayImage::new(MAX_SIDE + 1, 1).save(&wide).unwrap();
        assert_eq!(load(&wide), Content::TooManyPixels(MAX_SIDE + 1, 1));
    }

    /// A baseline JPEG's start (SOI, SOF0) declaring `w` x `h`: enough for a header reader.
    fn jpeg_header(w: u32, h: u32) -> Vec<u8> {
        let (w, h) = (w as u16, h as u16);
        let mut b = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08];
        b.extend(h.to_be_bytes());
        b.extend(w.to_be_bytes());
        b.extend([0x03, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1, 0xFF, 0xD9]);
        b
    }

    #[test]
    fn svgs_are_sized_from_their_root_tag() {
        assert_eq!(svg_size(br#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" stroke-width="2" width="24px" height="16">"#), Some((24, 16)));
        assert_eq!(svg_size(br#"<svg viewBox="0 0 100.5 50" width="100%">"#), Some((101, 50)));
        assert_eq!(svg_size(br#"<svg width='10mm' height='4mm'>"#), None);
        assert_eq!(svg_size(b"<html>"), None);
        let dir = tempfile::tempdir().unwrap();
        let huge = dir.path().join("h.svg");
        std::fs::write(&huge, r#"<svg width="5000" height="5000"></svg>"#).unwrap();
        assert_eq!(load(&huge), Content::TooManyPixels(5000, 5000));
        let icon = dir.path().join("i.svg");
        std::fs::write(&icon, r#"<svg viewBox="0 0 24 24"></svg>"#).unwrap();
        assert_eq!(load(&icon), Content::Svg(24, 24));
    }

    #[test]
    fn sizes_read_as_people_write_them() {
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(5_000_001), "5.0 MB");
    }
}
