use std::{io::Read, time::Duration};

use anyhow::{Context, Result, bail};
use semver::Version;
use serde::Deserialize;

#[derive(Debug, PartialEq, Eq)]
pub struct AvailableRelease {
    pub version: String,
    pub url: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CheckResult {
    NoPublishedRelease,
    UpToDate,
    Available(AvailableRelease),
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
}

fn parse_version(value: &str) -> Result<Version, semver::Error> {
    Version::parse(value.strip_prefix(['v', 'V']).unwrap_or(value))
}

pub fn newer_version(current: &str, latest: &str) -> Option<String> {
    let current = parse_version(current).ok()?;
    let latest = parse_version(latest).ok()?;
    (latest.cmp_precedence(&current).is_gt() && latest.pre.is_empty()).then(|| latest.to_string())
}

fn evaluate_release(current: &str, release: Release) -> Result<CheckResult> {
    if release.draft || release.prerelease {
        return Ok(CheckResult::NoPublishedRelease);
    }
    let latest =
        parse_version(&release.tag_name).context("release tag is not a valid SemVer version")?;
    if !latest.pre.is_empty() {
        return Ok(CheckResult::NoPublishedRelease);
    }
    parse_version(current).context("application version is not a valid SemVer version")?;
    Ok(match newer_version(current, &release.tag_name) {
        Some(version) => CheckResult::Available(AvailableRelease {
            version,
            url: format!("{}/releases/tag/{}", crate::REPOSITORY, release.tag_name),
        }),
        None => CheckResult::UpToDate,
    })
}

pub fn check() -> Result<CheckResult> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(format!("DiscordQuestCompleter/{}", crate::VERSION))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()?;
    let response = client
        .get(format!(
            "https://api.github.com/repos/{}/releases/latest",
            crate::REPOSITORY.trim_start_matches("https://github.com/")
        ))
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(CheckResult::NoPublishedRelease);
    }
    if response.status().as_u16() == 429
        || (response.status().as_u16() == 403
            && response
                .headers()
                .get("x-ratelimit-remaining")
                .is_some_and(|remaining| remaining == "0"))
    {
        bail!("GitHub API rate limit reached; try again later");
    }
    let response = response.error_for_status()?;
    let mut body = String::new();
    response.take(1024 * 1024 + 1).read_to_string(&mut body)?;
    if body.len() > 1024 * 1024 {
        bail!("release response exceeds 1 MiB");
    }
    evaluate_release(crate::VERSION, serde_json::from_str(&body)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str) -> Release {
        Release {
            tag_name: tag.into(),
            prerelease: false,
            draft: false,
        }
    }

    #[test]
    fn compares_versions_numerically() {
        assert_eq!(newer_version("1.0.9", "v1.0.10"), Some("1.0.10".into()));
        assert_eq!(newer_version("1.0.10", "v1.0.9"), None);
        assert_eq!(newer_version("1.0.0", "1.0.0"), None);
        assert_eq!(newer_version("1.0.0", "2.0.0-beta.1"), None);
        assert_eq!(newer_version("1.0.0", "invalid"), None);
        assert_eq!(newer_version("1.0.0", "vv2.0.0"), None);
        assert_eq!(newer_version("V1.0.0", "V1.1.0"), Some("1.1.0".into()));
        assert_eq!(newer_version("1.0.0+abc", "1.0.0+xyz"), None);
        assert_eq!(newer_version("1.1.0-rc.1", "v1.1.0"), Some("1.1.0".into()));
    }

    #[test]
    fn links_to_the_release_that_was_checked() {
        assert_eq!(
            evaluate_release("1.0.0", release("v1.2.0")).unwrap(),
            CheckResult::Available(AvailableRelease {
                version: "1.2.0".into(),
                url: format!("{}/releases/tag/v1.2.0", crate::REPOSITORY),
            })
        );
    }

    #[test]
    fn distinguishes_current_versions_from_unpublished_releases() {
        for tag in ["1.0.0", "v0.9.9"] {
            assert_eq!(
                evaluate_release("1.0.0", release(tag)).unwrap(),
                CheckResult::UpToDate
            );
        }
        let mut draft = release("v2.0.0");
        draft.draft = true;
        let mut prerelease = release("v2.0.0");
        prerelease.prerelease = true;
        for release in [draft, prerelease, release("v2.0.0-beta.1")] {
            assert_eq!(
                evaluate_release("1.0.0", release).unwrap(),
                CheckResult::NoPublishedRelease
            );
        }
    }

    #[test]
    fn malformed_release_tags_are_reported_as_errors() {
        assert!(evaluate_release("1.0.0", release("not-a-version")).is_err());
        assert!(evaluate_release("invalid", release("v2.0.0")).is_err());
    }
}
