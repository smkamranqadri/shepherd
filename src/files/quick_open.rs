//! Quick Open's work without gpui: the root's files listed (rg, else git ls-files), the query read (its words, a
//! trailing `:line[:col]`), and the paths ranked by it, the best `k` kept, cancellable between keystrokes.
use crate::files::fuzzy::{Scratch, matched, score_ascii};
use crate::files::runner::{self, Cmd, Host, Limits, Output, RunError};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The most paths kept from a listing: a bigger tree shows these and says so.
pub const MAX_FILES: usize = 200_000;
/// Said under the list when a listing went past `MAX_FILES`.
pub const CAPPED: &str = "Showing the first 200,000 files";
/// Said when neither rg nor git can list the root.
pub const NO_TOOL: &str = "Install ripgrep, or open a git repository";
/// How many files a query shows.
pub const TOP_K: usize = 100;
/// A listing's deadline and what of its output is read: 200,000 paths fit in 32 MiB.
pub const LIMITS: Limits = Limits { timeout: Duration::from_secs(10), max_bytes: 32 << 20 };
/// How many paths are ranked between looks at the cancel flag.
const CANCEL_EVERY: usize = 4096;
/// A token found in the file's name outranks any found only in its folders.
const NAME_BONUS: i32 = 4000;

/// `rg --files` over `root`: hidden files too, the user's rg config left out, `.git` skipped, ignore files obeyed.
pub fn rg_cmd(root: &Path, program: &str) -> Cmd {
    let args = ["--files", "--hidden", "--no-config", "--glob", "!.git"];
    Cmd { program: program.to_string(), args: args.map(String::from).to_vec(), cwd: root.to_path_buf(), ..Default::default() }
}

/// `git ls-files` over `root` when rg is not installed: tracked and untracked files git does not ignore, NUL-split.
pub fn ls_files_cmd(root: &Path) -> Cmd {
    runner::git_cmd(root, &["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
}

/// A root's files, relative to it with `/` between folders, at most `MAX_FILES`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Listing {
    pub paths: Vec<Box<str>>,
    /// There were more than `MAX_FILES` (or more than `LIMITS` let through).
    pub capped: bool,
}

/// A listing's output split at `sep`: the piece after the last separator is dropped when the output was cut short;
/// paths that are not UTF-8 are skipped (they could not be opened by name); `backslash` (rg on Windows) turns `\`
/// into `/`.
pub fn parse(out: &[u8], sep: u8, truncated: bool, backslash: bool) -> Listing {
    let mut pieces: Vec<&[u8]> = out.split(|&b| b == sep).collect();
    if truncated || out.last().is_some_and(|&b| b == sep) {
        // the cut one, or the empty piece after the last separator
        pieces.pop();
    }
    let mut paths = Vec::new();
    let mut capped = truncated;
    for piece in pieces {
        let Ok(path) = std::str::from_utf8(piece) else { continue };
        let path = if backslash { path.replace('\\', "/") } else { path.to_string() };
        let path = path.strip_prefix("./").unwrap_or(&path);
        if path.is_empty() {
            continue;
        }
        if paths.len() == MAX_FILES {
            capped = true;
            break;
        }
        paths.push(Box::from(path));
    }
    Listing { paths, capped }
}

/// rg's answer: its files, a fall back to git when it is not installed (`None`), or what went wrong. rg exits 1 when
/// it finds no file and 2 when some folder could not be read, which still lists the rest.
fn from_rg(result: Result<Output, RunError>) -> Option<Result<Listing, String>> {
    match result {
        Err(RunError::NotFound(_)) => None,
        Err(e) => Some(Err(e.to_string())),
        Ok(out) if out.truncated || matches!(out.code, Some(0 | 1)) || (out.code == Some(2) && !out.stdout.is_empty()) => {
            Some(Ok(parse(&out.stdout, b'\n', out.truncated, cfg!(windows))))
        }
        Ok(out) => Some(Err(first_line(&out.stderr).unwrap_or_else(|| "rg failed".into()))),
    }
}

/// git's answer: its files, or `NO_TOOL` when it is not installed or the root is not in a repository.
fn from_git(result: Result<Output, RunError>) -> Result<Listing, String> {
    match result {
        Ok(out) if out.truncated || out.code == Some(0) => Ok(parse(&out.stdout, 0, out.truncated, false)),
        Ok(_) | Err(RunError::NotFound(_)) => Err(NO_TOOL.into()),
        Err(e) => Err(e.to_string()),
    }
}

fn first_line(text: &str) -> Option<String> {
    text.lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_string)
}

/// Lists `root`'s files with `rg` (the program to run), else git; runs on the runtime.
pub async fn list(host: &Host, root: &Path, rg: &str) -> Result<Listing, String> {
    match from_rg(runner::run(host, rg_cmd(root, rg), LIMITS).await) {
        Some(listed) => listed,
        None => from_git(runner::run(host, ls_files_cmd(root), LIMITS).await),
    }
}

/// What is typed, read: its words (lowercase; on Windows `\` as `/`), and the line and column after a trailing
/// `:12` or `:12:3`, which go to the file opened rather than into the match.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Query {
    pub tokens: Vec<String>,
    pub line: Option<u32>,
    pub col: Option<u32>,
}

pub fn parse_query(raw: &str, backslash: bool) -> Query {
    let mut text = raw.trim();
    let mut numbers: Vec<u32> = Vec::new();
    // at most two numbers, each after a colon, and something before them
    for _ in 0..2 {
        let Some((head, tail)) = text.rsplit_once(':') else { break };
        if head.is_empty() || tail.is_empty() || !tail.bytes().all(|b| b.is_ascii_digit()) {
            break;
        }
        let Ok(n) = tail.parse::<u32>() else { break };
        numbers.insert(0, n);
        text = head;
    }
    let (line, col) = match numbers[..] {
        [line] => (Some(line), None),
        [line, col] => (Some(line), Some(col)),
        _ => (None, None),
    };
    let tokens = text
        .split_whitespace()
        .map(|t| t.to_lowercase())
        .map(|t| if backslash { t.replace('\\', "/") } else { t })
        .collect();
    Query { tokens, line, col }
}

/// Whether `token` (lowercase) could match `text` at all: its bytes in order in `text`, ASCII case folded. Only a
/// shortcut before `matched`: a token or text that is not ASCII passes, since Unicode case folding (the Kelvin sign
/// is a `k`) is `matched`'s to decide.
pub fn may_match(token: &str, text: &str) -> bool {
    !token.is_ascii() || !text.is_ascii() || in_order(token.as_bytes(), text.as_bytes())
}

/// `want`'s bytes in order in `text`, ASCII case folded.
fn in_order(want: &[u8], text: &[u8]) -> bool {
    let mut rest = text;
    for &c in want {
        match rest.iter().position(|&b| b.to_ascii_lowercase() == c) {
            Some(at) => rest = &rest[at + 1..],
            None => return false,
        }
    }
    true
}

/// The file name of `path` (after its last `/`).
pub fn name_of(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// How well every one of `tokens` matches `path`; None when one does not. A token with a `/` is matched against the
/// whole path; any other against the file's name first (worth `NAME_BONUS` more), else the whole path.
pub fn score_path(tokens: &[String], path: &str, scratch: &mut Scratch) -> Option<i32> {
    let name = name_of(path);
    let ascii = path.is_ascii();
    let mut total = 0;
    for token in tokens {
        // `may_match`, the path's ASCII check made once
        let fast = ascii && token.is_ascii();
        if fast && !in_order(token.as_bytes(), path.as_bytes()) {
            return None;
        }
        // ASCII both: the same score, faster
        let score = |text: &str, scratch: &mut Scratch| match fast {
            true => score_ascii(token.as_bytes(), text.as_bytes(), scratch),
            false => matched(token, text).map(|(score, _)| score),
        };
        let in_name = (!token.contains('/') && may_match(token, name)).then(|| score(name, scratch)).flatten();
        total += match in_name {
            Some(score) => NAME_BONUS + score,
            None => score(path, scratch)?,
        };
    }
    Some(total)
}

/// The chars of a file's `name` that `query`'s words matched, for the bold in its row.
pub fn name_hits(query: &Query, name: &str) -> Vec<usize> {
    let mut hits: Vec<usize> = query.tokens.iter().filter(|t| !t.contains('/')).filter_map(|t| matched(t, name)).flat_map(|(_, at)| at).collect();
    hits.sort_unstable();
    hits.dedup();
    hits
}

/// Which heading a ranked file sits under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// A file tab of the shown workspace.
    Open,
    /// Opened from Quick Open this run (only while nothing is typed).
    Recent,
    Files,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranked {
    pub path: String,
    pub group: Group,
}

/// The better of two scored paths first: higher score, then the shorter path, then the path's order.
type Key<'a> = (i32, Reverse<usize>, Reverse<&'a str>);

fn key(score: i32, path: &str) -> Key<'_> {
    (score, Reverse(path.len()), Reverse(path))
}

/// `paths` ranked by `query`, the `open` tabs (relative paths, most recent first) ahead of the rest. With nothing
/// typed: the open tabs, then the `recent` files not open. Otherwise the open tabs that match, best first, then the
/// best `k` of `paths` that are not open. `None` once `cancel` is set (a newer keystroke's ranking took over).
pub fn rank(query: &Query, paths: &[Box<str>], open: &[String], recent: &[String], k: usize, cancel: &AtomicBool) -> Option<Vec<Ranked>> {
    let opened: HashSet<&str> = open.iter().map(String::as_str).collect();
    let ranked = |path: &str, group| Ranked { path: path.to_string(), group };
    if query.tokens.is_empty() {
        let recent = recent.iter().filter(|p| !opened.contains(p.as_str())).take(k);
        return Some(open.iter().map(|p| ranked(p, Group::Open)).chain(recent.map(|p| ranked(p, Group::Recent))).collect());
    }
    let mut scratch = Scratch::default();
    let mut open_hits: Vec<(Key, &str)> = open.iter().filter_map(|p| Some((key(score_path(&query.tokens, p, &mut scratch)?, p), p.as_str()))).collect();
    open_hits.sort_by(|a, b| b.0.cmp(&a.0));
    // the worst kept on top, so a better one replaces it
    let mut best: BinaryHeap<Reverse<Key>> = BinaryHeap::with_capacity(k + 1);
    for (i, path) in paths.iter().enumerate() {
        if i % CANCEL_EVERY == 0 && cancel.load(Ordering::Relaxed) {
            return None;
        }
        let Some(score) = score_path(&query.tokens, path, &mut scratch) else { continue };
        let candidate = key(score, path);
        if best.len() == k && best.peek().is_some_and(|Reverse(worst)| candidate <= *worst) {
            continue;
        }
        if opened.contains(&**path) {
            continue;
        }
        best.push(Reverse(candidate));
        if best.len() > k {
            best.pop();
        }
    }
    let files = best.into_sorted_vec().into_iter().map(|Reverse((_, _, Reverse(path)))| ranked(path, Group::Files));
    Some(open_hits.into_iter().map(|(_, p)| ranked(p, Group::Open)).chain(files).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(paths: &[&str]) -> Vec<Box<str>> {
        paths.iter().map(|p| Box::from(*p)).collect()
    }

    fn q(raw: &str) -> Query {
        parse_query(raw, false)
    }

    fn ranked(raw: &str, paths: &[&str], open: &[&str]) -> Vec<String> {
        let open: Vec<String> = open.iter().map(|p| p.to_string()).collect();
        rank(&q(raw), &boxed(paths), &open, &[], TOP_K, &AtomicBool::new(false)).unwrap().into_iter().map(|r| r.path).collect()
    }

    /// Fictional paths with every kind of case, separator and non-ASCII letter the matcher folds.
    fn sample_paths() -> Vec<String> {
        let mut out = Vec::new();
        let dirs = ["", "src/", "Src/Widgets/", "docs/guide-ÉTÉ/", "lib/K\u{212A}elvin/", "a_b/c-d/", "İstanbul/"];
        let names = ["main.rs", "FooBar.ts", "foo_bar.py", "README.md", "zzz", "Ünïcode.txt", "\u{212A}ilo.cfg", "x.y.z", "a b.txt"];
        for d in dirs {
            for n in names {
                out.push(format!("{d}{n}"));
            }
        }
        out
    }

    #[test]
    fn the_prefilter_never_drops_a_path_matched_would_keep() {
        let queries = ["m", "mr", "main", "src/", "fb", "foobar", "readme", "k", "ki", "kilo", "kelvin", "été", "é", "ü", "i", "is", "zz", "ab", "x.z", "/", "_", "-", "a b"];
        let (mut checked, mut scratch) = (0, Scratch::default());
        for path in sample_paths() {
            for token in queries.iter().flat_map(|q| q.split_whitespace()) {
                let token = token.to_lowercase();
                if matched(&token, &path).is_some() {
                    assert!(may_match(&token, &path), "{token:?} in {path:?}");
                    assert!(score_path(std::slice::from_ref(&token), &path, &mut scratch).is_some(), "ranked: {token:?} in {path:?}");
                    checked += 1;
                }
            }
        }
        assert!(checked > 100, "the sample matched enough to say something: {checked}");
        // and it does drop the rest
        assert!(!may_match("zq", "src/main.rs"));
        assert!(!may_match("nm", "main"), "in order");
    }

    #[test]
    fn every_word_must_match() {
        let paths = ["src/alpha/beta.rs", "src/alpha/gamma.rs", "lib/beta.rs", "Ünï/beta.rs"];
        assert_eq!(ranked("alpha beta", &paths, &[]), ["src/alpha/beta.rs"]);
        assert_eq!(ranked("beta alpha", &paths, &[]), ["src/alpha/beta.rs"], "in any order");
        assert_eq!(ranked("beta zzz", &paths, &[]), Vec::<String>::new(), "past the prefilter (Ünï) too");
    }

    #[test]
    fn the_name_beats_the_folders() {
        // "widget" names the second file and only starts the first's (shorter) path
        let paths = ["widget/z/quux.rs", "src/deep/widget.rs"];
        assert_eq!(ranked("widget", &paths, &[]), ["src/deep/widget.rs", "widget/z/quux.rs"]);
        // a word with a slash is the path's
        assert_eq!(ranked("z/q", &paths, &[]), ["widget/z/quux.rs"]);
    }

    #[test]
    fn letters_on_word_starts_beat_scattered_ones() {
        let paths = ["src/fab.rs", "src/foo_bar.rs"];
        assert_eq!(ranked("fb", &paths, &[]), ["src/foo_bar.rs", "src/fab.rs"]);
    }

    #[test]
    fn ties_go_to_the_shorter_path_then_its_order() {
        let paths = ["b/long/name.rs", "b/name.rs", "a/name.rs"];
        assert_eq!(ranked("name", &paths, &[]), ["a/name.rs", "b/name.rs", "b/long/name.rs"]);
    }

    #[test]
    fn a_trailing_line_and_column_go_to_the_file() {
        let read = |raw: &str| {
            let q = q(raw);
            (q.tokens.join(" "), q.line, q.col)
        };
        assert_eq!(read("main.rs:12"), ("main.rs".into(), Some(12), None));
        assert_eq!(read("main.rs:12:3"), ("main.rs".into(), Some(12), Some(3)));
        assert_eq!(read(" src main:7 "), ("src main".into(), Some(7), None));
        assert_eq!(read("main.rs:"), ("main.rs:".into(), None, None), "no number");
        assert_eq!(read("main.rs:1a"), ("main.rs:1a".into(), None, None));
        assert_eq!(read(":12"), (":12".into(), None, None), "nothing before it");
        assert_eq!(read("a:1:2:3"), ("a:1".into(), Some(2), Some(3)), "two numbers at most");
        assert_eq!(read("Main.RS"), ("main.rs".into(), None, None));
        assert_eq!(parse_query("src\\main", true).tokens, ["src/main"], "Windows' separator");
        assert_eq!(parse_query("src\\main", false).tokens, ["src\\main"]);
    }

    #[test]
    fn open_tabs_come_first_then_the_rest() {
        let paths = ["src/main.rs", "src/many.rs", "docs/mango.md"];
        // the open one matches worse (its letters apart), and still leads; it is not listed twice
        assert_eq!(ranked("man", &paths, &["src/main.rs"]), ["src/main.rs", "src/many.rs", "docs/mango.md"]);
        assert_eq!(ranked("zzz", &paths, &["docs/mango.md"]), Vec::<String>::new(), "an open tab that does not match is left out");
        // nothing typed: open tabs in their order, then recent ones not open
        let open = vec!["src/many.rs".to_string(), "src/main.rs".to_string()];
        let recent = vec!["src/main.rs".to_string(), "docs/mango.md".to_string()];
        let empty = rank(&q(" "), &boxed(&paths), &open, &recent, TOP_K, &AtomicBool::new(false)).unwrap();
        let rows: Vec<_> = empty.iter().map(|r| (r.path.as_str(), r.group)).collect();
        assert_eq!(rows, [("src/many.rs", Group::Open), ("src/main.rs", Group::Open), ("docs/mango.md", Group::Recent)]);
    }

    #[test]
    fn the_best_k_are_the_full_sort_s_first_k() {
        // many ties: names alike in folders of every length
        let paths: Vec<String> = (0..5000).map(|i| format!("{}/item{}.rs", "d".repeat(i % 7 + 1), i % 50)).collect();
        let boxed: Vec<Box<str>> = paths.iter().map(|p| Box::from(p.as_str())).collect();
        let query = q("item1");
        let mut scratch = Scratch::default();
        let mut all: Vec<(Key, &str)> = paths.iter().filter_map(|p| Some((key(score_path(&query.tokens, p, &mut scratch)?, p), p.as_str()))).collect();
        all.sort_by(|a, b| b.0.cmp(&a.0));
        let want: Vec<&str> = all.iter().take(TOP_K).map(|(_, p)| *p).collect();
        let got = rank(&query, &boxed, &[], &[], TOP_K, &AtomicBool::new(false)).unwrap();
        assert_eq!(got.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(), want);
        assert_eq!(got.len(), TOP_K);
    }

    #[test]
    fn a_cancelled_ranking_stops() {
        let paths = boxed(&["src/main.rs"]);
        assert_eq!(rank(&q("main"), &paths, &[], &[], TOP_K, &AtomicBool::new(true)), None);
        assert!(rank(&q("main"), &paths, &[], &[], TOP_K, &AtomicBool::new(false)).is_some());
    }

    #[test]
    fn rg_lines_and_ls_files_nuls_are_paths() {
        assert_eq!(parse(b"src/a.rs\nREADME.md\n", b'\n', false, false).paths, boxed(&["src/a.rs", "README.md"]));
        // cut short: the last, partial path goes
        let cut = parse(b"src/a.rs\nsrc/b.r", b'\n', true, false);
        assert_eq!((cut.paths, cut.capped), (boxed(&["src/a.rs"]), true));
        assert_eq!(parse(b"./x.rs\n", b'\n', false, false).paths, boxed(&["x.rs"]));
        assert_eq!(parse(b"src\\win\\a.rs\n", b'\n', false, true).paths, boxed(&["src/win/a.rs"]));
        assert_eq!(parse(b"odd\\name\n", b'\n', false, false).paths, boxed(&["odd\\name"]), "a backslash is a name's elsewhere");
        assert_eq!(parse(b"a b.txt\0dir/c\nd.txt\0\xff\xfe.bin\0", 0, false, false).paths, boxed(&["a b.txt", "dir/c\nd.txt"]), "not UTF-8: skipped");
        assert_eq!(parse(b"", b'\n', false, false), Listing::default());
    }

    #[test]
    fn a_listing_stops_at_the_cap() {
        let out: Vec<u8> = (0..MAX_FILES + 5).flat_map(|i| format!("f{i}\n").into_bytes()).collect();
        let listed = parse(&out, b'\n', false, false);
        assert_eq!((listed.paths.len(), listed.capped), (MAX_FILES, true));
        assert_eq!(listed.paths.last().map(|p| &**p), Some(format!("f{}", MAX_FILES - 1).as_str()));
        let exact: Vec<u8> = (0..MAX_FILES).flat_map(|i| format!("f{i}\n").into_bytes()).collect();
        assert!(!parse(&exact, b'\n', false, false).capped, "exactly the cap is all of them");
    }

    fn git(dir: &Path, args: &[&str]) {
        let mut cmd = std::process::Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                cmd.env_remove(key);
            }
        }
        let out = cmd.args(args).current_dir(dir).env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    #[tokio::test]
    async fn without_rg_git_lists_the_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        std::fs::create_dir_all(root.join("src")).unwrap();
        for f in ["src/one.rs", "two words.txt", "skipped.log", ".gitignore"] {
            std::fs::write(root.join(f), "x").unwrap();
        }
        std::fs::write(root.join(".gitignore"), "*.log\n").unwrap();
        git(root, &["add", "src/one.rs"]);
        let missing = "shepherd-test-no-such-rg";
        let mut listed = list(&Host::Local, root, missing).await.unwrap().paths;
        listed.sort();
        assert_eq!(listed, boxed(&[".gitignore", "src/one.rs", "two words.txt"]), "tracked and untracked, the ignored left out");
        // not a repository either
        let bare = tempfile::tempdir().unwrap();
        assert_eq!(list(&Host::Local, bare.path(), missing).await, Err(NO_TOOL.to_string()));
        assert_eq!(list(&Host::Ssh { target: "box".into() }, root, missing).await, Err(RunError::RemoteUnsupported.to_string()));
    }

    /// 200,000 fictional paths ranked on a few keystrokes; prints the time each took (run with `--ignored
    /// --nocapture`, best in release).
    #[test]
    #[ignore]
    fn ranking_200k_paths_takes() {
        let words = ["core", "util", "widget", "render", "parser", "net", "store", "model", "view", "test"];
        let exts = ["rs", "ts", "py", "md", "json"];
        let paths: Vec<Box<str>> = (0..MAX_FILES)
            .map(|i| {
                let (a, b, c) = (words[i % 10], words[(i / 10) % 10], words[(i / 100) % 10]);
                Box::from(format!("pkg{}/{a}/{b}_{c}/{c}{}_{a}.{}", i / 1000, i % 97, exts[i % 5]))
            })
            .collect();
        for raw in ["w", "wid", "widget", "rend view", "pkg12/core", "zzzz", "u_t.rs"] {
            let started = std::time::Instant::now();
            let got = rank(&q(raw), &paths, &[], &[], TOP_K, &AtomicBool::new(false)).unwrap();
            eprintln!("{raw:>12}: {:>6.1} ms, {} shown", started.elapsed().as_secs_f64() * 1000., got.len());
        }
    }
}
