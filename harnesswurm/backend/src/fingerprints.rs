//! Attribution of proxied calls to an agent when the client sends no
//! `X-Agent-ID` header — which is the common case, because most coding
//! agents cannot be configured to send custom headers at all.
//!
//! An agent can't hide, though: each one sends a distinctive `User-Agent`
//! header, and a system prompt whose opening lines name it. Matching either
//! is enough to attribute the call without any cooperation from the agent.
//! Both are best-effort signals, not proof, so an explicit `X-Agent-ID`
//! header always wins over a fingerprint match.

use crate::providers::WireApi;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;

/// One agent's recognisable traits, from `fingerprints.yaml`.
#[derive(Debug, Clone, Deserialize)]
pub struct FingerprintConfig {
    pub agent: String,
    #[serde(default)]
    pub user_agents: Vec<String>,
    #[serde(default)]
    pub system_prompts: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct FingerprintTable {
    /// File order is priority: the first entry that matches wins, so a more
    /// specific agent can be listed ahead of one it might be confused with.
    entries: Vec<FingerprintConfig>,
}

impl FingerprintTable {
    /// Loads `fingerprints.yaml`, degrading to an empty table (no
    /// fingerprinting) rather than refusing to start: attribution headers
    /// and the run prefix still work with the file missing or broken.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(content) => match Self::parse(&content) {
                Ok(table) => table,
                Err(e) => {
                    eprintln!(
                        "Warning: failed to parse {} ({e}); calls without X-Agent-ID are not attributed to an agent",
                        path.display()
                    );
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    pub fn parse(yaml: &str) -> Result<Self, String> {
        #[derive(Deserialize)]
        struct FingerprintsFile {
            fingerprints: Vec<FingerprintConfig>,
        }
        let file: FingerprintsFile =
            serde_yaml::from_str(yaml).map_err(|e| format!("{e}"))?;
        for entry in &file.fingerprints {
            if entry.agent.trim().is_empty() {
                return Err("every fingerprint entry needs an agent name".to_string());
            }
        }
        Ok(Self { entries: file.fingerprints })
    }

    /// The agent a `User-Agent` header belongs to, by substring: agents
    /// identify themselves as `name/version`, so a fragment of the name is
    /// enough and survives version changes.
    pub fn match_user_agent(&self, user_agent: &str) -> Option<&str> {
        let ua = user_agent.to_ascii_lowercase();
        self.entries
            .iter()
            .find(|e| e.user_agents.iter().any(|frag| ua.contains(&frag.to_ascii_lowercase())))
            .map(|e| e.agent.as_str())
    }

    /// The agent a system prompt belongs to. Coding agents open their system
    /// prompt with their own name ("You are Claude Code, …"), so a fragment
    /// of that opening line is a stable marker across model and tool changes.
    pub fn match_system_prompt(&self, system_prompt: &str) -> Option<&str> {
        let prompt = system_prompt.to_ascii_lowercase();
        self.entries
            .iter()
            .find(|e| {
                e.system_prompts
                    .iter()
                    .any(|frag| prompt.contains(&frag.to_ascii_lowercase()))
            })
            .map(|e| e.agent.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The request's system prompt, where each wire format keeps it: OpenAI in a
/// `system`-role message, Anthropic in the top-level `system` field, the
/// Responses API in `instructions`. Content-block arrays are folded to their
/// text, which is all a fingerprint can match on anyway.
pub fn system_prompt_text(wire: WireApi, body: &Value) -> Option<String> {
    let text: Option<String> = match wire {
        WireApi::OpenAi => body["messages"]
            .as_array()?
            .iter()
            .find(|m| m["role"] == "system")
            .and_then(|m| content_text(&m["content"])),
        WireApi::Anthropic => content_text(&body["system"]),
        WireApi::Responses => body["instructions"]
            .as_str()
            .map(String::from)
            .or_else(|| {
                body["input"]
                    .as_array()?
                    .iter()
                    .find(|m| m["role"] == "system")
                    .and_then(|m| content_text(&m["content"]))
            }),
    };

    let normalized = text?.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

/// A stable id for calls the client didn't label with `X-Session-ID`.
///
/// Agents resend the whole conversation on every turn, and the *first* user
/// message of a conversation is the task prompt — it stays constant for the
/// length of a task and changes when a new task starts, which is exactly the
/// shape of a session id. Compaction that drops early turns starts a new id;
/// that over-splits rather than under-splits, the same trade `RunGrouping`
/// makes.
pub fn auto_session_id(body: &Value) -> Option<String> {
    let first_user_message = || {
        body["messages"]
            .as_array()
            .or_else(|| body["input"].as_array())?
            .iter()
            .find(|m| m["role"] == "user")
            .and_then(|m| content_text(&m["content"]))
    };
    // The Responses API also accepts `input` as one bare string — a single
    // user message with no shape around it.
    let text = first_user_message().or_else(|| body["input"].as_str().map(String::from))?;

    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }

    let digest = Sha256::digest(normalized.as_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    Some(format!("auto-{hex}"))
}

/// Plain text out of either content shape: a string, or an array of blocks
/// whose `text` fields are concatenated.
fn content_text(content: &Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    let blocks = content.as_array()?;
    let joined = blocks
        .iter()
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(yaml: &str) -> FingerprintTable {
        FingerprintTable::parse(yaml).expect("test yaml parses")
    }

    #[test]
    fn user_agent_matching_is_substring_and_case_insensitive() {
        let t = table(
            "fingerprints:\n  - agent: claude-code\n    user_agents: [\"claude-cli\"]\n",
        );
        assert_eq!(t.match_user_agent("claude-cli/1.0.123 (external, cli)"), Some("claude-code"));
        assert_eq!(t.match_user_agent("SomeLib/2.0 CLAUDE-CLI/1.1"), Some("claude-code"));
        assert_eq!(t.match_user_agent("python-requests/2.31"), None);
    }

    #[test]
    fn system_prompt_matching_finds_the_agents_own_name() {
        let t = table(
            "fingerprints:\n  - agent: claude-code\n    system_prompts: [\"You are Claude Code\"]\n",
        );
        assert_eq!(
            t.match_system_prompt("You are Claude Code, Anthropic's official CLI for Claude."),
            Some("claude-code")
        );
        assert_eq!(t.match_system_prompt("You are a helpful assistant."), None);
    }

    #[test]
    fn opencode_user_agent_matches_the_versioned_ua() {
        let t = table(
            "fingerprints:\n  - agent: opencode\n    user_agents: [\"opencode/\"]\n",
        );
        assert_eq!(t.match_user_agent("opencode/1.17.13"), Some("opencode"));
        assert_eq!(t.match_user_agent("OPENCODE/1.17.13"), Some("opencode"));
        assert_eq!(t.match_user_agent("python-requests/2.31"), None);
    }

    #[test]
    fn opencode_system_prompt_matches_the_environment_line() {
        let t = table(
            "fingerprints:\n  - agent: opencode\n    system_prompts: [\"You are powered by the model named\"]\n",
        );
        assert_eq!(
            t.match_system_prompt("You are powered by the model named claude-sonnet-4-5. The exact model ID is anthropic/claude-sonnet-4-5"),
            Some("opencode")
        );
        assert_eq!(t.match_system_prompt("You are a helpful assistant."), None);
    }

    #[test]
    fn file_order_breaks_ties_first_match_wins() {
        let t = table(
            "fingerprints:\n\
             \x20 - agent: specific\n    user_agents: [\"agent-x-pro\"]\n\
             \x20 - agent: generic\n    user_agents: [\"agent-x\"]\n",
        );
        assert_eq!(t.match_user_agent("agent-x-pro/1.0"), Some("specific"));
        assert_eq!(t.match_user_agent("agent-x/1.0"), Some("generic"));
    }

    #[test]
    fn entries_without_matchers_never_match() {
        let t = table("fingerprints:\n  - agent: ghost\n");
        assert_eq!(t.match_user_agent("anything"), None);
        assert_eq!(t.match_system_prompt("anything"), None);
    }

    #[test]
    fn a_nameless_entry_is_rejected_rather_than_silently_unmatchable() {
        assert!(FingerprintTable::parse("fingerprints:\n  - user_agents: [\"x\"]\n").is_err());
    }

    #[test]
    fn an_unparseable_file_is_an_error_not_a_crash() {
        assert!(FingerprintTable::parse("fingerprints: [ not: valid: yaml").is_err());
    }

    #[test]
    fn system_prompt_is_read_from_each_wires_home_for_it() {
        let openai: Value = serde_json::from_str(
            r#"{"messages": [
                {"role": "system", "content": [{"type": "text", "text": "You are Claude Code."}]},
                {"role": "user", "content": "hi"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(
            system_prompt_text(WireApi::OpenAi, &openai).as_deref(),
            Some("You are Claude Code.")
        );

        let anthropic: Value = serde_json::from_str(
            r#"{"system": "You are Claude Code.", "messages": [{"role": "user", "content": "hi"}]}"#,
        )
        .unwrap();
        assert_eq!(
            system_prompt_text(WireApi::Anthropic, &anthropic).as_deref(),
            Some("You are Claude Code.")
        );

        let responses: Value = serde_json::from_str(
            r#"{"instructions": "You are Claude Code.", "input": "hi"}"#,
        )
        .unwrap();
        assert_eq!(
            system_prompt_text(WireApi::Responses, &responses).as_deref(),
            Some("You are Claude Code.")
        );
    }

    #[test]
    fn a_body_without_a_system_prompt_yields_none_for_every_wire() {
        let body: Value = serde_json::from_str(
            r#"{"messages": [{"role": "user", "content": "hi"}]}"#,
        )
        .unwrap();
        assert_eq!(system_prompt_text(WireApi::OpenAi, &body), None);
        assert_eq!(system_prompt_text(WireApi::Anthropic, &body), None);
        assert_eq!(system_prompt_text(WireApi::Responses, &body), None);
    }

    #[test]
    fn the_same_first_user_message_hashes_to_the_same_session_id() {
        // The conversation grows between turns but the *first* user message
        // is constant, so both turns land in one session.
        let turn_1: Value = serde_json::from_str(
            r#"{"messages": [{"role": "user", "content": "fix the login bug"}]}"#,
        )
        .unwrap();
        let turn_2: Value = serde_json::from_str(
            r#"{"messages": [
                {"role": "user", "content": "fix the login bug"},
                {"role": "assistant", "content": "on it"},
                {"role": "user", "content": [{"type": "text", "text": "still broken"}]}
            ]}"#,
        )
        .unwrap();
        assert_eq!(auto_session_id(&turn_1), auto_session_id(&turn_2));
        assert!(auto_session_id(&turn_1).unwrap().starts_with("auto-"));
    }

    #[test]
    fn a_different_task_gets_a_different_session_id() {
        let a: Value = serde_json::from_str(r#"{"messages": [{"role": "user", "content": "fix the login bug"}]}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"messages": [{"role": "user", "content": "write the README"}]}"#).unwrap();
        assert_ne!(auto_session_id(&a), auto_session_id(&b));
    }

    #[test]
    fn responses_input_arrays_hash_like_messages() {
        let by_messages: Value = serde_json::from_str(
            r#"{"messages": [{"role": "user", "content": "fix the login bug"}]}"#,
        )
        .unwrap();
        let by_input: Value = serde_json::from_str(
            r#"{"input": [{"role": "user", "content": "fix the login bug"}]}"#,
        )
        .unwrap();
        assert_eq!(auto_session_id(&by_messages), auto_session_id(&by_input));
    }

    #[test]
    fn responses_bare_string_input_hashes_like_one_user_message() {
        let array: Value = serde_json::from_str(
            r#"{"input": [{"role": "user", "content": "write the README"}]}"#,
        )
        .unwrap();
        let string: Value = serde_json::from_str(r#"{"input": "write the README"}"#).unwrap();
        assert_eq!(auto_session_id(&array), auto_session_id(&string));
    }

    #[test]
    fn a_body_with_no_user_message_has_no_auto_session() {
        let body: Value = serde_json::from_str(r#"{"messages": [{"role": "system", "content": "sys"}]}"#).unwrap();
        assert_eq!(auto_session_id(&body), None);
    }
}
