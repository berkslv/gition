//! Checking GitHub for a newer release of gition.

use std::time::Duration;

const REPO: &str = "berkslv/gition";

/// The command that updates gition on this platform.
pub const COMMAND: &str = if cfg!(windows) {
    "powershell -c \"irm https://raw.githubusercontent.com/berkslv/gition/main/scripts/update.ps1 | iex\""
} else {
    "curl -fsSL https://raw.githubusercontent.com/berkslv/gition/main/scripts/update.sh | sh"
};

/// The latest release's version, like `v0.2.0`, if it's newer than this build.
/// Off when `GITION_NO_UPDATE_CHECK` is set.
pub fn check() -> Option<String> {
    if std::env::var_os("GITION_NO_UPDATE_CHECK").is_some() {
        return None;
    }
    latest().filter(|v| is_newer(env!("CARGO_PKG_VERSION"), v))
}

/// Reads where `/releases/latest` redirects to, `/releases/tag/<version>`, which isn't rate
/// limited like the API.
pub fn latest() -> Option<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .max_redirects(0)
        .user_agent(concat!("gition/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let res = agent.get(format!("https://github.com/{REPO}/releases/latest")).call().ok()?;
    let location = res.headers().get("location")?.to_str().ok()?;
    let version = location.rsplit('/').next()?;
    version.starts_with('v').then(|| version.to_string())
}

/// True if `latest` (`v1.2.3` or `1.2.3`) is a higher version than `current`.
pub fn is_newer(current: &str, latest: &str) -> bool {
    let parse = |v: &str| -> Option<Vec<u64>> {
        // A pre-release or build suffix like `-rc.1` doesn't count.
        let v = v.trim_start_matches('v').split(['-', '+']).next()?;
        v.split('.').map(|n| n.parse().ok()).collect()
    };
    match (parse(current), parse(latest)) {
        (Some(c), Some(l)) => l > c,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.1.0", "v0.2.0"));
        assert!(is_newer("0.1.9", "v0.1.10"));
        assert!(is_newer("0.9.0", "v1.0.0"));
        assert!(!is_newer("0.1.0", "v0.1.0"));
        assert!(!is_newer("0.2.0", "v0.1.0"));
        assert!(!is_newer("0.1.0", "v0.1.0-rc.1"));
        assert!(!is_newer("0.1.0", "latest"));
    }
}
