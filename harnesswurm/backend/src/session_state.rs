//! Derives "what is this agent doing right now" from the last call the proxy
//! saw on a session.
//!
//! The proxy is a sidecar: it never talks to the agent process, so it can't
//! ask. Everything here is inferred from the shape of one request/response
//! pair, which turns out to be enough because agent harnesses all run the
//! same loop — send a turn, either get tool calls back (and go run them) or
//! get a plain answer back (and hand control to the human).
//!
//! Kept separate from the DB and HTTP layers so the rules are pure
//! functions that can be tested without a database or a provider.

use serde::Serialize;

/// A call that is still open this long is far more likely hung (client gone,
/// provider stalled) than genuinely still generating, so it stops being
/// reported as healthy "thinking".
const STALLED_AFTER_SECS: i64 = 600;

/// How long after a `tool_use` turn the agent is still assumed to be off
/// running that tool. Beyond this the loop has almost certainly stopped
/// (agent exited, user hit escape) rather than a single tool taking minutes.
const TOOL_LOOP_GRACE_SECS: i64 = 180;

/// Call-level outcomes written to `tasks.status`. `&str` rather than an enum
/// because these round-trip through SQLite and JSON, and an unknown value
/// from an older/newer schema must degrade gracefully instead of failing to
/// parse.
pub const STATUS_IN_FLIGHT: &str = "in_flight";
pub const STATUS_OK: &str = "ok";
pub const STATUS_RATE_LIMITED: &str = "rate_limited";
pub const STATUS_OVERLOADED: &str = "overloaded";
pub const STATUS_ERROR: &str = "error";
pub const STATUS_INTERRUPTED: &str = "interrupted";

/// What the last call on a session tells us, as read out of the database.
#[derive(Debug, Clone, Default)]
pub struct LastCall {
    pub status: Option<String>,
    /// The response handed the turn back to the human (an explicit question
    /// tool, or a plain end-of-turn with no tool calls).
    pub awaiting_input: bool,
    /// Provider-reported reason the turn ended: `tool_use`/`tool_calls`,
    /// `end_turn`/`stop`, `max_tokens`/`length`.
    pub stop_reason: Option<String>,
    pub question_text: Option<String>,
    pub error_type: Option<String>,
    pub error_message: Option<String>,
    /// Seconds since the call *started* — for an open call, how long the
    /// agent has been waiting on the model.
    pub age_seconds: i64,
    /// Seconds since the call *finished* (or started, if it never did) — for
    /// a closed call, how long the session has been quiet.
    pub idle_seconds: i64,
    /// Seconds until the provider says the limit resets, from `retry-after`.
    pub retry_after_s: Option<i64>,
}

/// The state shown per session. `state` is the machine-readable kind the UI
/// keys colors and filters off; `label`/`detail` are what a human reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentState {
    pub state: &'static str,
    pub label: String,
    pub detail: Option<String>,
    /// True when this session wants something from the human right now — the
    /// one flag worth a notification or a badge count.
    pub needs_attention: bool,
}

impl AgentState {
    fn new(state: &'static str, label: impl Into<String>, detail: Option<String>, needs_attention: bool) -> Self {
        Self { state, label: label.into(), detail, needs_attention }
    }
}

/// Renders a duration the way a status line should: coarse and glanceable,
/// never "372s".
pub fn humanize_secs(secs: i64) -> String {
    if secs < 0 {
        return "just now".to_string();
    }
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

fn is_tool_stop(stop_reason: Option<&str>) -> bool {
    matches!(stop_reason, Some("tool_use") | Some("tool_calls"))
}

/// One recorded tool invocation: what was called, plus a fingerprint of the
/// arguments it was called with. The fingerprint is what separates "the
/// agent grepping five different patterns" (normal exploration) from "the
/// agent grepping the same pattern five times" (a loop). Null for rows
/// recorded before fingerprints existed — those never match, so legacy
/// history stays silent rather than crying wolf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolFingerprint {
    pub name: String,
    pub args_hash: Option<String>,
}

/// The tail of one session's call history, oldest first — enough context
/// for `detect_waste` to see repetition across calls.
#[derive(Debug, Clone, Default)]
pub struct RecentCall {
    pub status: Option<String>,
    pub error_type: Option<String>,
    pub error_message: Option<String>,
    pub tool_calls: Vec<ToolFingerprint>,
}

/// A detected pattern that burns calls without making progress. Surfaced on
/// the session card and folded into the attention badge: nobody watches an
/// agent spin, that is exactly what the sidecar is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasteSignal {
    /// Machine-readable kind: `tool_loop` or `error_loop`.
    pub kind: &'static str,
    /// What a human reads on the session card.
    pub detail: String,
}

/// How many recent calls per session feed the detector. Generous enough to
/// cover the longest cycle below with room for interleaved noise.
pub const RECENT_CALL_WINDOW: i64 = 12;

/// Identical failures needed before the same error is called a loop. Two is
/// how a normal retry looks; three in a row with the very same message means
/// nothing is changing between attempts.
const ERROR_LOOP_MIN: usize = 3;

/// The shortest repeating cycle searched for. Beyond period 3 a "loop" is
/// indistinguishable from an ordinary plan.
const TOOL_LOOP_MAX_PERIOD: usize = 3;

/// A cycle must repeat this many times to count. Three reads of the same
/// file with the same arguments in a row is already waste; anything shorter
/// is how agents normally double-check.
const TOOL_LOOP_REPS: usize = 3;

fn same_failed_call(a: &RecentCall, b: &RecentCall) -> bool {
    a.status.as_deref() == Some(STATUS_ERROR)
        && b.status.as_deref() == Some(STATUS_ERROR)
        && a.error_type == b.error_type
        && a.error_message == b.error_message
}

fn fingerprint_eq(a: &ToolFingerprint, b: &ToolFingerprint) -> bool {
    a.name == b.name && a.args_hash.is_some() && a.args_hash == b.args_hash
}

/// Detects calls spent without progress, from the session's recent history.
///
/// Two patterns, checked in order:
///
/// - **Error loop** — a trailing run of identical failures. Trailing only:
///   if the newest call succeeded, whatever loop came before it is broken.
/// - **Tool loop** — the shortest suffix of the flattened tool sequence
///   (oldest to newest) that repeats the same fingerprint cycle three
///   times. Exact fingerprints only, so repeated tool *names* with fresh
///   arguments stay normal work.
pub fn detect_waste(calls: &[RecentCall]) -> Option<WasteSignal> {
    let failures = calls
        .iter()
        .rev()
        .take_while(|call| call.status.as_deref() == Some(STATUS_ERROR))
        .count();
    if failures >= ERROR_LOOP_MIN {
        // Every call in the trailing run matches the last one by
        // `same_failed_call`, which chains transitively from the newest.
        let newest = &calls[calls.len() - 1];
        let all_same = calls[calls.len() - failures..]
            .iter()
            .all(|call| same_failed_call(call, newest));
        if all_same {
            return Some(WasteSignal {
                kind: "error_loop",
                detail: format!(
                    "The last {failures} calls all failed with the same error{} — retrying is not fixing it",
                    newest
                        .error_type
                        .as_deref()
                        .map(|t| format!(" ({t})"))
                        .unwrap_or_default()
                ),
            });
        }
    }

    let mut fingerprints: Vec<&ToolFingerprint> =
        calls.iter().flat_map(|call| call.tool_calls.iter()).collect();
    if fingerprints.len() > RECENT_CALL_WINDOW as usize * TOOL_LOOP_MAX_PERIOD {
        fingerprints = fingerprints.split_off(fingerprints.len() - RECENT_CALL_WINDOW as usize * TOOL_LOOP_MAX_PERIOD);
    }

    for period in 1..=TOOL_LOOP_MAX_PERIOD {
        let needed = period * TOOL_LOOP_REPS;
        if fingerprints.len() < needed {
            continue;
        }
        let tail_start = fingerprints.len() - needed;
        let cycle: Vec<&ToolFingerprint> = fingerprints[tail_start..tail_start + period].to_vec();
        let repeats = (0..needed).all(|i| fingerprint_eq(fingerprints[tail_start + i], cycle[i % period]));
        if repeats {
            let detail = if period == 1 {
                // Report the whole trailing run, not just the minimum —
                // five identical calls in a row should not read as three.
                let run = fingerprints
                    .iter()
                    .rev()
                    .take_while(|f| fingerprint_eq(f, cycle[0]))
                    .count();
                format!(
                    "`{}` called {run}× in a row with identical arguments",
                    cycle[0].name
                )
            } else {
                format!(
                    "Repeating the same {}-step cycle {}×: {}",
                    period,
                    TOOL_LOOP_REPS,
                    cycle.iter().map(|f| f.name.as_str()).collect::<Vec<_>>().join(" → ")
                )
            };
            return Some(WasteSignal { kind: "tool_loop", detail });
        }
    }

    None
}

/// Maps the last call on a session onto a human-facing state.
///
/// Order matters: an open request beats everything (it's happening now), then
/// hard provider outcomes (limits, errors) which the human must act on, then
/// the softer "who holds the turn" question.
pub fn derive_state(call: &LastCall) -> AgentState {
    match call.status.as_deref() {
        Some(STATUS_IN_FLIGHT) => {
            if call.age_seconds > STALLED_AFTER_SECS {
                AgentState::new(
                    "stalled",
                    "Stalled",
                    Some(format!(
                        "No response for {} — the request looks hung, or the proxy was restarted mid-call",
                        humanize_secs(call.age_seconds)
                    )),
                    true,
                )
            } else {
                AgentState::new(
                    "working",
                    "Thinking",
                    Some(format!("Waiting on the model for {}", humanize_secs(call.age_seconds))),
                    false,
                )
            }
        }

        Some(STATUS_RATE_LIMITED) => {
            // `retry-after` was issued when the call failed, so the wait left
            // is that window minus however long ago that was. Reporting the
            // raw header would keep saying "retry in 60s" an hour later.
            let remaining = call.retry_after_s.map(|secs| secs - call.idle_seconds);
            let detail = match remaining {
                Some(secs) if secs > 0 => format!("Provider says retry in {}", humanize_secs(secs)),
                Some(_) => "The retry window has passed — the next call should go through".to_string(),
                None => call
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "Provider returned 429".to_string()),
            };
            AgentState::new("rate_limited", "Rate limited", Some(detail), true)
        }

        Some(STATUS_OVERLOADED) => AgentState::new(
            "overloaded",
            "Provider overloaded",
            call.error_message.clone().or_else(|| Some("Upstream is shedding load; the agent will usually retry".to_string())),
            false,
        ),

        Some(STATUS_ERROR) => {
            let label = match call.error_type.as_deref() {
                Some("authentication_error") | Some("invalid_api_key") => "Auth failed",
                Some("invalid_request_error") => "Request rejected",
                Some("context_length_exceeded") => "Context full",
                _ => "Error",
            };
            AgentState::new("error", label, call.error_message.clone(), true)
        }

        Some(STATUS_INTERRUPTED) => AgentState::new(
            "interrupted",
            "Interrupted",
            Some("The response was cut off before it finished — cancelled, or the connection dropped".to_string()),
            false,
        ),

        // A completed call: the question is only who holds the turn now.
        Some(STATUS_OK) => {
            if call.awaiting_input {
                let detail = call
                    .question_text
                    .clone()
                    .or_else(|| Some(format!("Idle for {} since handing the turn back", humanize_secs(call.idle_seconds))));
                return AgentState::new("waiting_for_you", "Waiting for you", detail, true);
            }

            if is_tool_stop(call.stop_reason.as_deref()) {
                if call.idle_seconds <= TOOL_LOOP_GRACE_SECS {
                    return AgentState::new(
                        "working",
                        "Running a tool",
                        Some(format!("Tool call issued {} ago", humanize_secs(call.idle_seconds))),
                        false,
                    );
                }
                return AgentState::new(
                    "idle",
                    "Idle",
                    Some(format!(
                        "Stopped mid tool-loop {} ago — the agent never came back",
                        humanize_secs(call.idle_seconds)
                    )),
                    false,
                );
            }

            if matches!(call.stop_reason.as_deref(), Some("max_tokens") | Some("length")) {
                return AgentState::new(
                    "truncated",
                    "Hit output limit",
                    Some("The last response was cut off by max_tokens".to_string()),
                    true,
                );
            }

            AgentState::new("idle", "Idle", Some(format!("Quiet for {}", humanize_secs(call.idle_seconds))), false)
        }

        // No status recorded: rows from before status tracking existed.
        _ => AgentState::new("unknown", "Unknown", Some("No status recorded for this call".to_string()), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(status: &str) -> LastCall {
        LastCall { status: Some(status.to_string()), ..Default::default() }
    }

    #[test]
    fn open_request_reads_as_thinking() {
        let state = derive_state(&LastCall { age_seconds: 12, ..call(STATUS_IN_FLIGHT) });
        assert_eq!(state.state, "working");
        assert_eq!(state.label, "Thinking");
        assert!(!state.needs_attention);
    }

    #[test]
    fn long_open_request_reads_as_stalled_and_needs_attention() {
        let state = derive_state(&LastCall { age_seconds: STALLED_AFTER_SECS + 1, ..call(STATUS_IN_FLIGHT) });
        assert_eq!(state.state, "stalled");
        assert!(state.needs_attention);
    }

    #[test]
    fn tool_use_turn_means_the_agent_is_working_not_waiting() {
        let state = derive_state(&LastCall {
            stop_reason: Some("tool_use".to_string()),
            idle_seconds: 5,
            ..call(STATUS_OK)
        });
        assert_eq!(state.state, "working");
        assert_eq!(state.label, "Running a tool");
    }

    #[test]
    fn a_tool_loop_that_never_resumed_falls_back_to_idle() {
        let state = derive_state(&LastCall {
            stop_reason: Some("tool_calls".to_string()),
            idle_seconds: TOOL_LOOP_GRACE_SECS + 1,
            ..call(STATUS_OK)
        });
        assert_eq!(state.state, "idle");
        assert!(state.detail.unwrap().contains("mid tool-loop"));
    }

    #[test]
    fn end_of_turn_hands_control_back_to_the_human() {
        let state = derive_state(&LastCall {
            awaiting_input: true,
            stop_reason: Some("end_turn".to_string()),
            idle_seconds: 30,
            ..call(STATUS_OK)
        });
        assert_eq!(state.state, "waiting_for_you");
        assert!(state.needs_attention);
    }

    #[test]
    fn an_explicit_question_becomes_the_detail_line() {
        let state = derive_state(&LastCall {
            awaiting_input: true,
            question_text: Some("Use TypeScript?".to_string()),
            ..call(STATUS_OK)
        });
        assert_eq!(state.state, "waiting_for_you");
        assert_eq!(state.detail.as_deref(), Some("Use TypeScript?"));
    }

    #[test]
    fn rate_limit_reports_the_providers_own_retry_window() {
        let state = derive_state(&LastCall { retry_after_s: Some(120), ..call(STATUS_RATE_LIMITED) });
        assert_eq!(state.state, "rate_limited");
        assert_eq!(state.detail.as_deref(), Some("Provider says retry in 2m"));
        assert!(state.needs_attention);
    }

    #[test]
    fn a_retry_window_that_has_already_elapsed_is_not_still_counted_down() {
        let state = derive_state(&LastCall {
            retry_after_s: Some(60),
            idle_seconds: 3600,
            ..call(STATUS_RATE_LIMITED)
        });
        assert_eq!(state.state, "rate_limited");
        assert_eq!(
            state.detail.as_deref(),
            Some("The retry window has passed — the next call should go through")
        );
    }

    #[test]
    fn the_retry_countdown_accounts_for_time_already_waited() {
        let state = derive_state(&LastCall {
            retry_after_s: Some(180),
            idle_seconds: 60,
            ..call(STATUS_RATE_LIMITED)
        });
        assert_eq!(state.detail.as_deref(), Some("Provider says retry in 2m"));
    }

    #[test]
    fn rate_limit_without_retry_after_falls_back_to_the_error_message() {
        let state = derive_state(&LastCall {
            error_message: Some("quota exceeded for this org".to_string()),
            ..call(STATUS_RATE_LIMITED)
        });
        assert_eq!(state.detail.as_deref(), Some("quota exceeded for this org"));
    }

    #[test]
    fn auth_errors_get_their_own_label() {
        let state = derive_state(&LastCall {
            error_type: Some("authentication_error".to_string()),
            error_message: Some("invalid x-api-key".to_string()),
            ..call(STATUS_ERROR)
        });
        assert_eq!(state.label, "Auth failed");
        assert!(state.needs_attention);
    }

    #[test]
    fn overload_is_not_the_humans_problem_to_act_on() {
        let state = derive_state(&call(STATUS_OVERLOADED));
        assert_eq!(state.state, "overloaded");
        assert!(!state.needs_attention);
    }

    #[test]
    fn truncated_output_is_flagged_because_the_answer_is_incomplete() {
        let state = derive_state(&LastCall { stop_reason: Some("max_tokens".to_string()), ..call(STATUS_OK) });
        assert_eq!(state.state, "truncated");
        assert!(state.needs_attention);
    }

    #[test]
    fn rows_from_before_status_tracking_do_not_masquerade_as_healthy() {
        let state = derive_state(&LastCall::default());
        assert_eq!(state.state, "unknown");
        assert!(!state.needs_attention);
    }

    #[test]
    fn durations_stay_glanceable() {
        assert_eq!(humanize_secs(0), "0s");
        assert_eq!(humanize_secs(59), "59s");
        assert_eq!(humanize_secs(60), "1m");
        assert_eq!(humanize_secs(3599), "59m");
        assert_eq!(humanize_secs(3600), "1h");
        assert_eq!(humanize_secs(86_400), "1d");
        assert_eq!(humanize_secs(-3), "just now");
    }

    fn error_call(message: &str) -> RecentCall {
        RecentCall {
            status: Some(STATUS_ERROR.to_string()),
            error_type: Some("invalid_request_error".to_string()),
            error_message: Some(message.to_string()),
            ..Default::default()
        }
    }

    fn tool_call(name: &str, args: &str) -> RecentCall {
        RecentCall {
            status: Some(STATUS_OK.to_string()),
            tool_calls: vec![ToolFingerprint {
                name: name.to_string(),
                args_hash: Some(args.to_string()),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn three_identical_failures_in_a_row_are_an_error_loop() {
        let calls = vec![error_call("quota blown"), error_call("quota blown"), error_call("quota blown")];
        let waste = detect_waste(&calls).expect("three identical failures");
        assert_eq!(waste.kind, "error_loop");
        assert!(waste.detail.contains("3 calls"));
        assert!(waste.detail.contains("invalid_request_error"));
    }

    #[test]
    fn two_failures_are_just_a_retry() {
        let calls = vec![error_call("quota blown"), error_call("quota blown")];
        assert!(detect_waste(&calls).is_none());
    }

    #[test]
    fn failures_with_different_messages_are_healthy_iteration() {
        let calls = vec![
            error_call("missing field: name"),
            error_call("missing field: age"),
            error_call("missing field: email"),
        ];
        assert!(detect_waste(&calls).is_none());
    }

    #[test]
    fn a_success_after_the_failures_breaks_the_loop() {
        let calls = vec![
            error_call("quota blown"),
            error_call("quota blown"),
            RecentCall { status: Some(STATUS_OK.to_string()), ..Default::default() },
        ];
        assert!(detect_waste(&calls).is_none());
    }

    #[test]
    fn rate_limited_retries_are_never_called_a_loop() {
        // Backing off and retrying a 429 is exactly what the agent should do.
        let limited = RecentCall { status: Some(STATUS_RATE_LIMITED.to_string()), ..Default::default() };
        let calls = vec![limited.clone(), limited.clone(), limited];
        assert!(detect_waste(&calls).is_none());
    }

    #[test]
    fn three_identical_tool_calls_are_a_loop() {
        let calls = vec![tool_call("bash", "ls"), tool_call("bash", "ls"), tool_call("bash", "ls")];
        let waste = detect_waste(&calls).expect("same command three times");
        assert_eq!(waste.kind, "tool_loop");
        assert!(waste.detail.contains("`bash` called 3×"));
    }

    #[test]
    fn the_same_tool_with_fresh_arguments_is_normal_work() {
        let calls = vec![
            tool_call("grep", "pattern=a"),
            tool_call("grep", "pattern=b"),
            tool_call("grep", "pattern=c"),
        ];
        assert!(detect_waste(&calls).is_none());
    }

    #[test]
    fn an_alternating_two_step_cycle_is_a_loop() {
        let calls = vec![
            tool_call("read_file", "f=main.rs"),
            tool_call("bash", "cargo build"),
            tool_call("read_file", "f=main.rs"),
            tool_call("bash", "cargo build"),
            tool_call("read_file", "f=main.rs"),
            tool_call("bash", "cargo build"),
        ];
        let waste = detect_waste(&calls).expect("read → build × 3");
        assert_eq!(waste.kind, "tool_loop");
        assert!(waste.detail.contains("2-step cycle"));
        assert!(waste.detail.contains("read_file → bash"));
    }

    #[test]
    fn fingerprints_without_argument_hashes_never_match() {
        // Rows recorded before fingerprints existed must stay silent.
        let legacy = RecentCall {
            status: Some(STATUS_OK.to_string()),
            tool_calls: vec![ToolFingerprint { name: "bash".to_string(), args_hash: None }],
            ..Default::default()
        };
        let calls = vec![legacy.clone(), legacy.clone(), legacy];
        assert!(detect_waste(&calls).is_none());
    }

    #[test]
    fn a_short_history_has_nothing_to_repeat() {
        let calls = vec![tool_call("bash", "ls"), tool_call("bash", "ls")];
        assert!(detect_waste(&calls).is_none());
        assert!(detect_waste(&[]).is_none());
    }
}
