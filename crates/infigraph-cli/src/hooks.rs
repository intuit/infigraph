use anyhow::Result;
use serde_json::json;

pub(crate) const ENFORCE_HOOK_SCRIPT: &str = r#"#!/usr/bin/env bash
# Infigraph PreToolUse enforcement hook
# Warns when raw search/file tools are used in Infigraph-indexed projects.
# stdin: JSON {tool_name, tool_input, cwd}
# exit 0 = allow (with warning on stderr)

input=$(cat)
tool=$(echo "$input" | jq -r '.tool_name // empty')
cwd=$(echo "$input" | jq -r '.cwd // empty')

# Guard: only enforce in projects with a .infigraph directory
[ -d "$cwd/.infigraph" ] || exit 0

case "$tool" in
  Grep)
    echo "WARNING: Prefer mcp__infigraph__search (unified search) over Grep. Infigraph is indexed for this project." >&2
    ;;
  Glob)
    echo "WARNING: Prefer mcp__infigraph__list_files over Glob. Infigraph is indexed for this project." >&2
    ;;
  Bash)
    cmd=$(echo "$input" | jq -r '.tool_input.command // empty')
    if echo "$cmd" | grep -qE '(^|\s|/)(grep|egrep|fgrep|rg|ripgrep|ag|ack)(\s|$)'; then
      echo "WARNING: Prefer mcp__infigraph__search over grep/rg. Infigraph is indexed for this project." >&2
    fi
    if echo "$cmd" | grep -qE '(^|\s)find\s.*-name\s'; then
      echo "WARNING: Prefer mcp__infigraph__list_files over find. Infigraph is indexed for this project." >&2
    fi
    ;;
esac

exit 0
"#;

pub(crate) const SESSION_SAVE_HOOK_SCRIPT: &str = r#"#!/usr/bin/env bash
# Infigraph UserPromptSubmit hook — session save reminder
# Counts user exchanges per Claude session. Every 5th exchange, emits a
# reminder to call save_session. Resets when the PostToolUse reset hook fires.
# stdin: JSON {prompt, session_id, cwd, ...}

input=$(cat)
cwd=$(echo "$input" | jq -r '.cwd // empty')

# Only enforce in Infigraph-indexed projects
[ -d "$cwd/.infigraph" ] || exit 0

session_id=$(echo "$input" | jq -r '.session_id // empty')
[ -z "$session_id" ] && exit 0

counter_dir="${TMPDIR:-/tmp}/infigraph-sessions"
mkdir -p "$counter_dir" 2>/dev/null
counter_file="$counter_dir/$session_id.count"

count=0
[ -f "$counter_file" ] && count=$(cat "$counter_file" 2>/dev/null || echo 0)
count=$((count + 1))
echo "$count" > "$counter_file"

if [ $((count % 5)) -eq 0 ]; then
  cat <<'ENDJSON'
{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"INFIGRAPH SESSION SAVE: You have NOT called save_session in the last 5 exchanges. Call mcp__infigraph__save_session NOW with a summary of work done so far, pending tasks, and decisions made. Do NOT defer this."}}
ENDJSON
fi

exit 0
"#;

pub(crate) const SESSION_RESET_HOOK_SCRIPT: &str = r#"#!/usr/bin/env bash
# Infigraph PostToolUse hook — resets session save counter after save_session
# stdin: JSON {tool_name, tool_input, ...}

input=$(cat)
tool=$(echo "$input" | jq -r '.tool_name // empty')

[ "$tool" = "mcp__infigraph__save_session" ] || exit 0

session_id=$(echo "$input" | jq -r '.session_id // empty')
[ -z "$session_id" ] && exit 0

counter_file="${TMPDIR:-/tmp}/infigraph-sessions/$session_id.count"
echo "0" > "$counter_file" 2>/dev/null

exit 0
"#;

pub(crate) const SESSION_START_HOOK_SCRIPT: &str = r#"#!/usr/bin/env bash
# Infigraph SessionStart hook — session continuity on startup/resume/compaction
# stdin: JSON {session_id, cwd, source, ...}
# source: "startup" | "resume" | "clear" | "compact"

input=$(cat)
cwd=$(echo "$input" | jq -r '.cwd // empty')

# Only enforce in Infigraph-indexed projects
[ -d "$cwd/.infigraph" ] || exit 0

source_type=$(echo "$input" | jq -r '.source // "startup"')

case "$source_type" in
  compact)
    cat <<'ENDJSON'
{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"INFIGRAPH SESSION SAVE (COMPACTION): Context was just compacted. Pre-compaction decisions and context are at risk of being lost. Call mcp__infigraph__save_session NOW with summary, pending_tasks, decisions, constraints, assumptions, and blockers from this session. Then call mcp__infigraph__get_latest_session to reload saved context. Do NOT skip this."}}
ENDJSON
    ;;
  startup|resume)
    cat <<'ENDJSON'
{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"INFIGRAPH SESSION RESTORE: Call mcp__infigraph__get_latest_session to recover prior session context (decisions, constraints, blockers, pending tasks). Do NOT start work without checking prior session state."}}
ENDJSON
    ;;
esac

exit 0
"#;

pub(crate) fn install_enforcement_hook(home: &std::path::Path) -> Result<()> {
    let hooks_dir = home.join(".claude").join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;

    let hook_path = hooks_dir.join("infigraph-enforce.sh");
    std::fs::write(&hook_path, ENFORCE_HOOK_SCRIPT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755))?;
    }
    println!("  Installed enforcement hook: {}", hook_path.display());

    let settings_path = home.join(".claude").join("settings.json");
    let mut settings: serde_json::Value = if settings_path.is_file() {
        let content = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content)?
    } else {
        json!({})
    };

    if settings.get("hooks").is_none() {
        settings["hooks"] = json!({});
    }

    let hook_entry = json!({
        "matcher": "Grep|Glob|Bash",
        "hooks": [{
            "type": "command",
            "command": hook_path.to_string_lossy(),
            "timeout": 5
        }]
    });

    let pre_tool = settings["hooks"]
        .get("PreToolUse")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let already_exists = pre_tool.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| {
                hooks.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .map(|c| c.contains("infigraph-enforce"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    });

    if !already_exists {
        let mut arr = pre_tool;
        arr.push(hook_entry);
        settings["hooks"]["PreToolUse"] = serde_json::Value::Array(arr);

        let pretty = serde_json::to_string_pretty(&settings)?;
        std::fs::write(&settings_path, pretty)?;
        println!("  Added PreToolUse hook to {}", settings_path.display());
    } else {
        println!(
            "  PreToolUse hook already configured in {}",
            settings_path.display()
        );
    }

    Ok(())
}

pub(crate) fn install_session_save_hook(home: &std::path::Path) -> Result<()> {
    let hooks_dir = home.join(".claude").join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;

    let save_hook_path = hooks_dir.join("infigraph-session-save.sh");
    std::fs::write(&save_hook_path, SESSION_SAVE_HOOK_SCRIPT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&save_hook_path, std::fs::Permissions::from_mode(0o755))?;
    }

    let reset_hook_path = hooks_dir.join("infigraph-session-reset.sh");
    std::fs::write(&reset_hook_path, SESSION_RESET_HOOK_SCRIPT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&reset_hook_path, std::fs::Permissions::from_mode(0o755))?;
    }

    let start_hook_path = hooks_dir.join("infigraph-session-start.sh");
    std::fs::write(&start_hook_path, SESSION_START_HOOK_SCRIPT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&start_hook_path, std::fs::Permissions::from_mode(0o755))?;
    }

    println!(
        "  Installed session save hook: {}",
        save_hook_path.display()
    );
    println!(
        "  Installed session reset hook: {}",
        reset_hook_path.display()
    );
    println!(
        "  Installed session start hook: {}",
        start_hook_path.display()
    );

    let settings_path = home.join(".claude").join("settings.json");
    let mut settings: serde_json::Value = if settings_path.is_file() {
        let content = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content)?
    } else {
        json!({})
    };

    if settings.get("hooks").is_none() {
        settings["hooks"] = json!({});
    }

    // UserPromptSubmit hook for session save reminder
    let save_hook_entry = json!({
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": save_hook_path.to_string_lossy(),
            "timeout": 5
        }]
    });

    let user_prompt = settings["hooks"]
        .get("UserPromptSubmit")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let save_exists = user_prompt.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| {
                hooks.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .map(|c| c.contains("infigraph-session-save"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    });

    let mut settings_changed = false;

    if !save_exists {
        let mut arr = user_prompt;
        arr.push(save_hook_entry);
        settings["hooks"]["UserPromptSubmit"] = serde_json::Value::Array(arr);
        settings_changed = true;
        println!(
            "  Added UserPromptSubmit hook to {}",
            settings_path.display()
        );
    } else {
        println!("  UserPromptSubmit session hook already configured");
    }

    // PostToolUse hook for counter reset
    let reset_hook_entry = json!({
        "matcher": "mcp__infigraph__save_session",
        "hooks": [{
            "type": "command",
            "command": reset_hook_path.to_string_lossy(),
            "timeout": 5,
            "async": true
        }]
    });

    let post_tool = settings["hooks"]
        .get("PostToolUse")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let reset_exists = post_tool.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| {
                hooks.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .map(|c| c.contains("infigraph-session-reset"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    });

    if !reset_exists {
        let mut arr = post_tool;
        arr.push(reset_hook_entry);
        settings["hooks"]["PostToolUse"] = serde_json::Value::Array(arr);
        settings_changed = true;
        println!(
            "  Added PostToolUse reset hook to {}",
            settings_path.display()
        );
    } else {
        println!("  PostToolUse session reset hook already configured");
    }

    // SessionStart hook for compaction save + startup/resume restore
    let start_hook_entry = json!({
        "hooks": [{
            "type": "command",
            "command": start_hook_path.to_string_lossy(),
            "timeout": 5
        }]
    });

    let session_start = settings["hooks"]
        .get("SessionStart")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let start_exists = session_start.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| {
                hooks.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .map(|c| c.contains("infigraph-session-start"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    });

    if !start_exists {
        let mut arr = session_start;
        arr.push(start_hook_entry);
        settings["hooks"]["SessionStart"] = serde_json::Value::Array(arr);
        settings_changed = true;
        println!("  Added SessionStart hook to {}", settings_path.display());
    } else {
        println!("  SessionStart session hook already configured");
    }

    if settings_changed {
        let pretty = serde_json::to_string_pretty(&settings)?;
        std::fs::write(&settings_path, pretty)?;
    }

    Ok(())
}

pub(crate) fn uninstall_hooks(home: &std::path::Path) -> Result<()> {
    let hooks_dir = home.join(".claude").join("hooks");
    for hook_file in &[
        "infigraph-enforce.sh",
        "infigraph-session-save.sh",
        "infigraph-session-reset.sh",
        "infigraph-session-start.sh",
    ] {
        let hook_path = hooks_dir.join(hook_file);
        if hook_path.exists() {
            std::fs::remove_file(&hook_path)?;
            println!("  Removed hook: {}", hook_path.display());
        }
    }

    let settings_path = home.join(".claude").join("settings.json");
    if settings_path.is_file() {
        let content = std::fs::read_to_string(&settings_path)?;
        if let Ok(mut settings) = serde_json::from_str::<serde_json::Value>(&content) {
            let infigraph_hook = |entry: &serde_json::Value| -> bool {
                entry
                    .get("hooks")
                    .and_then(|h| h.as_array())
                    .map(|hooks| {
                        hooks.iter().any(|h| {
                            h.get("command")
                                .and_then(|c| c.as_str())
                                .map(|c| c.contains("infigraph-"))
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false)
            };
            for event in &[
                "PreToolUse",
                "UserPromptSubmit",
                "PostToolUse",
                "SessionStart",
            ] {
                if let Some(arr) = settings["hooks"]
                    .get_mut(*event)
                    .and_then(|v| v.as_array_mut())
                {
                    let before = arr.len();
                    arr.retain(|entry| !infigraph_hook(entry));
                    if arr.len() < before {
                        println!(
                            "  Removed {} hook(s) from {}",
                            event,
                            settings_path.display()
                        );
                    }
                }
            }
            let pretty = serde_json::to_string_pretty(&settings)?;
            std::fs::write(&settings_path, pretty)?;
        }
    }

    Ok(())
}
