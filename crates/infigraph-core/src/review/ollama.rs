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
    let api_key = std::env::var("OLLAMA_API_KEY").context("OLLAMA_API_KEY not set")?;
    let model = std::env::var("INFIGRAPH_LLM_MODEL").unwrap_or_else(|_| "glm-5.2".to_string());
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

    for attempt in 0..=max_continuations {
        let body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": false,
            "options": { "num_predict": max_tokens },
        });

        let mut req =
            ureq::post(&format!("{base_url}/api/chat")).set("content-type", "application/json");
        if !api_key.is_empty() {
            req = req.set("Authorization", &format!("Bearer {api_key}"));
        }
        let resp = req
            .send_string(&body.to_string())
            .context("Ollama API request failed")?;

        let resp_body: serde_json::Value = resp.into_json().context("parse Ollama response")?;

        let chunk = resp_body["message"]["content"].as_str().unwrap_or("");

        full_text.push_str(chunk);
        total_input += resp_body["prompt_eval_count"].as_u64().unwrap_or(0);
        total_output += resp_body["eval_count"].as_u64().unwrap_or(0);

        let done_reason = resp_body["done_reason"].as_str().unwrap_or("stop");

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
    // Strip markdown code fences if present
    let trimmed = text.trim();
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            return &trimmed[start..=end];
        }
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
