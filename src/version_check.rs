use semver::Version;
use serde::Deserialize;
use std::env;
use std::time::Duration;

const GHCR_TOKEN_URL: &str =
    "https://ghcr.io/token?service=ghcr.io&scope=repository:ipandral/rustydb:pull";
const GHCR_TAGS_URL: &str = "https://ghcr.io/v2/ipandral/rustydb/tags/list";
const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    pub current_version: String,
    pub latest_version: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    token: String,
}

#[derive(Debug, Deserialize)]
struct TagsResponse {
    tags: Option<Vec<String>>,
}

pub fn print_outdated_version_warning() {
    if update_check_disabled() {
        return;
    }

    if let Ok(Some(update)) = check_for_update() {
        println!(
            "WARNING: RustyDB {} is outdated. Latest available package version is {}.",
            update.current_version, update.latest_version
        );
        println!(
            "         Update from: https://github.com/IPandral/RustyDB/pkgs/container/rustydb"
        );
        println!();
    }
}

fn update_check_disabled() -> bool {
    env::var("RUSTYDB_UPDATE_CHECK")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            )
        })
        .unwrap_or(false)
}

fn check_for_update() -> Result<Option<UpdateInfo>, Box<dyn std::error::Error>> {
    let current_version = env!("CARGO_PKG_VERSION");
    let latest_version = fetch_latest_package_version()?;

    if is_newer_version(&latest_version, current_version) {
        Ok(Some(UpdateInfo {
            current_version: current_version.to_string(),
            latest_version,
        }))
    } else {
        Ok(None)
    }
}

fn fetch_latest_package_version() -> Result<String, Box<dyn std::error::Error>> {
    let agent = ureq::AgentBuilder::new()
        .timeout(UPDATE_CHECK_TIMEOUT)
        .build();

    let token: TokenResponse = agent
        .get(GHCR_TOKEN_URL)
        .set("User-Agent", "RustyDB update checker")
        .call()?
        .into_json()?;

    let tags: TagsResponse = agent
        .get(GHCR_TAGS_URL)
        .set("User-Agent", "RustyDB update checker")
        .set("Authorization", &format!("Bearer {}", token.token))
        .call()?
        .into_json()?;

    latest_semver_tag(tags.tags.unwrap_or_default())
        .ok_or_else(|| "no semantic version tags found for RustyDB package".into())
}

fn latest_semver_tag(tags: Vec<String>) -> Option<String> {
    tags.into_iter()
        .filter_map(|tag| parse_tag_version(&tag).map(|version| (version, tag)))
        .max_by(|(left, _), (right, _)| left.cmp(right))
        .map(|(_, tag)| tag)
}

fn is_newer_version(candidate: &str, current: &str) -> bool {
    match (parse_tag_version(candidate), parse_tag_version(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

fn parse_tag_version(tag: &str) -> Option<Version> {
    let tag = tag.strip_prefix('v').unwrap_or(tag);
    Version::parse(tag).ok()
}

#[cfg(test)]
mod tests {
    use super::{is_newer_version, latest_semver_tag, parse_tag_version};

    #[test]
    fn parses_plain_and_v_prefixed_versions() {
        assert_eq!(
            parse_tag_version("0.3.0-beta").unwrap().to_string(),
            "0.3.0-beta"
        );
        assert_eq!(parse_tag_version("v0.3.1").unwrap().to_string(), "0.3.1");
    }

    #[test]
    fn ignores_non_version_tags_when_selecting_latest() {
        let latest = latest_semver_tag(vec![
            "latest".to_string(),
            "0.3.0-beta".to_string(),
            "0.3.1".to_string(),
        ]);

        assert_eq!(latest.as_deref(), Some("0.3.1"));
    }

    #[test]
    fn detects_when_candidate_is_newer() {
        assert!(is_newer_version("0.3.1", "0.3.0-beta"));
        assert!(!is_newer_version("0.3.0-beta", "0.3.0-beta"));
        assert!(!is_newer_version("latest", "0.3.0-beta"));
    }
}
