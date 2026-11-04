use std::path::PathBuf;

pub fn update_cache_path() -> Option<PathBuf> {
    dirs_next::home_dir().map(|h| h.join(".infigraph").join("update_check.json"))
}

pub fn fetch_latest_version() -> Option<String> {
    let gh_host = std::env::var("INFIGRAPH_GH_HOST").unwrap_or_else(|_| "github.com".to_string());
    let gh_owner = std::env::var("INFIGRAPH_GH_OWNER").unwrap_or_else(|_| "intuit".to_string());
    let gh_repo = "infigraph";

    let mut args = vec!["api"];
    let api_path = format!("repos/{gh_owner}/{gh_repo}/releases/latest");
    if gh_host != "github.com" {
        args.extend(["--hostname", &gh_host]);
    }
    args.push(&api_path);
    args.extend(["--jq", ".tag_name"]);

    let output = std::process::Command::new("gh").args(&args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let tag = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let version = tag.strip_prefix('v').unwrap_or(&tag).to_string();
    if version.is_empty() {
        None
    } else {
        Some(version)
    }
}

pub fn version_newer(latest: &str, current: &str) -> bool {
    let parse = |s: &str| -> Vec<u32> { s.split('.').filter_map(|p| p.parse().ok()).collect() };
    parse(latest) > parse(current)
}

pub fn check_for_update_background() -> Option<std::thread::JoinHandle<()>> {
    let cache_path = update_cache_path()?;

    if let Ok(content) = std::fs::read_to_string(&cache_path) {
        if let Ok(cached) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(ts) = cached.get("checked_at").and_then(|v| v.as_i64()) {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                if now - ts < 86400 {
                    return None;
                }
            }
        }
    }

    Some(std::thread::spawn(move || {
        let Some(latest) = fetch_latest_version() else {
            return;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let cache = serde_json::json!({
            "latest_version": latest,
            "current_version": env!("CARGO_PKG_VERSION"),
            "checked_at": now,
        });
        if let Some(parent) = cache_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(
            &cache_path,
            serde_json::to_string(&cache).unwrap_or_default(),
        );
    }))
}

/// The cached latest version, if it is newer than the running one.
pub fn cached_newer_version() -> Option<String> {
    let content = std::fs::read_to_string(update_cache_path()?).ok()?;
    let cached: serde_json::Value = serde_json::from_str(&content).ok()?;
    let latest = cached.get("latest_version")?.as_str()?;
    version_newer(latest, env!("CARGO_PKG_VERSION")).then(|| latest.to_string())
}

#[cfg(test)]
mod tests {
    use super::version_newer;

    #[test]
    fn newer_compares_numerically() {
        assert!(version_newer("3.2.17", "3.2.16"));
        assert!(version_newer("3.10.0", "3.9.9"));
        assert!(!version_newer("3.2.16", "3.2.16"));
        assert!(!version_newer("3.2.15", "3.2.16"));
    }
}
