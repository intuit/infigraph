//! Ollama LLM provider for code review.
//!
//! Local-development add-on: everything Ollama-specific lives in this file so
//! upstream `review/llm.rs` only carries a single dispatch hook in
//! `review_with_llm`. Activated when `INFIGRAPH_LLM_PROVIDER=ollama`, or when
//! `OLLAMA_API_KEY` is set and no `ANTHROPIC_API_KEY` is present. Talks to the
//! Ollama chat API (`/api/chat`) — works with Ollama Cloud
//! (`https://ollama.com`) or any self-hosted server.
//!
//! Overridable via `INFIGRAPH_LLM_MODEL`, `INFIGRAPH_LLM_BASE_URL`,
//! `INFIGRAPH_LLM_MAX_TOKENS`.
//!
//! The response-parsing helpers at the bottom intentionally duplicate small
//! private helpers from `llm.rs` — keeping upstream files untouched is worth
//! a few duplicated lines here.

use anyhow::{Context, Result};

use super::llm::{LlmFinding, LlmReviewResult, RiskItem, TestCase, TokenUsage};

/// Per-request timeout. Reviews of large diffs on slow models legitimately
/// take minutes, so this only guards against a fully stalled server.
const REQUEST_TIMEOUT_SECS: u64 = 600;

/// Whether the Ollama provider is enabled.
///
/// Explicit selection via `INFIGRAPH_LLM_PROVIDER=ollama` always wins (any
/// other value disables Ollama). Without a selector, Ollama is used only when
/// `OLLAMA_API_KEY` is set and `ANTHROPIC_API_KEY` is not — so a shared env
/// file containing both keys still routes to the Anthropic-compatible backend
/// (Claude / Kimi / Grok).
pub fn enabled() -> bool {
    let has_key = |name: &str| std::env::var(name).map(|k| !k.is_empty()).unwrap_or(false);
    match std::env::var("INFIGRAPH_LLM_PROVIDER") {
        Ok(p) if !p.is_empty() => p.eq_ignore_ascii_case("ollama"),
        _ => has_key("OLLAMA_API_KEY") && !has_key("ANTHROPIC_API_KEY"),
    }
}

/// Run a review prompt through Ollama and parse the structured result.
pub fn review(prompt: &str) -> Result<LlmReviewResult> {
    // Optional: self-hosted Ollama needs no key; the Authorization header is
    // only sent when a key is configured (e.g. for ollama.com cloud).
    let api_key = std::env::var("OLLAMA_API_KEY").unwrap_or_default();
    let model =
        std::env::var("INFIGRAPH_LLM_MODEL").unwrap_or_else(|_| "glm-5.2:cloud".to_string());
    let base_url = std::env::var("INFIGRAPH_LLM_BASE_URL")
        .unwrap_or_else(|_| "https://ollama.com".to_string());
    let max_tokens: u32 = std::env::var("INFIGRAPH_LLM_MAX_TOKENS")
        .unwrap_or_else(|_| "16384".to_string())
        .parse()
        .unwrap_or(16384);

    let mut messages: Vec<serde_json::Value> =
        vec![serde_json::json!({"role": "user", "content": prompt})];
    let mut full_text = String::new();
    let mut total_input: u64 = 0;
    let mut total_output: u64 = 0;
    let max_continuations = 5;

    // Bound each request so a stalled server can't hang the review forever.
    // Reviews of large diffs on slow models legitimately take minutes.
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build();

    for attempt in 0..=max_continuations {
        let body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": false,
            "options": { "num_predict": max_tokens },
        });

        let mut req = agent
            .post(&format!("{base_url}/api/chat"))
            .set("content-type", "application/json");
        if !api_key.is_empty() {
            req = req.set("Authorization", &format!("Bearer {api_key}"));
        }
        let resp = req
            .send_string(&body.to_string())
            .context("Ollama API request failed")?;

        let resp_body: serde_json::Value = resp.into_json().context("parse Ollama response")?;

        // Reject structurally incomplete responses (e.g. `{ "done": true }`)
        // instead of silently treating them as an empty, finished reply.
        let chunk = resp_body
            .pointer("/message/content")
            .and_then(serde_json::Value::as_str)
            .context("Ollama response missing string message.content")?;

        full_text.push_str(chunk);
        total_input += resp_body["prompt_eval_count"].as_u64().unwrap_or(0);
        total_output += resp_body["eval_count"].as_u64().unwrap_or(0);

        let done_reason = resp_body["done_reason"]
            .as_str()
            .context("Ollama response missing string done_reason")?;

        if done_reason != "length" || attempt == max_continuations {
            break;
        }

        // Truncated — ask the model to continue
        messages.push(serde_json::json!({"role": "assistant", "content": chunk}));
        messages.push(serde_json::json!({"role": "user", "content": "Continue from where you left off. Complete the JSON."}));
    }

    let usage = TokenUsage {
        input_tokens: total_input,
        output_tokens: total_output,
    };

    parse_review_response(&full_text, usage)
}

/// Parse the model's raw text into a structured review result.
fn parse_review_response(full_text: &str, usage: TokenUsage) -> Result<LlmReviewResult> {
    let json_str = extract_json(full_text);
    let parsed: serde_json::Value = serde_json::from_str(json_str).unwrap_or_else(|_| {
        serde_json::json!({
            "summary": full_text,
            "findings": [],
            "test_plan": [],
            "risk_assessment": [],
            "deployment_notes": null
        })
    });

    let summary = parsed["summary"].as_str().unwrap_or("").to_string();
    let findings: Vec<LlmFinding> = parse_json_array(&parsed["findings"]);
    let test_plan: Vec<TestCase> = parse_json_array(&parsed["test_plan"]);
    let risk_assessment: Vec<RiskItem> = parse_json_array(&parsed["risk_assessment"]);
    let deployment_notes = parsed["deployment_notes"].as_str().map(|s| s.to_string());

    Ok(LlmReviewResult {
        summary,
        findings,
        test_plan,
        risk_assessment,
        deployment_notes,
        token_usage: Some(usage),
    })
}

fn parse_json_array<T: serde::de::DeserializeOwned>(val: &serde_json::Value) -> Vec<T> {
    val.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| serde_json::from_value(v.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn extract_json(text: &str) -> &str {
    // Find the first position where a complete, valid JSON value can be
    // parsed. A first-`{`-to-last-`}` slice would break when the model emits
    // prose containing brace-like examples before the actual JSON object.
    let trimmed = text.trim();
    let mut search_from = 0;
    while let Some(rel) = trimmed[search_from..].find('{') {
        let start = search_from + rel;
        let mut stream =
            serde_json::Deserializer::from_str(&trimmed[start..]).into_iter::<serde_json::Value>();
        if let Some(Ok(_)) = stream.next() {
            let end = start + stream.byte_offset();
            return &trimmed[start..end];
        }
        search_from = start + 1;
    }
    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_review_response_extracts_json() {
        let text = r#"Here is my review:
{"summary": "Looks good", "findings": [], "risk_assessment": []}
Done."#;
        let usage = TokenUsage {
            input_tokens: 10,
            output_tokens: 20,
        };
        let result = parse_review_response(text, usage).unwrap();
        assert_eq!(result.summary, "Looks good");
        assert!(result.findings.is_empty());
        let usage = result.token_usage.unwrap();
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 20);
    }

    #[test]
    fn extract_json_skips_brace_like_prose_before_json() {
        let text = r#"The format is {summary, findings} as discussed.
{"summary": "ok", "findings": []}"#;
        let extracted = extract_json(text);
        let parsed: serde_json::Value = serde_json::from_str(extracted).unwrap();
        assert_eq!(parsed["summary"], "ok");
    }

    #[test]
    fn parse_review_response_falls_back_to_raw_text() {
        let text = "not json at all";
        let usage = TokenUsage {
            input_tokens: 1,
            output_tokens: 2,
        };
        let result = parse_review_response(text, usage).unwrap();
        assert_eq!(result.summary, "not json at all");
        assert!(result.findings.is_empty());
    }
}
