//! Syntax highlighting for the viewer: syntect's parser with bat's syntaxes (`two-face`), no syntect theme. Each
//! scope is sorted into a handful of `Token`s that the viewer colours from the theme's terminal palette, so code
//! reads in the colours the panes beside it use and every Shepherd theme highlights without a theme of its own.
//!
//! The syntaxes load once, on first use; only the runtime's blocking pool calls in here (the load takes a good
//! tenth of a second, a highlight up to the budget's worth), never the main thread.
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

/// The most characters highlighted per file: the lines after it show plain. syntect parses a few MB a second with
/// fancy-regex; 400k keeps a large file under about a second of one background thread.
pub const BUDGET: usize = 400_000;
/// A line longer than this ends the highlighting (minified code, a data line): syntect's time on one line is not
/// cancellable, and its state after a skipped line would be wrong for the rest.
const MAX_LINE: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    Plain,
    Comment,
    String,
    Keyword,
    Number,
    Function,
    Type,
    Constant,
    Punct,
    Tag,
    Attr,
}

/// bat's syntaxes (a superset of Sublime's defaults: TOML, TypeScript, Dockerfile, …), loaded on first call.
fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

/// The syntax for `path`: by its whole name (`Makefile`, `Dockerfile`), its extension, then its first line (a
/// shebang, a modeline). Plain text has none.
pub fn syntax_for(path: &Path, first_line: &str) -> Option<&'static SyntaxReference> {
    let set = syntaxes();
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
    let found = set
        .find_syntax_by_extension(name)
        .or_else(|| (!ext.is_empty()).then(|| set.find_syntax_by_extension(ext).or_else(|| set.find_syntax_by_extension(&ext.to_ascii_lowercase()))).flatten())
        .or_else(|| set.find_syntax_by_first_line(first_line))?;
    (found.name != "Plain Text").then_some(found)
}

/// The scopes each `Token` starts with, tried innermost scope first.
struct Prefixes {
    comment: Scope,
    string: Scope,
    keyword: Scope,
    storage: Scope,
    storage_type: Scope,
    number: Scope,
    constant: Scope,
    function: [Scope; 2],
    types: [Scope; 4],
    tag: Scope,
    attr: Scope,
    punct: Scope,
    variable_language: Scope,
}

fn prefixes() -> &'static Prefixes {
    static P: OnceLock<Prefixes> = OnceLock::new();
    P.get_or_init(|| {
        let s = |name: &str| Scope::new(name).expect("a valid scope");
        Prefixes {
            comment: s("comment"),
            string: s("string"),
            keyword: s("keyword"),
            storage: s("storage"),
            storage_type: s("storage.type"),
            number: s("constant.numeric"),
            constant: s("constant"),
            function: [s("entity.name.function"), s("support.function")],
            types: [s("entity.name.type"), s("entity.name.class"), s("support.type"), s("support.class")],
            tag: s("entity.name.tag"),
            attr: s("entity.other.attribute-name"),
            punct: s("punctuation"),
            variable_language: s("variable.language"),
        }
    })
}

/// The token for text under `stack`: its innermost scope that says something. Punctuation only counts when no
/// outer scope does, so a string's quotes and a comment's `//` take the string's and the comment's colour.
pub fn classify(stack: &[Scope]) -> Token {
    let p = prefixes();
    let mut punct = false;
    for scope in stack.iter().rev() {
        let is = |prefix: &Scope| prefix.is_prefix_of(*scope);
        if is(&p.comment) {
            return Token::Comment;
        }
        if is(&p.string) {
            return Token::String;
        }
        if is(&p.punct) {
            punct = true;
            continue;
        }
        if punct {
            continue;
        }
        if is(&p.number) {
            return Token::Number;
        }
        if is(&p.constant) {
            return Token::Constant;
        }
        if p.function.iter().any(is) {
            return Token::Function;
        }
        if p.types.iter().any(is) {
            return Token::Type;
        }
        if is(&p.tag) {
            return Token::Tag;
        }
        if is(&p.attr) {
            return Token::Attr;
        }
        // `storage.type` is `fn`, `struct`, `let` in most grammars, but a type name (`int`) in C's: keyword either
        // way reads right
        if is(&p.keyword) || is(&p.storage) || is(&p.storage_type) || is(&p.variable_language) {
            return Token::Keyword;
        }
    }
    if punct { Token::Punct } else { Token::Plain }
}

/// One run of a line's bytes in one token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub token: Token,
}

/// The tokens of the lines highlighted, flat: one list of spans and where each line's begin, not a `Vec` per line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Highlights {
    spans: Vec<Span>,
    /// Line `i`'s spans are `spans[firsts[i]..firsts[i + 1]]`; one more entry than lines highlighted.
    firsts: Vec<u32>,
}

impl Highlights {
    /// How many lines were highlighted (the first ones; the rest draw plain).
    pub fn lines(&self) -> usize {
        self.firsts.len().saturating_sub(1)
    }

    /// Line `ix`'s spans, in order, `Plain` left out; none for a line not highlighted.
    pub fn line(&self, ix: usize) -> &[Span] {
        match (self.firsts.get(ix), self.firsts.get(ix + 1)) {
            (Some(&a), Some(&b)) => &self.spans[a as usize..b as usize],
            _ => &[],
        }
    }
}

/// The tokens of `lines` (byte ranges into each line): the first `budget_chars` characters' worth of lines, line ends
/// counted, fewer when `cancel` is set (another file opened) or a line is longer than `MAX_LINE`.
pub fn highlight<'a>(syntax: &SyntaxReference, lines: impl Iterator<Item = &'a str>, budget_chars: usize, cancel: &AtomicBool) -> Highlights {
    let set = syntaxes();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut out = Highlights { spans: Vec::new(), firsts: vec![0] };
    let mut spent = 0;
    let mut buf = String::new();
    for line in lines {
        spent += line.len() + 1;
        if cancel.load(Ordering::Relaxed) || spent > budget_chars || line.len() > MAX_LINE {
            break;
        }
        // the `_newlines` syntaxes match on the line end
        buf.clear();
        buf.push_str(line);
        buf.push('\n');
        let Ok(ops) = state.parse_line(&buf, set) else { break };
        let first = out.spans.len();
        let mut at = 0;
        let push = |start: usize, end: usize, token: Token, spans: &mut Vec<Span>| {
            let (start, end) = (start.min(line.len()) as u32, end.min(line.len()) as u32);
            if start >= end || token == Token::Plain {
                return;
            }
            let own = spans.len() > first;
            match spans.last_mut() {
                Some(last) if own && last.token == token && last.end == start => last.end = end,
                _ => spans.push(Span { start, end, token }),
            }
        };
        for (pos, op) in ops {
            if pos > at {
                push(at, pos, classify(stack.as_slice()), &mut out.spans);
                at = pos;
            }
            if stack.apply(&op).is_err() {
                break;
            }
        }
        push(at, buf.len(), classify(stack.as_slice()), &mut out.spans);
        out.firsts.push(out.spans.len() as u32);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token_at(spans: &[Span], byte: u32) -> Token {
        spans.iter().find(|s| s.start <= byte && byte < s.end).map_or(Token::Plain, |s| s.token)
    }

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn rust_has_keywords_strings_and_comments() {
        let syntax = syntax_for(Path::new("src/main.rs"), "").expect("rust");
        let text = lines(&["fn main() {", "    let x = \"x\"; // hi", "    let n = 42;", "}"]);
        let out = highlight(syntax, text.iter().map(String::as_str), BUDGET, &AtomicBool::new(false));
        assert_eq!(out.lines(), 4);
        assert_eq!(token_at(out.line(0), 0), Token::Keyword);
        assert_eq!(token_at(out.line(0), 3), Token::Function);
        assert_eq!(token_at(out.line(1), 12), Token::String, "the quote");
        assert_eq!(token_at(out.line(1), 13), Token::String);
        assert_eq!(token_at(out.line(1), 18), Token::Comment);
        assert_eq!(token_at(out.line(2), 13), Token::Number);
        assert!(out.line(4).is_empty());
    }

    #[test]
    fn syntaxes_come_from_names_extensions_and_shebangs() {
        assert_eq!(syntax_for(Path::new("Cargo.toml"), "").map(|s| s.name.as_str()), Some("TOML"));
        assert_eq!(syntax_for(Path::new("a.ts"), "").map(|s| s.name.as_str()), Some("TypeScript"));
        assert_eq!(syntax_for(Path::new("Makefile"), "").map(|s| s.name.as_str()), Some("Makefile"));
        assert_eq!(syntax_for(Path::new("run"), "#!/usr/bin/env python3").map(|s| s.name.as_str()), Some("Python"));
        assert!(syntax_for(Path::new("notes.txt"), "").is_none());
        assert!(syntax_for(Path::new("noext"), "hello").is_none());
    }

    #[test]
    fn the_budget_and_a_cancel_stop_it() {
        let syntax = syntax_for(Path::new("a.rs"), "").unwrap();
        let text: Vec<String> = (0..100).map(|i| format!("let a{i} = {i};")).collect();
        let out = highlight(syntax, text.iter().map(String::as_str), 50, &AtomicBool::new(false));
        assert!(out.lines() > 0 && out.lines() < 10, "{}", out.lines());
        assert_eq!(highlight(syntax, text.iter().map(String::as_str), BUDGET, &AtomicBool::new(true)).lines(), 0);
        // line ends count: 100 empty lines are 100 characters
        let empty = vec![String::new(); 100];
        assert_eq!(highlight(syntax, empty.iter().map(String::as_str), 50, &AtomicBool::new(false)).lines(), 50);
        let long = ["x".repeat(MAX_LINE + 1)];
        assert_eq!(highlight(syntax, long.iter().map(String::as_str), BUDGET, &AtomicBool::new(false)).lines(), 0);
        let one = highlight(syntax, std::iter::once(""), BUDGET, &AtomicBool::new(false));
        assert_eq!((one.lines(), one.line(0)), (1, &[][..]));
    }
}
