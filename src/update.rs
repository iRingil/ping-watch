//! Update check: asks GitHub for the latest release of the app and compares its version with this one. Runs on a
//! thread of its own: once a day if the user keeps it on (Settings → General), and when asked in About.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use crate::APP_NAME;

/// The app's repository on GitHub.
pub const REPOSITORY: &str = "iRingil/ping-watch";
/// Longest wait for GitHub's answer.
const TIMEOUT: Duration = Duration::from_secs(15);

/// What the check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// This version is the latest (or newer).
    UpToDate,
    /// A newer version is published.
    Newer {
        /// Its version, "1.3.0".
        version: String,
        /// The release page to download it from.
        url: String,
    },
    /// The repository has no releases yet.
    NoReleases,
}

/// Why the check failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UpdateError {
    /// GitHub couldn't be reached or answered with an error.
    #[error("GitHub couldn't be reached: {0}")]
    Network(String),
    /// GitHub's answer wasn't what a release looks like.
    #[error("GitHub's answer couldn't be read: {0}")]
    Answer(String),
}

/// A check running in the background.
#[derive(Debug)]
pub struct Check {
    /// Where the thread sends its result.
    receiver: Receiver<Result<Outcome, UpdateError>>,
}

impl Check {
    /// Starts checking the latest release of `repository` (owner/name) against `current` (this version).
    pub fn start(repository: &str, current: &str) -> Self {
        let (sender, receiver) = mpsc::channel();
        let (repository, current) = (repository.to_owned(), current.to_owned());
        let spawned = thread::Builder::new().name("update-check".to_owned()).spawn({
            let sender = sender.clone();
            move || {
                // Nobody may be waiting anymore (the window was closed); then the answer is dropped.
                sender.send(check(&repository, &current)).ok();
            }
        });
        if let Err(error) = spawned {
            sender.send(Err(UpdateError::Network(error.to_string()))).ok();
        }
        Self { receiver }
    }

    /// The result once it arrived, `None` while the check runs.
    pub fn result(&self) -> Option<Result<Outcome, UpdateError>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(UpdateError::Network("the check stopped".to_owned()))),
        }
    }
}

/// Asks GitHub for the latest release of `repository` and compares it with `current`. Blocks.
fn check(repository: &str, current: &str) -> Result<Outcome, UpdateError> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(TIMEOUT)).build().into();
    let url = format!("https://api.github.com/repos/{repository}/releases/latest");
    let answer = agent
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", &format!("{APP_NAME}/{current}"))
        .call();
    let mut response = match answer {
        Ok(response) => response,
        // GitHub answers "not found" for a repository without releases.
        Err(ureq::Error::StatusCode(404)) => return Ok(Outcome::NoReleases),
        Err(error) => return Err(UpdateError::Network(error.to_string())),
    };
    let body = response.body_mut().read_to_string().map_err(|error| UpdateError::Network(error.to_string()))?;
    outcome(&body, repository, current)
}

/// The outcome of GitHub's answer `body` (the latest release of `repository` as JSON) for this version `current`.
/// The download link is opened by the Windows shell, so only a release page of `repository` is taken from the
/// answer; anything else gives way to the repository's own releases page.
fn outcome(body: &str, repository: &str, current: &str) -> Result<Outcome, UpdateError> {
    let release: serde_json::Value =
        serde_json::from_str(body).map_err(|error| UpdateError::Answer(error.to_string()))?;
    let tag = release.get("tag_name").and_then(serde_json::Value::as_str).unwrap_or_default();
    let url = release.get("html_url").and_then(serde_json::Value::as_str).unwrap_or_default();
    let (Some(latest), Some(this)) = (version_of(tag), version_of(current)) else {
        return Err(UpdateError::Answer(format!("unknown version \"{tag}\"")));
    };
    if latest > this {
        let version = tag.trim_start_matches(['v', 'V']).to_owned();
        let releases = format!("https://github.com/{repository}/releases/");
        let url = if url.starts_with(&releases) && !url.chars().any(char::is_whitespace) {
            url.to_owned()
        } else {
            log::warn!("unexpected release link {url:?}, using the releases page");
            format!("{releases}latest")
        };
        Ok(Outcome::Newer { version, url })
    } else {
        Ok(Outcome::UpToDate)
    }
}

/// Major, minor and patch of "1.3.0" or "v1.3.0"; `None` if it isn't such a version.
fn version_of(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.trim().trim_start_matches(['v', 'V']).split('.').map(|part| part.parse::<u64>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_read_and_compared() {
        assert_eq!(version_of("v1.3.0"), Some((1, 3, 0)));
        assert_eq!(version_of("1.10.2"), Some((1, 10, 2)));
        assert_eq!(version_of("1.2"), None);
        assert_eq!(version_of("1.2.3.4"), None);
        assert_eq!(version_of("latest"), None);
        assert!(version_of("1.10.0") > version_of("1.9.9"), "numbers, not text");
    }

    #[test]
    fn the_answer_decides() {
        let release = r#"{"tag_name": "v1.3.0", "html_url": "https://github.com/o/r/releases/tag/v1.3.0"}"#;
        assert_eq!(
            outcome(release, "o/r", "1.0.0"),
            Ok(Outcome::Newer {
                version: "1.3.0".to_owned(),
                url: "https://github.com/o/r/releases/tag/v1.3.0".to_owned()
            })
        );
        assert_eq!(outcome(release, "o/r", "1.3.0"), Ok(Outcome::UpToDate));
        assert_eq!(outcome(release, "o/r", "2.0.0"), Ok(Outcome::UpToDate), "a newer build than the release");
        assert!(outcome("{}", "o/r", "1.0.0").is_err());
        assert!(outcome("not json", "o/r", "1.0.0").is_err());
    }

    #[test]
    fn only_a_release_page_of_the_repository_is_linked() {
        let newer_url = |link: &str| {
            let body = format!(r#"{{"tag_name": "v9.0.0", "html_url": "{link}"}}"#);
            match outcome(&body, "o/r", "1.0.0") {
                Ok(Outcome::Newer { url, .. }) => url,
                other => panic!("{other:?}"),
            }
        };
        let fallback = "https://github.com/o/r/releases/latest";
        assert_eq!(
            newer_url("https://github.com/o/r/releases/tag/v9.0.0"),
            "https://github.com/o/r/releases/tag/v9.0.0"
        );
        assert_eq!(newer_url("file:///C:/Windows/System32/calc.exe"), fallback);
        assert_eq!(newer_url("https://evil.example/o/r/releases/"), fallback);
        assert_eq!(newer_url("https://github.com/other/r/releases/tag/v9.0.0"), fallback);
    }

    /// Asks the real GitHub (run with `cargo test -- --ignored`): a repository with releases, and this one, whose
    /// latest release is never newer than the version being built.
    #[test]
    #[ignore = "needs the internet"]
    fn real_github() {
        let found = check("iRingil/gamepreset-peek", "1.0.0");
        assert!(matches!(found, Ok(Outcome::Newer { .. })), "{found:?}");
        let found = check(REPOSITORY, env!("CARGO_PKG_VERSION"));
        assert!(matches!(found, Ok(Outcome::NoReleases | Outcome::UpToDate)), "{found:?}");
    }
}
