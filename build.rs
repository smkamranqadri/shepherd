//! Hands the short git commit to the crate as `GIT_HASH` (the dev badge's tooltip), unless the environment already sets it.
fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-env-changed=GIT_HASH");
    if std::env::var_os("GIT_HASH").is_some() {
        return;
    }
    let hash = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(hash) = hash {
        println!("cargo:rustc-env=GIT_HASH={hash}");
    }
}
