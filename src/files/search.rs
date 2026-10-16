//! The Search tab's work without gpui: the query as rg's arguments, rg's `--json` lines (and git grep's and grep's,
//! when rg is not installed) as matches, the include and exclude globs as a filter on the paths, and an unsaved
//! buffer searched as rg would search its file.
//!
//! Include globs: rg's own `--glob` would do, but a positive one overrides the ignore files (a `node_modules` git
//! ignores comes back), and a `--type-add` glob matches the file's name only (`src/**/*.rs` matches nothing). So rg
//! gets each include's file-name part as a type when it is clearly a file's (`*.rs` of `src/**/*.rs`; none at all
//! when any part may be a folder's), and the full glob is checked here against the path of each match. Globs read
//! as VS Code's: at any depth unless `./` or `/` anchors them at the root, and a folder they match takes its
//! contents. Excludes go to rg as `--glob !x` in that same full form, and are checked here too (git grep and grep
//! have no such flag).
use base64::Engine;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use std::ops::Range;

/// What the Search tab asks: fixed text (never a regex), its case and whole-word switches, and the globs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    /// Match case (Aa); off, rg's `--ignore-case`.
    pub case: bool,
    /// Whole word (ab).
    pub word: bool,
    /// Comma-separated globs (`split_globs`): only paths one of them matches.
    pub include: String,
    /// Comma-separated globs: no path one of them matches.
    pub exclude: String,
}

/// One matching line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// From the root, `/` between folders.
    pub path: String,
    /// 1-based.
    pub line: u32,
    /// The line without its line end.
    pub text: String,
    /// The matches in `text`, byte offsets on char boundaries, in order.
    pub ranges: Vec<Range<usize>>,
}

/// One line of rg's `--json`, of the kinds the tab uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A file with matches starts.
    Begin { path: String },
    Match(Hit),
    /// The last line: how many matches rg found in all.
    Summary { matches: u64 },
}

/// rg's search: its deadline, what of its output is read, and the matches shown.
pub const LIMITS: crate::files::runner::Limits = crate::files::runner::Limits { timeout: std::time::Duration::from_secs(10), max_bytes: 5 << 20 };
pub const MAX_MATCHES: usize = 1000;
/// rg's lines longer than this (a minified bundle's match, as JSON) are dropped as they are read, so one cannot
/// fill `LIMITS`' cap on its own.
pub const MAX_RECORD: usize = 64 << 10;
/// A match row's text is cut to this many characters around its first match.
pub const MAX_LINE: usize = 300;

/// `s` split at its commas, except a comma inside `{…}` or `[…]` or escaped (`\,`, which becomes a plain comma);
/// each glob trimmed, empty ones dropped. Other escapes stay for the glob.
pub fn split_globs(s: &str) -> Vec<String> {
    let (mut globs, mut current, mut braces, mut bracket) = (Vec::new(), String::new(), 0usize, false);
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(',') => current.push(','),
                Some(next) => {
                    current.push('\\');
                    current.push(next);
                }
                None => current.push('\\'),
            },
            ',' if braces == 0 && !bracket => globs.push(std::mem::take(&mut current)),
            '[' if !bracket => {
                bracket = true;
                current.push(c);
            }
            ']' if bracket => {
                bracket = false;
                current.push(c);
            }
            '{' if !bracket => {
                braces += 1;
                current.push(c);
            }
            '}' if !bracket && braces > 0 => {
                braces -= 1;
                current.push(c);
            }
            _ => current.push(c),
        }
    }
    globs.push(current);
    globs.into_iter().map(|g| g.trim().to_string()).filter(|g| !g.is_empty()).collect()
}

/// The byte index of `glob`'s last `/` outside `{…}` and `[…]`.
fn last_slash(glob: &str) -> Option<usize> {
    let (mut braces, mut bracket, mut escaped, mut at) = (0usize, false, false, None);
    for (i, c) in glob.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '[' if !bracket => bracket = true,
            ']' if bracket => bracket = false,
            '{' if !bracket => braces += 1,
            '}' if !bracket => braces = braces.saturating_sub(1),
            '/' if braces == 0 && !bracket => at = Some(i),
            _ => {}
        }
    }
    at
}

/// `glob` as a glob from the root, as VS Code's search reads one: a leading `./` or `/` anchors it at the root,
/// anything else may match at any depth (`**/` before it); a trailing `/` says it is a folder. The glob, whether it
/// is a folder's, and whether it is anchored; `None` for the root itself (`./`, `/`).
fn rooted(glob: &str) -> Option<(String, bool, bool)> {
    let (glob, anchored) = match glob.strip_prefix("./").or_else(|| glob.strip_prefix('/')) {
        Some(rest) => (rest.trim_start_matches('/'), true),
        None => (glob, false),
    };
    let (glob, dir) = match glob.strip_suffix('/') {
        Some(d) => (d.trim_end_matches('/'), true),
        None => (glob, false),
    };
    if glob.is_empty() {
        return None;
    }
    let base = if anchored || glob == "**" || glob.starts_with("**/") { glob.to_string() } else { format!("**/{glob}") };
    Some((base, dir, anchored))
}

/// The full-path globs an include or exclude means (`rooted`): what it matches, and the contents of a folder it
/// matches (`src/ui` takes `src/ui/a.rs`, `test*` takes `tests/a.rs`); a folder's glob (`dir/`) its contents only.
/// The root itself (`./`) is everything. Whether they are anchored at the root.
fn full_globs(glob: &str) -> (Vec<String>, bool) {
    match rooted(glob) {
        None => (vec!["**".into()], true),
        Some((base, true, anchored)) => (vec![format!("{base}/**")], anchored),
        Some((base, false, anchored)) => (vec![format!("{base}/**"), base], anchored),
    }
}

/// `*.ext` (or `*.{rs,toml}`): clearly a file's name, not a folder's.
fn is_file_pattern(name: &str) -> bool {
    name.strip_prefix("*.").is_some_and(|ext| !ext.is_empty() && !ext.contains(['*', '?', '[', '/']))
}

/// The file-name part rg is given for an include (`--type-add`, which matches names only): the last part when it is
/// clearly a file pattern (`*.rs` of `src/**/*.rs`), else `*`, as any other part may be a folder's name (`src/ui`,
/// `vendor`, `test*`, `src/{a,b}`) and every name under it must reach the full-path check.
pub fn name_part(glob: &str) -> String {
    let Some((base, false, _)) = rooted(glob) else { return "*".into() };
    let name = match last_slash(&base) {
        Some(at) => &base[at + 1..],
        None => &base,
    };
    if is_file_pattern(name) { name.to_string() } else { "*".into() }
}

fn glob_set(globs: &[String]) -> Result<Option<GlobSet>, String> {
    if globs.is_empty() {
        return Ok(None);
    }
    let mut set = GlobSetBuilder::new();
    for glob in globs {
        for full in full_globs(glob).0 {
            let built = GlobBuilder::new(&full).literal_separator(true).backslash_escape(true).build().map_err(|e| format!("{glob}: {}", e.kind()))?;
            set.add(built);
        }
    }
    set.build().map(Some).map_err(|e| e.to_string())
}

/// The include and exclude globs, as checked against each match's path.
#[derive(Debug, Clone, Default)]
pub struct PathFilter {
    include: Option<GlobSet>,
    exclude: Option<GlobSet>,
}

impl PathFilter {
    /// The globs of `q`, or why one is not a glob (`src/{a`).
    pub fn new(q: &Query) -> Result<PathFilter, String> {
        Ok(PathFilter { include: glob_set(&split_globs(&q.include))?, exclude: glob_set(&split_globs(&q.exclude))? })
    }

    /// `path` (from the root, `/` between folders) is searched: an include matches it (or there are none), no
    /// exclude does, and it is not in `.git`.
    pub fn allows(&self, path: &str) -> bool {
        if path == ".git" || path.starts_with(".git/") || path.contains("/.git/") {
            return false;
        }
        self.include.as_ref().is_none_or(|s| s.is_match(path)) && !self.exclude.as_ref().is_some_and(|s| s.is_match(path))
    }
}

/// rg's arguments for `q`, run in the root: fixed strings, hidden files but not `.git`, the ignore files obeyed, the
/// query after `--` so one that starts with `-` is text.
pub fn rg_args(q: &Query) -> Vec<String> {
    let mut args: Vec<String> = ["--json", "--no-config", "--fixed-strings"].map(String::from).to_vec();
    args.push(if q.case { "--case-sensitive" } else { "--ignore-case" }.into());
    if q.word {
        args.push("--word-regexp".into());
    }
    args.extend(["--hidden", "--glob", "!.git"].map(String::from));
    let names: Vec<String> = split_globs(&q.include).iter().map(|g| name_part(g)).collect();
    // a name part of `*` takes every name: no type at all, the full globs do the filtering
    if !names.is_empty() && !names.iter().any(|n| n == "*") {
        let mut seen = std::collections::HashSet::new();
        for name in names.into_iter().filter(|n| seen.insert(n.clone())) {
            args.push("--type-add".into());
            args.push(format!("shepherdinclude:{name}"));
        }
        args.extend(["--type", "shepherdinclude"].map(String::from));
    }
    // the same full-path globs as the filter checks, so a folder is skipped rather than walked; rg anchors one with
    // a leading `/`, and one starting `**/` matches at any depth
    for glob in split_globs(&q.exclude) {
        let (globs, anchored) = full_globs(&glob);
        for full in globs {
            args.push("--glob".into());
            args.push(if anchored { format!("!/{full}") } else { format!("!{full}") });
        }
    }
    // a minified bundle or a database dump is no source to read through
    args.extend(["--max-filesize".into(), "50M".into()]);
    args.extend(["--".into(), q.text.clone(), ".".into()]);
    args
}

/// git grep's arguments for `q` (rg missing), in the root: tracked and untracked files git does not ignore, binary
/// files skipped, `path\0line\0column\0text` per match.
pub fn git_grep_args(q: &Query) -> Vec<String> {
    let mut args: Vec<String> = ["grep", "--untracked", "-z", "-n", "--column", "-I", "--no-color", "-F"].map(String::from).to_vec();
    if !q.case {
        args.push("-i".into());
    }
    if q.word {
        args.push("-w".into());
    }
    args.extend(["-e".into(), q.text.clone(), "--".into()]);
    args
}

/// grep's arguments for `q` (neither rg nor a repository): `./path\0line:text` per match, `.git` left out.
pub fn grep_args(q: &Query) -> Vec<String> {
    let mut args: Vec<String> = ["-r", "-n", "-I", "-F", "--null", "--exclude-dir=.git"].map(String::from).to_vec();
    if !q.case {
        args.push("-i".into());
    }
    if q.word {
        args.push("-w".into());
    }
    args.extend(["-e".into(), q.text.clone(), "--".into(), ".".into()]);
    args
}

/// A path as rg or grep prints it, from the root: no `./`, `/` between folders.
pub fn clean_path(path: &str) -> String {
    let path = path.strip_prefix("./").or_else(|| path.strip_prefix(".\\")).unwrap_or(path);
    if cfg!(windows) { path.replace('\\', "/") } else { path.to_string() }
}

/// rg's `{"text": …}` or `{"bytes": <base64>}` (a path or line that is not UTF-8), as bytes.
fn data(v: &serde_json::Value) -> Option<Vec<u8>> {
    if let Some(text) = v.get("text").and_then(|t| t.as_str()) {
        return Some(text.as_bytes().to_vec());
    }
    base64::engine::general_purpose::STANDARD.decode(v.get("bytes")?.as_str()?).ok()
}

/// `bytes` lossily as text, `ranges` (byte offsets in `bytes`) moved to match it.
fn lossy(bytes: &[u8], ranges: &[Range<usize>]) -> (String, Vec<Range<usize>>) {
    match std::str::from_utf8(bytes) {
        Ok(text) => {
            let fix = |at: usize| {
                let mut at = at.min(text.len());
                while !text.is_char_boundary(at) {
                    at -= 1;
                }
                at
            };
            (text.to_string(), ranges.iter().map(|r| fix(r.start)..fix(r.end)).collect())
        }
        Err(_) => {
            let at = |i: usize| String::from_utf8_lossy(&bytes[..i.min(bytes.len())]).len();
            (String::from_utf8_lossy(bytes).into_owned(), ranges.iter().map(|r| at(r.start)..at(r.end)).collect())
        }
    }
}

/// `text` without its line end (`\n`, `\r\n`), `ranges` kept inside it.
fn trim_line_end(mut text: String, ranges: Vec<Range<usize>>) -> (String, Vec<Range<usize>>) {
    while text.ends_with(['\n', '\r']) {
        text.pop();
    }
    let len = text.len();
    let ranges = ranges.into_iter().map(|r| r.start.min(len)..r.end.min(len)).filter(|r| !r.is_empty()).collect();
    (text, ranges)
}

/// One line of rg's `--json`: a file's start, a match, or the summary; `None` for the rest (`end`, `context`) and
/// what does not parse.
pub fn parse_rg_line(line: &[u8]) -> Option<Event> {
    let v: serde_json::Value = serde_json::from_slice(line).ok()?;
    let d = v.get("data")?;
    let path = || data(d.get("path")?).map(|p| clean_path(&String::from_utf8_lossy(&p)));
    match v.get("type")?.as_str()? {
        "begin" => Some(Event::Begin { path: path()? }),
        "summary" => Some(Event::Summary { matches: d.get("stats")?.get("matches")?.as_u64()? }),
        "match" => {
            let bytes = data(d.get("lines")?)?;
            let ranges: Vec<Range<usize>> = d
                .get("submatches")?
                .as_array()?
                .iter()
                .filter_map(|m| Some(m.get("start")?.as_u64()? as usize..m.get("end")?.as_u64()? as usize))
                .collect();
            let (text, ranges) = lossy(&bytes, &ranges);
            let (text, ranges) = trim_line_end(text, ranges);
            Some(Event::Match(Hit { path: path()?, line: d.get("line_number")?.as_u64()? as u32, text, ranges }))
        }
        _ => None,
    }
}

/// How a search ended, from its exit code and the tail of its stderr: rg's 0 (matches) and 1 (none) are fine; its
/// 2 with places it could not read (`problems`) found what it could; anything else failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    Ok,
    Problems(Vec<String>),
    Failed(String),
}

pub fn ended(code: Option<i32>, stderr: &str) -> Ended {
    match code {
        Some(0 | 1) => Ended::Ok,
        Some(2) => {
            let problems = problems(stderr);
            if problems.is_empty() { Ended::Failed(first_line(stderr, "rg failed")) } else { Ended::Problems(problems) }
        }
        Some(code) => Ended::Failed(first_line(stderr, &format!("rg exited with {code}"))),
        None => Ended::Failed("rg was stopped".into()),
    }
}

fn first_line(stderr: &str, otherwise: &str) -> String {
    let line = stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or(otherwise);
    line.strip_prefix("rg: ").unwrap_or(line).to_string()
}

/// The places rg could not read, from its stderr (`rg: ./private: Permission denied (os error 13)`), as
/// `private: no permission`. Only lines naming an OS error are places; a bad glob's complaint is not one.
pub fn problems(stderr: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in stderr.lines() {
        let line = line.trim();
        let Some(at) = line.find(" (os error ") else { continue };
        let line = &line[..at];
        let line = line.strip_prefix("rg: ").unwrap_or(line);
        let Some((path, message)) = line.split_once(": ") else { continue };
        // "IO error for operation on x: No such file or directory": the reason is last
        let reason = message.rsplit(": ").next().unwrap_or(message);
        let reason = match reason {
            "Permission denied" => "no permission".to_string(),
            "No such file or directory" => "no longer there".to_string(),
            r => {
                let mut r = r.to_string();
                if let Some(first) = r.get(..1) {
                    let lower = first.to_lowercase();
                    r.replace_range(..1, &lower);
                }
                r
            }
        };
        let problem = format!("{}: {reason}", clean_path(path));
        if !out.contains(&problem) {
            out.push(problem);
        }
    }
    out
}

/// One record of git grep's `-z -n --column` output: `path\0line\0column\0text`. The ranges are found in the text
/// again (git prints only the first match's column), falling back to that column.
pub fn parse_git_grep(record: &[u8], q: &Query) -> Option<Hit> {
    let mut parts = record.splitn(4, |&b| b == 0);
    let (path, line, column, text) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let line: u32 = std::str::from_utf8(line).ok()?.parse().ok()?;
    let column: usize = std::str::from_utf8(column).ok()?.parse().ok()?;
    let start = column.saturating_sub(1);
    let fallback = start..(start + q.text.len()).min(text.len());
    found(String::from_utf8_lossy(path).into_owned(), line, text, std::slice::from_ref(&fallback), q)
}

/// One record of grep's `-n --null` output: `./path\0line:text`.
pub fn parse_grep(record: &[u8], q: &Query) -> Option<Hit> {
    let nul = record.iter().position(|&b| b == 0)?;
    let (path, rest) = (&record[..nul], &record[nul + 1..]);
    let colon = rest.iter().position(|&b| b == b':')?;
    let line: u32 = std::str::from_utf8(&rest[..colon]).ok()?.parse().ok()?;
    found(clean_path(&String::from_utf8_lossy(path)), line, &rest[colon + 1..], &[], q)
}

/// A match git grep or grep reported, its ranges found again. When they cannot be, and the match was a whole word's
/// or a case-folded one, it is dropped: git grep runs under `LC_ALL=C`, where a byte of `é` is no word character
/// and case folding is ASCII's, so it reports lines this text search (and rg) would not.
fn found(path: String, line: u32, text: &[u8], fallback: &[Range<usize>], q: &Query) -> Option<Hit> {
    let (text, fallback) = lossy(text, fallback);
    let (text, fallback) = trim_line_end(text, fallback);
    let ranges = find_ranges(&text, &q.text, q.case, q.word);
    let ranges = match ranges.is_empty() {
        true if q.word || !q.case => return None,
        true => fallback,
        false => ranges,
    };
    Some(Hit { path, line, text, ranges })
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Where `needle` (in full) starts `text[at..]`: the end of the match.
fn match_at(text: &str, at: usize, needle: &str, case: bool) -> Option<usize> {
    let rest = &text[at..];
    if case {
        return rest.starts_with(needle).then(|| at + needle.len());
    }
    if needle.is_ascii() {
        let n = needle.len();
        return (rest.len() >= n && rest.as_bytes()[..n].eq_ignore_ascii_case(needle.as_bytes()) && rest.is_char_boundary(n)).then(|| at + n);
    }
    let mut chars = rest.char_indices();
    for n in needle.chars() {
        let (_, c) = chars.next()?;
        if c != n && !c.to_lowercase().eq(n.to_lowercase()) {
            return None;
        }
    }
    Some(at + chars.next().map_or(rest.len(), |(i, _)| i))
}

/// The matches of `needle` in `text`, as rg's fixed-string search finds them: left to right, not overlapping,
/// `case` exact or not, `word` with no word character either side.
pub fn find_ranges(text: &str, needle: &str, case: bool, word: bool) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    if needle.is_empty() {
        return out;
    }
    let mut at = 0;
    while at < text.len() {
        if let Some(end) = match_at(text, at, needle, case) {
            let before = text[..at].chars().next_back().is_some_and(is_word);
            let after = text[end..].chars().next().is_some_and(is_word);
            if !word || (!before && !after) {
                out.push(at..end);
                at = end.max(at + 1);
                while !text.is_char_boundary(at) {
                    at += 1;
                }
                continue;
            }
        }
        at += text[at..].chars().next().map_or(1, char::len_utf8);
    }
    out
}

/// The matching lines of an unsaved buffer `text` (the file at `path`), at most `budget` of them.
pub fn search_text(path: &str, text: &str, q: &Query, budget: usize) -> Vec<Hit> {
    let mut hits = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        if hits.len() >= budget {
            break;
        }
        let line = line.strip_suffix('\r').unwrap_or(line);
        let ranges = find_ranges(line, &q.text, q.case, q.word);
        if !ranges.is_empty() {
            hits.push(Hit { path: path.to_string(), line: i as u32 + 1, text: line.to_string(), ranges });
        }
    }
    hits
}

/// A match's line as its row shows it: leading blanks gone, and a long line cut to `MAX_LINE` characters a little
/// before its first match (`…` where it was cut); `ranges` moved to match.
pub fn excerpt(text: &str, ranges: &[Range<usize>]) -> (String, Vec<Range<usize>>) {
    let blank = text.len() - text.trim_start().len();
    let first = ranges.first().map_or(0, |r| r.start);
    // the blanks go only while no match is in them
    let skip = blank.min(first);
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let start_char = chars.iter().position(|&(i, _)| i >= skip).unwrap_or(chars.len());
    let count = chars.len() - start_char;
    let (from, to, lead) = if count <= MAX_LINE {
        (skip, text.len(), false)
    } else {
        let first_char = chars.iter().position(|&(i, _)| i >= first).unwrap_or(chars.len());
        // some context before the match, the rest of the room after it
        let from_char = first_char.saturating_sub(40).max(start_char).min(chars.len().saturating_sub(MAX_LINE));
        let to_char = (from_char + MAX_LINE).min(chars.len());
        let to = chars.get(to_char).map_or(text.len(), |&(i, _)| i);
        (chars[from_char].0, to, from_char > start_char)
    };
    let prefix = if lead { "…" } else { "" };
    let suffix = if to < text.len() { "…" } else { "" };
    let shown = format!("{prefix}{}{suffix}", &text[from..to]);
    let shift = prefix.len();
    let ranges = ranges
        .iter()
        .filter(|r| r.end > from && r.start < to)
        .map(|r| r.start.max(from) - from + shift..r.end.min(to) - from + shift)
        .collect();
    (shown, ranges)
}

#[cfg(test)]
// a match is a range; one match is a list of one
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    fn q(text: &str) -> Query {
        Query { text: text.into(), ..Default::default() }
    }

    #[test]
    fn globs_split_at_commas_outside_braces_brackets_and_escapes() {
        assert_eq!(split_globs(r"*.rs, src/{a,b}/*, \,x"), ["*.rs", "src/{a,b}/*", ",x"]);
        assert_eq!(split_globs(" , ,, "), Vec::<String>::new());
        assert_eq!(split_globs(""), Vec::<String>::new());
        assert_eq!(split_globs("[,a]*,b"), ["[,a]*", "b"]);
        assert_eq!(split_globs(r"a\*b,c"), [r"a\*b", "c"]);
        assert_eq!(split_globs("{a,{b,c}},d"), ["{a,{b,c}}", "d"]);
    }

    fn allows(include: &str, exclude: &str, path: &str) -> bool {
        PathFilter::new(&Query { include: include.into(), exclude: exclude.into(), ..q("x") }).unwrap().allows(path)
    }

    #[test]
    fn includes_take_the_full_path() {
        // a file-name glob anywhere
        assert!(allows("*.rs", "", "main.rs") && allows("*.rs", "", "src/ui/a.rs"));
        assert!(!allows("*.rs", "", "src/a.rs.bak"));
        // `**/` reaches the root's own files too
        assert!(allows("**/*.rs", "", "a.rs") && allows("**/*.rs", "", "x/y/a.rs"));
        // with a slash, as VS Code: at any depth, unless `./` or `/` anchors it at the root
        assert!(allows("src/**/*.rs", "", "src/a.rs") && allows("src/**/*.rs", "", "src/x/a.rs"));
        assert!(allows("src/**/*.rs", "", "lib/src/a.rs") && !allows("src/**/*.rs", "", "lib/a.rs"));
        assert!(allows("./src/**/*.rs", "", "src/a.rs") && !allows("./src/**/*.rs", "", "lib/src/a.rs"));
        assert!(allows("/src/*.rs", "", "src/a.rs") && !allows("/src/*.rs", "", "x/src/a.rs"));
        // a folder the glob matches takes its contents with it (`src/x` of `src/*`)
        assert!(allows("src/*", "", "src/a.rs") && allows("src/*", "", "src/x/a.rs") && !allows("src/*", "", "lib/a.rs"));
        assert!(allows("src/{a,b}/*", "", "src/b/f") && !allows("src/{a,b}/*", "", "src/c/f"));
        // a folder named without a trailing `/`
        assert!(allows("/src", "", "src/a.rs") && !allows("/src", "", "x/src/a.rs"));
        assert!(allows("src/ui", "", "src/ui/a.rs") && allows("src/ui", "", "app/src/ui/deep/b.rs") && !allows("src/ui", "", "src/uix/a.rs"));
        assert!(allows("a/vendor", "", "a/vendor/x.c") && allows("src/{a,b}", "", "src/b/f.rs"));
        // wildcards without a slash take a folder's contents too
        assert!(allows("test*", "", "tests/a.rs") && allows("test*", "", "x/testing.rs"));
        assert!(allows("*.d", "", "etc/conf.d/x.conf"));
        // `dir/`: a folder of that name anywhere, its contents at any depth
        assert!(allows("src/", "", "src/a.rs") && allows("src/", "", "x/src/y/a.rs"));
        assert!(!allows("src/", "", "srcs/a.rs") && !allows("src/", "", "src"));
        assert!(allows("a/src/", "", "a/src/f") && allows("a/src/", "", "b/a/src/f"));
        assert!(allows("/src/", "", "src/f") && !allows("/src/", "", "x/src/f"));
        assert!(allows("./", "", "anything/at/all"));
        // a plain name: a file or a folder
        assert!(allows("Makefile", "", "x/Makefile") && allows("vendor", "", "vendor/a.c"));
        // several: any of them
        assert!(allows("*.md, src/", "", "README.md") && allows("*.md, src/", "", "src/a.rs") && !allows("*.md, src/", "", "a.rs"));
        // excludes the same way, and over includes
        assert!(!allows("", "target/", "target/debug/x") && allows("", "target/", "src/target.rs"));
        assert!(!allows("*.rs", "src/gen/", "src/gen/a.rs") && !allows("", "src/gen", "lib/src/gen/a.rs"));
        assert!(!allows("", "", ".git/config") && !allows("", "", "sub/.git/HEAD"));
        assert!(PathFilter::new(&Query { include: "{a".into(), ..q("x") }).unwrap_err().starts_with("{a:"));
    }

    #[test]
    fn rg_gets_only_file_name_parts_as_types() {
        assert_eq!(name_part("src/**/*.rs"), "*.rs");
        assert_eq!(name_part("*.{rs,toml}"), "*.{rs,toml}");
        assert_eq!(name_part("./x/*.c"), "*.c");
        assert_eq!(name_part("/x/*.c"), "*.c");
        // anything that may name a folder: every name
        for glob in ["src/", "src/**", "src/{a,b}/*", "{a/b,c}.rs", "vendor", "/src", "src/ui", "a/vendor", "src/{a,b}", "test*", "a?c", "*", "./", "Makefile"] {
            assert_eq!(name_part(glob), "*", "{glob}");
        }
    }

    fn has_pair(args: &[String], a: &str, b: &str) -> bool {
        args.windows(2).any(|w| w[0] == a && w[1] == b)
    }

    #[test]
    fn rg_args_follow_the_options() {
        let plain = rg_args(&q("foo"));
        assert_eq!(plain[..4], ["--json", "--no-config", "--fixed-strings", "--ignore-case"]);
        assert!(has_pair(&plain, "--glob", "!.git") && plain.contains(&"--hidden".to_string()));
        assert!(!plain.contains(&"--word-regexp".to_string()) && !plain.contains(&"--type".to_string()));
        assert_eq!(plain[plain.len() - 3..], ["--", "foo", "."]);
        let all = rg_args(&Query { text: "foo".into(), case: true, word: true, include: "src/**/*.rs, *.toml, *.rs".into(), exclude: "target/, *.lock".into() });
        assert!(all.contains(&"--case-sensitive".to_string()) && !all.contains(&"--ignore-case".to_string()));
        assert!(all.contains(&"--word-regexp".to_string()));
        assert!(has_pair(&all, "--type-add", "shepherdinclude:*.rs") && has_pair(&all, "--type-add", "shepherdinclude:*.toml"));
        assert_eq!(all.iter().filter(|a| *a == "--type-add").count(), 2, "one type per name");
        assert!(has_pair(&all, "--type", "shepherdinclude"));
        assert!(has_pair(&all, "--glob", "!**/target/**") && has_pair(&all, "--glob", "!**/*.lock") && has_pair(&all, "--glob", "!**/*.lock/**"));
        let anchored = rg_args(&Query { exclude: "./gen, /out/".into(), ..q("x") });
        assert!(has_pair(&anchored, "--glob", "!/gen") && has_pair(&anchored, "--glob", "!/gen/**") && has_pair(&anchored, "--glob", "!/out/**"));
        assert!(has_pair(&all, "--max-filesize", "50M"));
        // never a positive --glob: it would override the ignore files
        assert!(all.windows(2).filter(|w| w[0] == "--glob").all(|w| w[1].starts_with('!')));
        // an include that takes every name: no type
        let folder = rg_args(&Query { include: "*.rs, docs/".into(), ..q("x") });
        assert!(!folder.contains(&"--type".to_string()) && !folder.contains(&"--type-add".to_string()));
    }

    #[test]
    fn a_query_like_a_flag_stays_text() {
        let args = rg_args(&q("-e"));
        let dashes = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(args[dashes + 1..], ["-e", "."]);
        assert!(!args[..dashes].contains(&"-e".to_string()));
        let git = git_grep_args(&q("-e"));
        assert!(has_pair(&git, "-e", "-e"));
        let grep = grep_args(&q("--help"));
        assert!(has_pair(&grep, "-e", "--help"));
    }

    #[test]
    fn rg_json_lines_parse() {
        let begin = br#"{"type":"begin","data":{"path":{"text":"./sub/a.txt"}}}"#;
        assert_eq!(parse_rg_line(begin), Some(Event::Begin { path: "sub/a.txt".into() }));
        let m = br#"{"type":"match","data":{"path":{"text":"./sub/a.txt"},"lines":{"text":"h\u00e9llo world foo\n"},"line_number":7,"absolute_offset":0,"submatches":[{"match":{"text":"foo"},"start":13,"end":16}]}}"#;
        let Some(Event::Match(hit)) = parse_rg_line(m) else { panic!() };
        assert_eq!((hit.path.as_str(), hit.line, hit.text.as_str()), ("sub/a.txt", 7, "héllo world foo"));
        // byte offsets past a two-byte é
        assert_eq!(hit.ranges, [13..16]);
        assert_eq!(&hit.text[hit.ranges[0].clone()], "foo");
        let summary = br#"{"data":{"elapsed_total":{"human":"0.004830s","nanos":4829896,"secs":0},"stats":{"bytes_printed":246,"bytes_searched":17,"matched_lines":1,"matches":3,"searches":1,"searches_with_match":1}},"type":"summary"}"#;
        assert_eq!(parse_rg_line(summary), Some(Event::Summary { matches: 3 }));
        assert_eq!(parse_rg_line(br#"{"type":"end","data":{"path":{"text":"a"}}}"#), None);
        assert_eq!(parse_rg_line(b"not json"), None);
    }

    #[test]
    fn rg_bytes_paths_and_lines_decode() {
        // a path that is not UTF-8 ("a\xff.txt") and a line with an invalid byte before the match
        let path = base64::engine::general_purpose::STANDARD.encode(b"./a\xff.txt");
        let line = base64::engine::general_purpose::STANDARD.encode(b"\xffx foo\r\n");
        let m = format!(r#"{{"type":"match","data":{{"path":{{"bytes":"{path}"}},"lines":{{"bytes":"{line}"}},"line_number":1,"submatches":[{{"start":3,"end":6}}]}}}}"#);
        let Some(Event::Match(hit)) = parse_rg_line(m.as_bytes()) else { panic!() };
        assert_eq!(hit.path, "a\u{fffd}.txt");
        assert_eq!(hit.text, "\u{fffd}x foo");
        assert_eq!(&hit.text[hit.ranges[0].clone()], "foo");
    }

    #[test]
    fn exit_codes_and_problems() {
        assert_eq!(ended(Some(0), ""), Ended::Ok);
        assert_eq!(ended(Some(1), ""), Ended::Ok);
        let stderr = "rg: ./private: Permission denied (os error 13)\nrg: gone: IO error for operation on gone: No such file or directory (os error 2)\nrg: ./private: Permission denied (os error 13)\n";
        assert_eq!(ended(Some(2), stderr), Ended::Problems(vec!["private: no permission".into(), "gone: no longer there".into()]));
        // a bad glob is no place: the search failed
        let glob = "rg: error parsing glob '{a': unclosed alternate group; missing '}' (maybe escape '{' with '[{]'?)\n";
        assert_eq!(ended(Some(2), glob), Ended::Failed("error parsing glob '{a': unclosed alternate group; missing '}' (maybe escape '{' with '[{]'?)".into()));
        assert_eq!(ended(Some(2), ""), Ended::Failed("rg failed".into()));
        assert!(matches!(ended(Some(101), "boom"), Ended::Failed(m) if m == "boom"));
        assert_eq!(problems("rg: ./a b: Is a directory (os error 21)"), ["a b: is a directory"]);
    }

    #[test]
    fn git_grep_and_grep_records_parse() {
        let query = Query { text: "foo".into(), ..Default::default() };
        let hit = parse_git_grep(b"sub/b.txt\x001\x003\x00x Foo foo", &query).unwrap();
        assert_eq!((hit.path.as_str(), hit.line, hit.text.as_str()), ("sub/b.txt", 1, "x Foo foo"));
        assert_eq!(hit.ranges, [2..5, 6..9], "every match, case folded");
        // a match the text search cannot find again (git's own folding): its column
        let odd = parse_git_grep(b"a\x002\x005\x00abcdfoo", &Query { case: true, ..q("FOO") }).unwrap();
        assert_eq!(odd.ranges, [4..7]);
        // git's C locale: `caf` is a whole word before the bytes of `é`, and folds no `É`; this search says no
        assert_eq!(parse_git_grep("a\x001\x001\x00café".as_bytes(), &Query { word: true, case: true, ..q("caf") }), None);
        assert_eq!(parse_git_grep("a\x001\x001\x00xé".as_bytes(), &q("xÉ")).map(|h| h.ranges), Some(vec![0..3]), "folded here too");
        assert_eq!(parse_git_grep(b"a\x001\x001\x00abc", &q("zzz")), None);
        assert_eq!(parse_git_grep(b"a\x00x\x001\x00t", &query), None);
        let hit = parse_grep(b"./dir/a:b.txt\x0012:foo: bar\n", &query).unwrap();
        assert_eq!((hit.path.as_str(), hit.line, hit.text.as_str()), ("dir/a:b.txt", 12, "foo: bar"));
        assert_eq!(hit.ranges, [0..3]);
    }

    #[test]
    fn text_search_matches_as_rg_does() {
        assert_eq!(find_ranges("foo Foo fOO", "foo", false, false), [0..3, 4..7, 8..11]);
        assert_eq!(find_ranges("foo Foo", "foo", true, false), [0..3]);
        assert_eq!(find_ranges("foo food _foo foo", "foo", true, true), [0..3, 14..17]);
        assert_eq!(find_ranges("aaaa", "aa", true, false), [0..2, 2..4]);
        assert_eq!(find_ranges("ÉCOLE école", "école", false, false), [0..6, 7..13]);
        assert_eq!(find_ranges("x", "", false, false), Vec::<Range<usize>>::new());
        let hits = search_text("a.rs", "one\r\nfoo two\nthree foo foo\n", &q("foo"), 10);
        assert_eq!(hits.iter().map(|h| (h.line, h.text.as_str(), h.ranges.len())).collect::<Vec<_>>(), [(2, "foo two", 1), (3, "three foo foo", 2)]);
        assert_eq!(search_text("a", "foo\nfoo\nfoo", &q("foo"), 2).len(), 2, "within the budget");
    }

    #[test]
    fn long_lines_are_cut_around_their_first_match() {
        assert_eq!(excerpt("    let x = foo;", &[12..15]), ("let x = foo;".to_string(), vec![8..11]));
        let long = format!("{}needle{}", "a".repeat(1000), "b".repeat(1000));
        let (shown, ranges) = excerpt(&long, &[1000..1006]);
        assert_eq!(shown.chars().count(), MAX_LINE + 2, "{shown}");
        assert!(shown.starts_with('…') && shown.ends_with('…'));
        assert_eq!(&shown[ranges[0].clone()], "needle");
        // multibyte: cut on characters
        let wide = "é".repeat(400) + "x";
        let (shown, ranges) = excerpt(&wide, &[800..801]);
        assert_eq!(&shown[ranges[0].clone()], "x");
    }
}
