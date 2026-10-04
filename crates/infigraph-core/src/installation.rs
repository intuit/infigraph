//! Binary discovery shared by installation and read-only diagnostics.
use std::path::PathBuf;

use anyhow::Result;

pub fn find_mcp_binary() -> Result<PathBuf> {
    let name = if cfg!(windows) {
        "infigraph-mcp.exe"
    } else {
        "infigraph-mcp"
    };
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.join(name)));
    let paths = std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .map(|p| p.join(name))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    find_binary(sibling.into_iter().chain(paths)).ok_or_else(|| anyhow::anyhow!(
        "Could not find infigraph-mcp binary. Build it with `cargo build -p infigraph-mcp` or ensure it is on your PATH."
    ))
}

fn find_binary(paths: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    paths.into_iter().find(|p| {
        let Ok(meta) = p.metadata() else { return false };
        if !meta.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            true
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_binary_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_binary([dir.path().join("missing")]).is_none());
        assert!(find_binary([dir.path().to_path_buf()]).is_none());
    }
}
