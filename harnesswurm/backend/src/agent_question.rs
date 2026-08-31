use serde_json::Value;

/// Tool names matching known real coding-agent conventions for "ask the
/// human a question." Greenfield — no existing precedent in this repo to
/// extend — so this is a small, explicit, easy-to-extend allowlist plus a
/// conservative word-boundary fuzzy fallback for conventions not yet seen.
const KNOWN_QUESTION_TOOLS: &[&str] = &["ask_followup_question", "askuserquestion", "ask_user_question"];

pub(crate) fn is_agent_question_tool(name: &str) -> bool {
    let lower = name.to_lowercase();
    let normalized = lower.replace(['_', '-'], "");
    if KNOWN_QUESTION_TOOLS.iter().any(|k| k.replace('_', "") == normalized) {
        return true;
    }

    // Fuzzy fallback: require "ask" as a whole word or a leading prefix, not
    // a raw substring — `.contains("ask")` alone would false-positive on
    // names like "mask_user_data" or "task_user_assignment".
    let words: Vec<&str> = lower.split(['_', '-']).collect();
    let ask_like = lower.starts_with("ask") || words.iter().any(|w| *w == "ask");
    ask_like && (lower.contains("question") || lower.contains("user"))
}

/// Best-effort extraction of human-readable question text from a
/// question-tool's arguments. Argument schemas vary per agent/tool with no
/// shared spec, so this tries known shapes in order and falls back to the
/// raw arguments JSON so nothing is silently lost — mirrors
/// `extract_task_preview`'s "best effort, Option-returning" spirit.
pub(crate) fn extract_question_text(arguments: &Value) -> Option<String> {
    // Claude Code's AskUserQuestion nests one-or-more questions inside a
    // `questions` array, each with its own `question` field — checked first
    // since the flat fallback below can't reach a level-2 field.
    if let Some(questions) = arguments.get("questions").and_then(|v| v.as_array()) {
        let texts: Vec<String> = questions.iter()
            .filter_map(|q| q.get("question").and_then(|t| t.as_str()))
            .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|s| !s.is_empty())
            .collect();
        if !texts.is_empty() {
            return Some(texts.join(" / "));
        }
    }

    for field in ["question", "text", "prompt", "message"] {
        if let Some(s) = arguments.get(field).and_then(|v| v.as_str()) {
            let normalized = s.split_whitespace().collect::<Vec<_>>().join(" ");
            if !normalized.is_empty() {
                return Some(normalized);
            }
        }
    }

    // Unknown schema: surface raw arguments rather than nothing, unless
    // there's nothing there to show.
    match arguments.as_object() {
        Some(obj) if obj.is_empty() => None,
        None if arguments.is_null() => None,
        _ => Some(arguments.to_string()),
    }
}

/// Accumulator for one streamed tool call's name + progressively arriving
/// arguments text. Lives here (not lib.rs) because it exists only in
/// service of question extraction — plain tool-call counting doesn't need
/// it and keeps using its own simpler HashSet.
#[derive(Debug, Clone, Default)]
pub(crate) struct ToolCallAccumulator {
    pub(crate) name: Option<String>,
    pub(crate) arguments: String,
}

/// Linear-scan get-or-insert. Call volume is always small (a handful of
/// in-flight tool calls per response), so a Vec keyed identically to the
/// existing tool-count dedup key is simpler than a second map to keep in
/// sync with it, and gives first-seen order for free.
pub(crate) fn tool_call_entry<'a>(
    tool_calls: &'a mut Vec<(String, ToolCallAccumulator)>,
    key: &str,
) -> &'a mut ToolCallAccumulator {
    if let Some(pos) = tool_calls.iter().position(|(k, _)| k == key) {
        &mut tool_calls[pos].1
    } else {
        tool_calls.push((key.to_string(), ToolCallAccumulator::default()));
        &mut tool_calls.last_mut().unwrap().1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_known_tool_names_case_insensitively() {
        assert!(is_agent_question_tool("ask_followup_question"));
        assert!(is_agent_question_tool("AskUserQuestion"));
    }

    #[test]
    fn fuzzy_fallback_matches_unseen_ask_conventions() {
        assert!(is_agent_question_tool("ask_for_confirmation_from_user"));
        assert!(is_agent_question_tool("ask-user"));
    }

    #[test]
    fn fuzzy_fallback_does_not_false_positive_on_substring_ask() {
        assert!(!is_agent_question_tool("mask_user_data"));
        assert!(!is_agent_question_tool("task_user_assignment"));
        assert!(!is_agent_question_tool("read_file"));
    }

    #[test]
    fn extracts_flat_question_field_in_priority_order() {
        let args: Value = serde_json::from_str(r#"{"question": "A?", "text": "B?"}"#).unwrap();
        assert_eq!(extract_question_text(&args).as_deref(), Some("A?"));
    }

    #[test]
    fn extracts_nested_questions_array_and_joins_multiple() {
        let args: Value = serde_json::from_str(
            r#"{"questions": [{"question": "Use TypeScript?"}, {"question": "Add tests?"}]}"#
        ).unwrap();
        assert_eq!(extract_question_text(&args).as_deref(), Some("Use TypeScript? / Add tests?"));
    }

    #[test]
    fn falls_back_to_raw_json_for_unknown_shape() {
        let args: Value = serde_json::from_str(r#"{"foo": "bar"}"#).unwrap();
        assert_eq!(extract_question_text(&args), Some(r#"{"foo":"bar"}"#.to_string()));
    }

    #[test]
    fn empty_arguments_yield_no_text() {
        let args: Value = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(extract_question_text(&args), None);
    }

    #[test]
    fn tool_call_entry_reuses_existing_key() {
        let mut calls: Vec<(String, ToolCallAccumulator)> = Vec::new();
        tool_call_entry(&mut calls, "0").name = Some("ask_followup_question".to_string());
        tool_call_entry(&mut calls, "0").arguments.push_str("{\"question\":");
        tool_call_entry(&mut calls, "0").arguments.push_str("\"hi?\"}");

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1.name.as_deref(), Some("ask_followup_question"));
        assert_eq!(calls[0].1.arguments, "{\"question\":\"hi?\"}");
    }
}
