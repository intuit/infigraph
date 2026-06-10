use serde_json::Value;

use super::handlers_analysis::{api_architecture, api_cluster, api_dead_code, api_stats};
use super::handlers_symbol::api_search;

pub(crate) fn api_chat(params: &Value) -> Value {
    let message = params.get("message").and_then(|m| m.as_str()).unwrap_or("");
    let msg_lower = message.to_lowercase();

    // Simple intent detection -- translate natural language to graph operations
    if msg_lower.contains("dead code") || msg_lower.contains("unused") {
        return api_dead_code(params);
    }
    if msg_lower.contains("architecture")
        || msg_lower.contains("overview")
        || msg_lower.contains("summary")
    {
        return api_architecture(params);
    }
    if msg_lower.contains("cluster")
        || msg_lower.contains("module")
        || msg_lower.contains("community")
    {
        return api_cluster(params);
    }
    if msg_lower.contains("who calls") || msg_lower.contains("callers of") {
        // Extract symbol name after "calls" or "of"
        let name = msg_lower
            .split("calls")
            .last()
            .or_else(|| msg_lower.split("of").last())
            .unwrap_or("")
            .trim();
        if !name.is_empty() {
            let mut p = params.clone();
            p["query"] = Value::String(name.to_string());
            return api_search(&p);
        }
    }
    if msg_lower.contains("stats") || msg_lower.contains("how many") || msg_lower.contains("count")
    {
        return api_stats(params);
    }

    // Default: treat as search query
    let mut p = params.clone();
    p["query"] = Value::String(message.to_string());
    api_search(&p)
}
