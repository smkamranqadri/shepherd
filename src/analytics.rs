//! Upstream reports anonymous usage events (an `active` event a day and an `agent` event per agent) to Aptabase.
//! This fork never does: [`seen`] is kept so its callers stay as upstream's and a later merge of upstream's analytics
//! shows up as a conflict here. There is no sender, no setting and no environment variable that turns one on.

/// Takes the agents herdr names in the panes, as upstream's does, and does nothing with them.
pub fn seen<'a>(_agents: impl IntoIterator<Item = &'a str>) {}

#[cfg(test)]
mod tests {
    use super::seen;
    use std::path::Path;

    fn sources(dir: &Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push((path.display().to_string(), std::fs::read_to_string(&path).unwrap()));
            }
        }
    }

    /// Nothing in this fork can send an event: `seen` has no body, this module has no way to reach the network, and
    /// no source file names Aptabase's endpoint or key (the test files that say so are built from parts).
    #[test]
    fn nothing_is_sent_whatever_the_settings_or_environment() {
        // whatever DO_NOT_TRACK says, `seen` returns at once, having started nothing
        seen(["claude", "codex"]);
        let this = include_str!("analytics.rs");
        let code = this.split("#[cfg(test)]").next().unwrap();
        for word in ["ureq", "TcpStream", "http", "spawn", "Command", "static ", "fn enabled"] {
            assert!(!code.contains(word), "analytics.rs uses {word}");
        }
        assert!(code.contains("pub fn seen<'a>(_agents: impl IntoIterator<Item = &'a str>) {}"), "`seen` does something");
        let (host, key) = (["eu.apt", "abase.com"].concat(), ["A-EU-", "5120995280"].concat());
        let mut all = Vec::new();
        sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut all);
        for (path, text) in all {
            assert!(!text.contains(&host) && !text.contains(&key), "{path} names the usage endpoint or key");
            if !path.ends_with("analytics.rs") {
                assert!(!text.contains("App-Key"), "{path} sends an App-Key header");
            }
        }
    }
}
