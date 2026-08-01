use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::Deserialize;
use serde_json::Value;

const DEFAULT_STALENESS_WINDOW: usize = 6;
const DEFAULT_TOKEN_BUDGET: usize = 150_000;

static SESSION: Mutex<Option<SessionContext>> = Mutex::new(None);

#[cfg(test)]
thread_local! {
    /// Test-only: panic at start of apply_seen_dedup (after compress in the pipeline).
    static FORCE_DEDUP_PANIC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub fn force_dedup_panic(enabled: bool) {
    FORCE_DEDUP_PANIC.with(|c| c.set(enabled));
}

/// Serializes tests across this crate that touch the shared `SESSION` static
/// (or other process-global state like env vars / cwd) — `cargo test` runs
/// tests in parallel threads within one binary by default, and this state
/// isn't per-thread, so any test using it must hold this lock for its
/// duration, not just the ones in this file.
#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    compression: CompressionConfig,
}

#[derive(Debug, Deserialize)]
struct CompressionConfig {
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    dedup: Option<bool>,
    #[serde(default)]
    token_budget: Option<usize>,
    #[serde(default)]
    staleness_window: Option<usize>,
    #[serde(default)]
    ml_compression: Option<String>,
}

fn default_true() -> bool {
    true
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            level: None,
            dedup: None,
            token_budget: None,
            staleness_window: None,
            ml_compression: None,
        }
    }
}

fn find_config_file() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let mut dir = cwd.as_path();
    loop {
        let candidate = dir.join(".infigraph").join("config.toml");
        if candidate.exists() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
}

fn find_config_file_with_home_fallback() -> Option<PathBuf> {
    if let Some(p) = find_config_file() {
        return Some(p);
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    let candidate = PathBuf::from(home).join(".infigraph").join("config.toml");
    candidate.exists().then_some(candidate)
}

const DEDUP_STATE_FILE: &str = "dedup_state.json";
const PERSIST_INTERVAL: usize = 5;

fn dedup_state_path() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let mut dir = cwd.as_path();
    loop {
        let infigraph = dir.join(".infigraph");
        if infigraph.is_dir() {
            return Some(infigraph.join(DEDUP_STATE_FILE));
        }
        dir = dir.parent()?;
    }
}

fn load_dedup_state() -> HashMap<String, u64> {
    let Some(path) = dedup_state_path() else {
        return HashMap::new();
    };
    let Ok(data) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

fn persist_dedup_state(seen: &HashMap<String, SeenEntry>) {
    let Some(path) = dedup_state_path() else {
        return;
    };
    let hashes: HashMap<&str, u64> = seen
        .iter()
        .map(|(k, v)| (k.as_str(), v.content_hash))
        .collect();
    if let Ok(data) = serde_json::to_string(&hashes) {
        let _ = std::fs::write(path, data);
    }
}

fn load_config() -> CompressionConfig {
    find_config_file_with_home_fallback()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| toml::from_str::<ConfigFile>(&s).ok())
        .map(|c| c.compression)
        .unwrap_or_default()
}

struct SeenEntry {
    call_seen: usize,
    content_hash: u64,
    tokens_sent: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompressionLevel {
    Off,
    Summary,
    Aggressive,
    Minimal,
}

struct ToolCallStats {
    total: usize,
    detail_requests: usize,
}

struct FocusEntry {
    call_seen: usize,
}

struct SessionContext {
    seen: HashMap<String, SeenEntry>,
    prior_hashes: HashMap<String, u64>,
    call_counter: usize,
    staleness_window: usize,
    total_tokens_sent: usize,
    token_budget: usize,
    config: CompressionConfig,
    tool_stats: HashMap<String, ToolCallStats>,
    persist_counter: usize,
    /// Symbols/files recently fetched for editing — never compress these.
    focus: HashMap<String, FocusEntry>,
    /// Advances on every tool call — ages focus entries independently of dedup.
    focus_clock: usize,
    /// Compress/dedup panics recovered via catch_unwind (task 2.9).
    compress_failures: usize,
}

impl SessionContext {
    fn new() -> Self {
        let cfg = load_config();
        let budget = std::env::var("INFIGRAPH_TOKEN_BUDGET")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(cfg.token_budget)
            .unwrap_or(DEFAULT_TOKEN_BUDGET);
        let staleness = cfg.staleness_window.unwrap_or(DEFAULT_STALENESS_WINDOW);
        let prior = load_dedup_state();
        Self {
            seen: HashMap::new(),
            prior_hashes: prior,
            call_counter: 0,
            staleness_window: staleness,
            total_tokens_sent: 0,
            token_budget: budget,
            config: cfg,
            tool_stats: HashMap::new(),
            persist_counter: 0,
            focus: HashMap::new(),
            focus_clock: 0,
            compress_failures: 0,
        }
    }

    fn auto_level(&self) -> CompressionLevel {
        if self.token_budget == 0 {
            return CompressionLevel::Summary;
        }
        let remaining_pct = ((self.token_budget.saturating_sub(self.total_tokens_sent)) as f64
            / self.token_budget as f64
            * 100.0) as usize;
        match remaining_pct {
            71..=100 => CompressionLevel::Off,
            50..=70 => CompressionLevel::Summary,
            20..=49 => CompressionLevel::Aggressive,
            _ => CompressionLevel::Minimal,
        }
    }
}

fn content_key(tool_name: &str, args: &Value) -> String {
    // Key by tool + primary identifier arg
    let id = args
        .get("symbol_id")
        .or_else(|| args.get("symbol"))
        .or_else(|| args.get("query"))
        .or_else(|| args.get("name"))
        .or_else(|| args.get("file"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    format!("{tool_name}:{id}")
}

fn hash_content(s: &str) -> u64 {
    // FNV-1a 64-bit — no dependency needed
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn estimate_tokens(s: &str) -> usize {
    ((s.split_whitespace().count() as f64) * 1.4).ceil() as usize
}

/// Get current compression level based on token budget usage.
/// Priority: env var > config.toml level > auto (budget-based).
/// If compression is disabled (config `enabled = false`), returns Off.
pub fn get_compression_level() -> CompressionLevel {
    if let Some(level) = parse_level_override() {
        return level;
    }
    let mut guard = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    let ctx = guard.get_or_insert_with(SessionContext::new);
    if !ctx.config.enabled {
        return CompressionLevel::Off;
    }
    if let Some(ref level_str) = ctx.config.level {
        if let Some(level) = parse_level_str(level_str) {
            if level != CompressionLevel::Off {
                return level;
            }
        }
    }
    ctx.auto_level()
}

fn parse_level_str(s: &str) -> Option<CompressionLevel> {
    match s.to_lowercase().as_str() {
        "off" => Some(CompressionLevel::Off),
        "summary" => Some(CompressionLevel::Summary),
        "aggressive" => Some(CompressionLevel::Aggressive),
        "minimal" => Some(CompressionLevel::Minimal),
        "auto" => None,
        _ => None,
    }
}

/// Get ML compression mode: "off", "extractive" (default), or "kompress".
/// Priority: env var INFIGRAPH_ML_COMPRESSION > config.toml > "extractive".
pub fn get_ml_compression_mode() -> String {
    if let Ok(v) = std::env::var("INFIGRAPH_ML_COMPRESSION") {
        return v.to_lowercase();
    }
    let guard = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .as_ref()
        .and_then(|ctx| ctx.config.ml_compression.clone())
        .unwrap_or_else(|| "extractive".to_string())
        .to_lowercase()
}

fn parse_level_override() -> Option<CompressionLevel> {
    std::env::var("INFIGRAPH_COMPRESSION_LEVEL")
        .ok()
        .and_then(|v| parse_level_str(&v))
}

/// Record tokens sent and return updated compression level.
pub fn track_tokens(tokens: usize) -> CompressionLevel {
    let mut guard = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    let ctx = guard.get_or_insert_with(SessionContext::new);
    ctx.total_tokens_sent += tokens;
    ctx.auto_level()
}

/// Apply seen-dedup to already-compressed tool output.
/// Returns the output unchanged if dedup is disabled or content is fresh.
pub fn apply_seen_dedup(compressed: &str, tool_name: &str, args: &Value) -> String {
    #[cfg(test)]
    if FORCE_DEDUP_PANIC.with(|c| c.get()) {
        panic!("forced dedup panic");
    }

    // Focused edit targets must keep full text (task 3.3) — skip dedup placeholders.
    if is_in_focus(args) {
        return compressed.to_string();
    }

    let env_dedup = std::env::var("INFIGRAPH_DEDUP").ok().map(|v| v != "0");
    let config_dedup = {
        let guard = SESSION.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .as_ref()
            .and_then(|ctx| ctx.config.dedup)
            .unwrap_or(true)
    };
    if !env_dedup.unwrap_or(config_dedup) {
        return compressed.to_string();
    }

    // Don't dedup error responses or tiny outputs
    if compressed.starts_with("Error:") || compressed.starts_with("No ") {
        return compressed.to_string();
    }
    let tokens = estimate_tokens(compressed);
    if tokens < 50 {
        return compressed.to_string();
    }

    let key = content_key(tool_name, args);
    if key.ends_with(':') {
        return compressed.to_string();
    }

    let hash = hash_content(compressed);

    let mut guard = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    let ctx = guard.get_or_insert_with(SessionContext::new);
    ctx.call_counter += 1;
    let current_call = ctx.call_counter;

    let effective_window = match ctx.auto_level() {
        CompressionLevel::Off => ctx.staleness_window,
        CompressionLevel::Summary => ctx.staleness_window,
        CompressionLevel::Aggressive => ctx.staleness_window.max(8),
        CompressionLevel::Minimal => ctx.staleness_window.max(12),
    };

    if let Some(entry) = ctx.seen.get(&key) {
        let age = current_call - entry.call_seen;
        if entry.content_hash == hash && age <= effective_window {
            // Same content, still fresh — return compact placeholder
            let placeholder = format!(
                "(seen {} call{} ago: {key}, {} tokens — use detail=true to force full output)",
                age,
                if age == 1 { "" } else { "s" },
                entry.tokens_sent
            );
            // Update the seen entry to refresh the call counter
            ctx.seen.insert(
                key,
                SeenEntry {
                    call_seen: current_call,
                    content_hash: hash,
                    tokens_sent: entry.tokens_sent,
                },
            );
            maybe_persist(ctx);
            return placeholder;
        }
        // Content changed or stale — fall through to show full + update
    }

    // Check prior session hashes (content-verified dedup)
    if let Some(&prior_hash) = ctx.prior_hashes.get(&key) {
        if prior_hash == hash {
            // Content unchanged since prior session — dedup
            ctx.seen.insert(
                key.clone(),
                SeenEntry {
                    call_seen: current_call,
                    content_hash: hash,
                    tokens_sent: tokens,
                },
            );
            ctx.prior_hashes.remove(&key);
            let placeholder = format!(
                "(seen in prior session: {key}, {} tokens — use detail=true to force full output)",
                tokens
            );
            maybe_persist(ctx);
            return placeholder;
        }
        // Content changed — remove stale prior hash
        ctx.prior_hashes.remove(&key);
    }

    ctx.seen.insert(
        key,
        SeenEntry {
            call_seen: current_call,
            content_hash: hash,
            tokens_sent: tokens,
        },
    );

    maybe_persist(ctx);
    compressed.to_string()
}

fn maybe_persist(ctx: &mut SessionContext) {
    ctx.persist_counter += 1;
    if ctx.persist_counter.is_multiple_of(PERSIST_INTERVAL) {
        persist_dedup_state(&ctx.seen);
    }
}

/// Record a tool call, noting whether detail=true was requested.
pub fn record_tool_call(tool_name: &str, detail_requested: bool) {
    let mut guard = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    let ctx = guard.get_or_insert_with(SessionContext::new);
    let entry = ctx
        .tool_stats
        .entry(tool_name.to_string())
        .or_insert(ToolCallStats {
            total: 0,
            detail_requests: 0,
        });
    entry.total += 1;
    if detail_requested {
        entry.detail_requests += 1;
    }
    ctx.focus_clock += 1;
}

/// Mark symbols/files as in-focus when the agent fetches them for editing (task 3.3).
/// Sources: `get_code_snippet`, or `get_doc_context` with `for_edit=true`.
pub fn record_focus(tool_name: &str, args: &Value) {
    let track = match tool_name {
        "get_code_snippet" => true,
        "get_doc_context" => args
            .get("for_edit")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        _ => false,
    };
    if !track {
        return;
    }
    let mut keys = Vec::new();
    if let Some(id) = args.get("symbol_id").and_then(|v| v.as_str()) {
        keys.push(id.to_string());
        if let Some((file, _)) = id.split_once("::") {
            keys.push(file.to_string());
        }
    }
    if let Some(sym) = args.get("symbol").and_then(|v| v.as_str()) {
        keys.push(sym.to_string());
    }
    if let Some(file) = args.get("file").and_then(|v| v.as_str()) {
        keys.push(file.to_string());
    }
    if keys.is_empty() {
        return;
    }
    let mut guard = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    let ctx = guard.get_or_insert_with(SessionContext::new);
    let call = ctx.focus_clock;
    let window = ctx.staleness_window;
    ctx.focus
        .retain(|_, e| call.saturating_sub(e.call_seen) <= window);
    for key in keys {
        ctx.focus.insert(key, FocusEntry { call_seen: call });
    }
}

/// True if args refer to a recently focused symbol/file (within staleness window).
pub fn is_in_focus(args: &Value) -> bool {
    // Only structured identity args — not free-text `query` (would false-positive on search).
    let candidates: Vec<&str> = [
        args.get("symbol_id").and_then(|v| v.as_str()),
        args.get("symbol").and_then(|v| v.as_str()),
        args.get("file").and_then(|v| v.as_str()),
    ]
    .into_iter()
    .flatten()
    .collect();
    if candidates.is_empty() {
        return false;
    }
    let guard = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    let Some(ctx) = guard.as_ref() else {
        return false;
    };
    for cand in candidates {
        if focus_hit(ctx, cand) {
            return true;
        }
        if let Some((file, _)) = cand.split_once("::") {
            if focus_hit(ctx, file) {
                return true;
            }
        }
    }
    false
}

fn focus_hit(ctx: &SessionContext, key: &str) -> bool {
    let Some(entry) = ctx.focus.get(key) else {
        return false;
    };
    ctx.focus_clock.saturating_sub(entry.call_seen) <= ctx.staleness_window
}

/// Record a recovered compress/dedup panic (task 2.9).
pub fn record_compress_failure() {
    let mut guard = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    let ctx = guard.get_or_insert_with(SessionContext::new);
    ctx.compress_failures += 1;
}

/// Check if a tool's detail-request rate exceeds 30%, suggesting compression is too aggressive.
pub fn should_reduce_compression(tool_name: &str) -> bool {
    let guard = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    let Some(ctx) = guard.as_ref() else {
        return false;
    };
    let Some(stats) = ctx.tool_stats.get(tool_name) else {
        return false;
    };
    if stats.total < 5 {
        return false;
    }
    (stats.detail_requests as f64 / stats.total as f64) > 0.3
}

/// Return compression stats for the current session.
pub fn get_compression_stats() -> String {
    let guard = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    let Some(ctx) = guard.as_ref() else {
        return "No compression data yet (no tool calls in this session).".to_string();
    };
    let level = if let Some(l) = parse_level_override() {
        format!("{l:?} (env override)")
    } else if !ctx.config.enabled {
        "Off (disabled in config)".to_string()
    } else if let Some(ref ls) = ctx.config.level {
        if let Some(l) = parse_level_str(ls) {
            format!("{l:?} (config)")
        } else {
            format!("{:?} (auto)", ctx.auto_level())
        }
    } else {
        format!("{:?} (auto)", ctx.auto_level())
    };
    let remaining_pct = if ctx.token_budget > 0 {
        ((ctx.token_budget.saturating_sub(ctx.total_tokens_sent)) as f64 / ctx.token_budget as f64
            * 100.0) as usize
    } else {
        0
    };
    let dedup_entries = ctx.seen.len();
    let mut out = format!(
        "Compression Stats (current session):\n  Level: {level}\n  Token budget: {}\n  Tokens sent: {}\n  Budget remaining: {remaining_pct}%\n  Tool calls tracked: {}\n  Dedup entries: {dedup_entries}\n  Focus entries: {}\n  Compress failures: {}",
        ctx.token_budget,
        ctx.total_tokens_sent,
        ctx.call_counter,
        ctx.focus.len(),
        ctx.compress_failures,
    );
    if !ctx.tool_stats.is_empty() {
        out.push_str("\n  Detail-request rates:");
        let mut tools: Vec<_> = ctx.tool_stats.iter().collect();
        tools.sort_by_key(|(name, _)| (*name).clone());
        for (name, stats) in &tools {
            let rate = if stats.total > 0 {
                (stats.detail_requests as f64 / stats.total as f64 * 100.0).round() as usize
            } else {
                0
            };
            let flag = if stats.total >= 5 && rate > 30 {
                " ⚠ auto-reduced"
            } else {
                ""
            };
            out.push_str(&format!(
                "\n    {name}: {}/{} ({rate}%){flag}",
                stats.detail_requests, stats.total
            ));
        }
    }
    out
}

/// Reset session state (for testing).
#[cfg(test)]
pub fn reset_session() {
    let mut guard = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    *guard = None;
}

/// Cross-module serialisation lock for tests that mutate the global SESSION.
/// Both session_context::tests and compress::tests share this to prevent races.
#[cfg(test)]
pub static SESSION_TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::MutexGuard;

    /// Isolates tests from the real repo / home config and dedup_state.json.
    struct TestEnv {
        _lock: MutexGuard<'static, ()>,
        _tmpdir: tempfile::TempDir,
        orig_cwd: PathBuf,
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            reset_session();
            force_dedup_panic(false);
            let _ = std::env::set_current_dir(&self.orig_cwd);
        }
    }

    fn setup() -> TestEnv {
        let lock = SESSION_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmpdir = tempfile::tempdir().unwrap();
        let ig = tmpdir.path().join(".infigraph");
        std::fs::create_dir_all(&ig).unwrap();
        // Shadow walk-up + ~/.infigraph/config.toml (e.g. level=summary).
        // No `level` → auto_level; no dedup_state.json → empty prior_hashes.
        std::fs::write(ig.join("config.toml"), "[compression]\n").unwrap();

        let orig_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmpdir.path()).unwrap();
        std::env::set_var("INFIGRAPH_DEDUP", "1");
        std::env::remove_var("INFIGRAPH_TOKEN_BUDGET");
        std::env::remove_var("INFIGRAPH_COMPRESSION_LEVEL");
        force_dedup_panic(false);
        reset_session();

        TestEnv {
            _lock: lock,
            _tmpdir: tmpdir,
            orig_cwd,
        }
    }

    fn big_output() -> String {
        "word ".repeat(100) // ~140 tokens
    }

    #[test]
    fn test_dedup_same_content_returns_placeholder() {
        let _g = setup();
        let output = big_output();
        let args = json!({"symbol_id": "src/lib.rs::foo"});

        let first = apply_seen_dedup(&output, "get_doc_context", &args);
        assert_eq!(first, output);

        let second = apply_seen_dedup(&output, "get_doc_context", &args);
        assert!(second.starts_with("(seen "));
        assert!(second.contains("get_doc_context:src/lib.rs::foo"));
    }

    #[test]
    fn test_dedup_changed_content_returns_full() {
        let _g = setup();
        let args = json!({"symbol_id": "src/lib.rs::foo"});

        let first = apply_seen_dedup(&big_output(), "get_doc_context", &args);
        assert!(!first.starts_with("(seen"));

        let changed = format!("{} extra", big_output());
        let second = apply_seen_dedup(&changed, "get_doc_context", &args);
        assert!(!second.starts_with("(seen"));
        assert_eq!(second, changed);
    }

    #[test]
    fn test_dedup_stale_returns_full() {
        let _g = setup();
        let output = big_output();
        let args = json!({"symbol_id": "src/lib.rs::foo"});

        apply_seen_dedup(&output, "get_doc_context", &args);

        // Burn through staleness window with other calls
        for i in 0..7 {
            let other_args = json!({"symbol_id": format!("other_{i}")});
            apply_seen_dedup(&big_output(), "search", &other_args);
        }

        let result = apply_seen_dedup(&output, "get_doc_context", &args);
        // Should be stale (>6 calls gap) so returns full
        assert!(!result.starts_with("(seen"));
    }

    #[test]
    fn test_dedup_skips_small_output() {
        let _g = setup();
        let small = "short output";
        let args = json!({"symbol_id": "src/lib.rs::foo"});

        apply_seen_dedup(small, "get_doc_context", &args);
        let second = apply_seen_dedup(small, "get_doc_context", &args);
        assert_eq!(second, small); // Not deduped — too small
    }

    #[test]
    fn test_dedup_skips_errors() {
        let _g = setup();
        let err = &format!("Error: not found {}", big_output());
        let args = json!({"symbol_id": "src/lib.rs::foo"});

        apply_seen_dedup(err, "get_doc_context", &args);
        let second = apply_seen_dedup(err, "get_doc_context", &args);
        assert!(second.starts_with("Error:"));
    }

    #[test]
    fn test_dedup_enabled_by_default() {
        let _g = SESSION_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_session();
        std::env::remove_var("INFIGRAPH_DEDUP");
        let output = big_output();
        let args = json!({"symbol_id": "src/lib.rs::foo"});

        apply_seen_dedup(&output, "get_doc_context", &args);
        let second = apply_seen_dedup(&output, "get_doc_context", &args);
        assert!(second.starts_with("(seen")); // Dedup on by default
    }

    #[test]
    fn test_dedup_disabled_with_env_zero() {
        let _g = SESSION_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_session();
        std::env::set_var("INFIGRAPH_DEDUP", "0");
        let output = big_output();
        let args = json!({"symbol_id": "src/lib.rs::foo"});

        apply_seen_dedup(&output, "get_doc_context", &args);
        let second = apply_seen_dedup(&output, "get_doc_context", &args);
        assert_eq!(second, output); // Dedup off via env
    }

    #[test]
    fn test_dedup_different_tools_different_keys() {
        let _g = setup();
        let output = big_output();
        let args = json!({"symbol_id": "src/lib.rs::foo"});

        apply_seen_dedup(&output, "get_doc_context", &args);
        let second = apply_seen_dedup(&output, "search", &args);
        assert!(!second.starts_with("(seen")); // Different tool = different key
    }

    #[test]
    fn test_dedup_refreshes_on_hit() {
        let _g = setup();
        let output = big_output();
        let args = json!({"symbol_id": "src/lib.rs::foo"});

        // Call 1: first see
        apply_seen_dedup(&output, "get_doc_context", &args);

        // Calls 2-4: other stuff
        for i in 0..3 {
            let other_args = json!({"symbol_id": format!("other_{i}")});
            apply_seen_dedup(&big_output(), "search", &other_args);
        }

        // Call 5: re-see foo — dedup hit, refreshes counter
        let result = apply_seen_dedup(&output, "get_doc_context", &args);
        assert!(result.starts_with("(seen"));

        // Calls 6-9: more other stuff (4 more)
        for i in 3..7 {
            let other_args = json!({"symbol_id": format!("other_{i}")});
            apply_seen_dedup(&big_output(), "search", &other_args);
        }

        // Call 10: foo again — should still be fresh (refreshed at call 5)
        let result2 = apply_seen_dedup(&output, "get_doc_context", &args);
        assert!(result2.starts_with("(seen"));
    }

    // --- Phase 6: Budget-aware level tests ---

    #[test]
    fn test_auto_level_high_budget() {
        let _g = setup();
        // Fresh session = 0 tokens sent, 150k budget = 100% remaining = Off
        assert_eq!(get_compression_level(), CompressionLevel::Off);
    }

    #[test]
    fn test_auto_level_transitions() {
        let _g = setup();
        std::env::set_var("INFIGRAPH_TOKEN_BUDGET", "100000");
        reset_session();

        // 0% used → Off
        assert_eq!(get_compression_level(), CompressionLevel::Off);

        // Use 35k → 65% remaining → Summary
        let level = track_tokens(35000);
        assert_eq!(level, CompressionLevel::Summary);

        // Use another 25k → 40% remaining → Aggressive
        let level = track_tokens(25000);
        assert_eq!(level, CompressionLevel::Aggressive);

        // Use another 25k → 15% remaining → Minimal
        let level = track_tokens(25000);
        assert_eq!(level, CompressionLevel::Minimal);
    }

    #[test]
    fn test_auto_level_custom_budget() {
        let _g = setup();
        std::env::set_var("INFIGRAPH_TOKEN_BUDGET", "1000");
        reset_session();

        // 900 tokens → 10% remaining → Minimal
        let level = track_tokens(900);
        assert_eq!(level, CompressionLevel::Minimal);
    }

    #[test]
    fn test_track_tokens_cumulative() {
        let _g = setup();
        std::env::set_var("INFIGRAPH_TOKEN_BUDGET", "10000");
        reset_session();

        track_tokens(1000);
        track_tokens(1000);
        track_tokens(1000);
        // 3000/10000 = 70% remaining → Summary (boundary)
        let level = get_compression_level();
        assert_eq!(level, CompressionLevel::Summary);
    }

    // --- Phase 3.7: Prior-session dedup tests ---

    fn inject_prior_hashes(hashes: HashMap<String, u64>) {
        let mut guard = SESSION.lock().unwrap_or_else(|e| e.into_inner());
        let ctx = guard.get_or_insert_with(SessionContext::new);
        ctx.prior_hashes = hashes;
    }

    #[test]
    fn test_prior_session_dedup_matching_hash() {
        let _g = setup();
        let output = big_output();
        let args = json!({"symbol_id": "src/lib.rs::foo"});
        let key = "get_doc_context:src/lib.rs::foo";
        let hash = hash_content(&output);

        inject_prior_hashes(HashMap::from([(key.to_string(), hash)]));

        let result = apply_seen_dedup(&output, "get_doc_context", &args);
        assert!(
            result.starts_with("(seen in prior session:"),
            "expected prior-session placeholder, got: {result}"
        );
        assert!(result.contains(key));
    }

    #[test]
    fn test_prior_session_dedup_stale_hash() {
        let _g = setup();
        let output = big_output();
        let changed = format!("{} changed content", big_output());
        let args = json!({"symbol_id": "src/lib.rs::foo"});
        let key = "get_doc_context:src/lib.rs::foo";
        let old_hash = hash_content(&output);

        inject_prior_hashes(HashMap::from([(key.to_string(), old_hash)]));

        // Content changed — should NOT dedup, should return full
        let result = apply_seen_dedup(&changed, "get_doc_context", &args);
        assert!(
            !result.starts_with("(seen"),
            "stale hash should not dedup, got: {result}"
        );
        assert_eq!(result, changed);

        // Prior hash should be removed — verify by checking second call isn't prior-session dedup
        let result2 = apply_seen_dedup(&changed, "get_doc_context", &args);
        assert!(
            result2.starts_with("(seen "),
            "second call should be regular dedup"
        );
        assert!(!result2.contains("prior session"));
    }

    #[test]
    fn test_prior_session_dedup_migrates_to_seen() {
        let _g = setup();
        let output = big_output();
        let args = json!({"symbol_id": "src/lib.rs::foo"});
        let key = "get_doc_context:src/lib.rs::foo";
        let hash = hash_content(&output);

        inject_prior_hashes(HashMap::from([(key.to_string(), hash)]));

        // First call: hits prior_hashes, migrates to seen
        let r1 = apply_seen_dedup(&output, "get_doc_context", &args);
        assert!(r1.contains("prior session"));

        // Second call: should hit regular seen map, not prior
        let r2 = apply_seen_dedup(&output, "get_doc_context", &args);
        assert!(r2.starts_with("(seen "));
        assert!(!r2.contains("prior session"));
    }

    #[test]
    fn test_persist_writes_at_interval() {
        let _g = setup();
        let dir = tempfile::tempdir().unwrap();
        let infigraph_dir = dir.path().join(".infigraph");
        std::fs::create_dir_all(&infigraph_dir).unwrap();
        let state_file = infigraph_dir.join(DEDUP_STATE_FILE);

        // Change cwd so dedup_state_path() finds our temp dir
        let orig_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        reset_session();

        let _args = json!({"symbol_id": "src/lib.rs::foo"});

        // Make PERSIST_INTERVAL calls (5) — file should exist after
        for i in 0..PERSIST_INTERVAL {
            let out = format!("output number {} {}", i, big_output());
            let a = json!({"symbol_id": format!("sym_{i}")});
            apply_seen_dedup(&out, "get_doc_context", &a);
        }

        assert!(
            state_file.exists(),
            "dedup state should be persisted after {PERSIST_INTERVAL} calls"
        );
        let data: HashMap<String, u64> =
            serde_json::from_str(&std::fs::read_to_string(&state_file).unwrap()).unwrap();
        assert_eq!(data.len(), PERSIST_INTERVAL);

        // Restore cwd
        std::env::set_current_dir(orig_dir).unwrap();
    }

    // --- Task 3.3: Focus tracking ---

    #[test]
    fn test_focus_from_code_snippet_bypasses_matching_symbol() {
        let _g = setup();
        record_tool_call("get_code_snippet", false);
        record_focus(
            "get_code_snippet",
            &json!({"symbol_id": "src/auth.rs::login"}),
        );
        assert!(is_in_focus(&json!({"symbol_id": "src/auth.rs::login"})));
        assert!(is_in_focus(&json!({"symbol_id": "src/auth.rs::logout"}))); // same file
        assert!(!is_in_focus(&json!({"symbol_id": "src/other.rs::foo"})));
    }

    #[test]
    fn test_focus_from_symbol_only_arg() {
        let _g = setup();
        record_tool_call("get_code_snippet", false);
        record_focus("get_code_snippet", &json!({"symbol": "authenticate"}));
        assert!(is_in_focus(&json!({"symbol": "authenticate"})));
        assert!(!is_in_focus(&json!({"symbol": "other"})));
    }

    #[test]
    fn test_focus_from_file_only_arg() {
        let _g = setup();
        record_tool_call("get_code_snippet", false);
        record_focus("get_code_snippet", &json!({"file": "src/auth.rs"}));
        assert!(is_in_focus(&json!({"file": "src/auth.rs"})));
        assert!(!is_in_focus(&json!({"file": "src/other.rs"})));
    }

    #[test]
    fn test_focus_from_bare_symbol_id_without_path() {
        let _g = setup();
        record_tool_call("get_code_snippet", false);
        record_focus("get_code_snippet", &json!({"symbol_id": "login"}));
        assert!(is_in_focus(&json!({"symbol_id": "login"})));
        // No "::" → no derived file-path key. Flat key space still matches
        // file:"login" (same string); unrelated paths stay cold.
        assert!(is_in_focus(&json!({"file": "login"})));
        assert!(!is_in_focus(&json!({"file": "src/auth.rs"})));
        assert!(!is_in_focus(&json!({"symbol_id": "src/auth.rs::login"})));
    }

    #[test]
    fn test_focus_ignores_unrelated_tools() {
        let _g = setup();
        record_tool_call("search", false);
        record_focus("search", &json!({"symbol_id": "src/auth.rs::login"}));
        assert!(!is_in_focus(&json!({"symbol_id": "src/auth.rs::login"})));
    }

    #[test]
    fn test_focus_requires_for_edit_on_doc_context() {
        let _g = setup();
        record_tool_call("get_doc_context", false);
        record_focus(
            "get_doc_context",
            &json!({"symbol_id": "src/auth.rs::login", "for_edit": false}),
        );
        assert!(!is_in_focus(&json!({"symbol_id": "src/auth.rs::login"})));

        record_tool_call("get_doc_context", false);
        record_focus(
            "get_doc_context",
            &json!({"symbol_id": "src/auth.rs::login", "for_edit": true}),
        );
        assert!(is_in_focus(&json!({"symbol_id": "src/auth.rs::login"})));
    }

    #[test]
    fn test_focus_ages_out_after_staleness_window() {
        let _g = setup();
        record_tool_call("get_code_snippet", false);
        record_focus(
            "get_code_snippet",
            &json!({"symbol_id": "src/auth.rs::login"}),
        );
        assert!(is_in_focus(&json!({"symbol_id": "src/auth.rs::login"})));

        // Advance focus_clock past default staleness window (6)
        for _ in 0..7 {
            record_tool_call("search", false);
        }
        assert!(!is_in_focus(&json!({"symbol_id": "src/auth.rs::login"})));
    }

    #[test]
    fn test_focus_ignores_free_text_query() {
        let _g = setup();
        record_tool_call("get_code_snippet", false);
        record_focus(
            "get_code_snippet",
            &json!({"symbol_id": "src/auth.rs::login"}),
        );
        // Exact path as search query must NOT trip focus (would disable search compression)
        assert!(!is_in_focus(&json!({"query": "src/auth.rs"})));
        assert!(!is_in_focus(&json!({"query": "src/auth.rs::login"})));
        assert!(is_in_focus(&json!({"symbol_id": "src/auth.rs::login"})));
    }

    #[test]
    fn test_focus_prune_removes_expired_entries() {
        let _g = setup();
        record_tool_call("get_code_snippet", false);
        record_focus("get_code_snippet", &json!({"symbol_id": "src/old.rs::a"}));
        for _ in 0..7 {
            record_tool_call("search", false);
        }
        // Recording new focus should prune expired keys from the map
        record_tool_call("get_code_snippet", false);
        record_focus("get_code_snippet", &json!({"symbol_id": "src/new.rs::b"}));
        let stats = get_compression_stats();
        // Only the fresh focus remains (symbol + file = 2 keys)
        assert!(
            stats.contains("Focus entries: 2"),
            "expected pruned focus map with 2 entries, got: {stats}"
        );
        assert!(!is_in_focus(&json!({"symbol_id": "src/old.rs::a"})));
        assert!(is_in_focus(&json!({"symbol_id": "src/new.rs::b"})));
    }

    #[test]
    fn test_config_level_used_for_metrics_not_auto_level() {
        let _g = setup();
        // High budget remaining → auto_level is Off, but config Summary must win
        let dir = tempfile::tempdir().unwrap();
        let ig = dir.path().join(".infigraph");
        std::fs::create_dir_all(&ig).unwrap();
        std::fs::write(
            ig.join("config.toml"),
            "[compression]\nlevel = \"summary\"\n",
        )
        .unwrap();
        let orig = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        reset_session();
        std::env::remove_var("INFIGRAPH_COMPRESSION_LEVEL");
        std::env::remove_var("INFIGRAPH_TOKEN_BUDGET");

        let level = get_compression_level();
        let auto = track_tokens(0); // still ~100% budget → Off
        assert_eq!(level, CompressionLevel::Summary);
        assert_eq!(auto, CompressionLevel::Off);
        // Metrics must log `level` (Summary), not `auto` (Off)

        std::env::set_current_dir(orig).unwrap();
    }

    // --- Task 2.9: Compress failure health ---

    #[test]
    fn test_record_compress_failure_shows_in_stats() {
        let _g = setup();
        record_tool_call("search", false);
        record_compress_failure();
        record_compress_failure();
        let stats = get_compression_stats();
        assert!(
            stats.contains("Compress failures: 2"),
            "expected failure count in stats, got: {stats}"
        );
    }
}
