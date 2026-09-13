mod driver;
mod plugin;

pub use driver::GrammarDriver;
pub use plugin::{
    discover_plugins, plugin_fingerprint, GrammarPlugin, GrammarPluginConfig, ProjectConfig,
    ProjectPreprocessorConfig,
};

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use infigraph_core::lang::{LanguagePack, LanguageRegistry};

/// Find the infigraph-driver.jar bundled with the binary.
/// Searches: next to the binary, in ../driver/, in the workspace root.
fn find_driver_jar() -> Option<std::path::PathBuf> {
    // Next to binary
    if let Ok(exe) = std::env::current_exe() {
        let dir = exe.parent()?;
        let candidate = dir.join("infigraph-driver.jar");
        if candidate.exists() {
            return Some(candidate);
        }
        // ../driver/
        let candidate = dir.parent()?.join("driver").join("infigraph-driver.jar");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    // INFIGRAPH_DRIVER_JAR env var
    if let Ok(path) = std::env::var("INFIGRAPH_DRIVER_JAR") {
        let p = std::path::PathBuf::from(path);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Default grammar plugins directory: ~/.infigraph/grammars/
fn default_grammars_dir() -> std::path::PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".infigraph")
        .join("grammars")
}

/// Bundled grammars shipped next to the binary (e.g., npm tarball).
fn bundled_grammars_dir() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.join("grammars");
    if dir.exists() {
        Some(dir)
    } else {
        None
    }
}

/// Load grammar plugins and register them into an existing registry.
/// If no JVM or driver jar is available, silently skips plugin loading.
/// Also searches `project_grammars_dir` (e.g., `<project>/grammars/`) if provided.
/// Reads `.infigraph.toml` from `project_root` for preprocessor config.
pub fn register_grammar_plugins(
    registry: &mut LanguageRegistry,
    project_grammars_dir: Option<&Path>,
    project_root: Option<&Path>,
) -> Result<()> {
    let driver_jar = match find_driver_jar() {
        Some(jar) => jar,
        None => {
            if std::env::var("INFIGRAPH_DEBUG").is_ok() {
                eprintln!("[infigraph] Grammar plugin driver jar not found, skipping plugins");
            }
            return Ok(());
        }
    };

    // Discover plugins: bundled (next to binary) → user home → project-local
    let mut all_plugins = Vec::new();

    if let Some(bundled) = bundled_grammars_dir() {
        if let Ok(plugins) = discover_plugins(&bundled) {
            all_plugins.extend(plugins);
        }
    }

    let home_dir = default_grammars_dir();
    if let Ok(plugins) = discover_plugins(&home_dir) {
        all_plugins.extend(plugins);
    }

    if let Some(project_dir) = project_grammars_dir {
        if let Ok(plugins) = discover_plugins(project_dir) {
            all_plugins.extend(plugins);
        }
    }

    if all_plugins.is_empty() {
        return Ok(());
    }

    // Read project-level preprocessor config, resolving relative include paths
    let project_pp = project_root
        .map(|root| root.join(".infigraph.toml"))
        .filter(|p| p.exists())
        .and_then(|p| {
            let root = p.parent()?;
            let content = std::fs::read_to_string(&p).ok()?;
            let config: ProjectConfig = toml::from_str(&content).ok()?;
            let mut pp = config.preprocessor?;
            pp.include_paths = pp
                .include_paths
                .iter()
                .map(|ip| root.join(ip).to_string_lossy().to_string())
                .collect();
            Some(pp)
        });

    // Spawn single shared JVM driver for all plugins
    let driver = Arc::new(GrammarDriver::spawn(
        driver_jar.to_str().unwrap_or("infigraph-driver.jar"),
    )?);

    for (config, dir) in all_plugins {
        let name = config.language.name.clone();
        let extensions = config.language.extensions.clone();
        // Computed before `config`/`dir` move into the plugin.
        let fingerprint = plugin_fingerprint(&config, &dir);

        let plugin = GrammarPlugin::new(config, dir, Arc::clone(&driver), project_pp.clone());

        if let Err(e) = plugin.load() {
            eprintln!(
                "[infigraph] Failed to load grammar plugin '{}': {}",
                name, e
            );
            continue;
        }

        let pack = LanguagePack::new_custom(&name, extensions, Box::new(plugin))
            .with_fingerprint_part(&fingerprint);
        registry.register(pack);
    }

    Ok(())
}
