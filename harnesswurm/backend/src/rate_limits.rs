//! Reads the quota headers both providers already send on every response.
//!
//! These are the only authoritative answer to "are my limits gone?" — the
//! alternative is inferring it from 429s, which tells you only after you've
//! already been blocked. Headers let the dashboard show "412 requests left,
//! resets in 4m" *before* the wall is hit.
//!
//! Header names differ per provider and neither set is guaranteed present
//! (they're absent on some plans and on cached/error paths), so every field
//! is optional and a snapshot with nothing in it is simply not stored.

use axum::http::HeaderMap;

/// One provider quota reading, taken from the response headers of a single
/// call. Token and request budgets are tracked separately because they run
/// out independently — a long-context agent exhausts tokens while barely
/// touching its request budget.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RateLimitSnapshot {
    pub requests_limit: Option<i64>,
    pub requests_remaining: Option<i64>,
    pub requests_reset: Option<String>,
    pub tokens_limit: Option<i64>,
    pub tokens_remaining: Option<i64>,
    pub tokens_reset: Option<String>,
    /// Seconds to wait, from `retry-after`. Only sent when already blocked.
    pub retry_after_s: Option<i64>,
}

impl RateLimitSnapshot {
    /// True when the snapshot carries nothing worth persisting, so callers
    /// can skip writing an all-NULL row for every unmetered response.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

fn header_str(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name)?.to_str().ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn header_i64(headers: &HeaderMap, name: &str) -> Option<i64> {
    header_str(headers, name)?.parse().ok()
}

/// Takes the first header present from `names`, so a provider that renames a
/// header (or sends both an old and new spelling) still reads correctly.
fn first_i64(headers: &HeaderMap, names: &[&str]) -> Option<i64> {
    names.iter().find_map(|n| header_i64(headers, n))
}

fn first_str(headers: &HeaderMap, names: &[&str]) -> Option<String> {
    names.iter().find_map(|n| header_str(headers, n))
}

/// `retry-after` is defined as either a delay in seconds or an HTTP date.
/// Only the numeric form is understood here; a date form yields `None`
/// rather than a wrong number, and the UI falls back to the error message.
fn parse_retry_after(headers: &HeaderMap) -> Option<i64> {
    header_str(headers, "retry-after")?.parse::<f64>().ok().map(|s| s.round() as i64)
}

pub fn extract(headers: &HeaderMap) -> RateLimitSnapshot {
    RateLimitSnapshot {
        // Anthropic spells these `anthropic-ratelimit-*`; OpenAI spells them
        // `x-ratelimit-*` with the noun and dimension in the other order.
        requests_limit: first_i64(headers, &["anthropic-ratelimit-requests-limit", "x-ratelimit-limit-requests"]),
        requests_remaining: first_i64(
            headers,
            &["anthropic-ratelimit-requests-remaining", "x-ratelimit-remaining-requests"],
        ),
        requests_reset: first_str(headers, &["anthropic-ratelimit-requests-reset", "x-ratelimit-reset-requests"]),
        // Anthropic also breaks tokens into input/output buckets; the
        // combined `tokens` bucket is preferred and the input bucket is the
        // fallback, since that's the one long-context agents exhaust first.
        tokens_limit: first_i64(
            headers,
            &["anthropic-ratelimit-tokens-limit", "anthropic-ratelimit-input-tokens-limit", "x-ratelimit-limit-tokens"],
        ),
        tokens_remaining: first_i64(
            headers,
            &[
                "anthropic-ratelimit-tokens-remaining",
                "anthropic-ratelimit-input-tokens-remaining",
                "x-ratelimit-remaining-tokens",
            ],
        ),
        tokens_reset: first_str(
            headers,
            &["anthropic-ratelimit-tokens-reset", "anthropic-ratelimit-input-tokens-reset", "x-ratelimit-reset-tokens"],
        ),
        retry_after_s: parse_retry_after(headers),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderName, HeaderValue};

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.insert(HeaderName::from_bytes(k.as_bytes()).unwrap(), HeaderValue::from_str(v).unwrap());
        }
        map
    }

    #[test]
    fn reads_anthropic_style_headers() {
        let snapshot = extract(&headers(&[
            ("anthropic-ratelimit-requests-limit", "1000"),
            ("anthropic-ratelimit-requests-remaining", "997"),
            ("anthropic-ratelimit-requests-reset", "2026-08-31T12:05:00Z"),
            ("anthropic-ratelimit-tokens-limit", "80000"),
            ("anthropic-ratelimit-tokens-remaining", "12500"),
        ]));

        assert_eq!(snapshot.requests_limit, Some(1000));
        assert_eq!(snapshot.requests_remaining, Some(997));
        assert_eq!(snapshot.requests_reset.as_deref(), Some("2026-08-31T12:05:00Z"));
        assert_eq!(snapshot.tokens_remaining, Some(12500));
        assert!(!snapshot.is_empty());
    }

    #[test]
    fn reads_openai_style_headers() {
        let snapshot = extract(&headers(&[
            ("x-ratelimit-limit-requests", "500"),
            ("x-ratelimit-remaining-requests", "0"),
            ("x-ratelimit-reset-tokens", "6m0s"),
            ("x-ratelimit-remaining-tokens", "0"),
        ]));

        assert_eq!(snapshot.requests_limit, Some(500));
        assert_eq!(snapshot.requests_remaining, Some(0));
        assert_eq!(snapshot.tokens_remaining, Some(0));
        assert_eq!(snapshot.tokens_reset.as_deref(), Some("6m0s"));
    }

    #[test]
    fn falls_back_to_the_anthropic_input_token_bucket() {
        let snapshot = extract(&headers(&[
            ("anthropic-ratelimit-input-tokens-limit", "40000"),
            ("anthropic-ratelimit-input-tokens-remaining", "39000"),
        ]));

        assert_eq!(snapshot.tokens_limit, Some(40000));
        assert_eq!(snapshot.tokens_remaining, Some(39000));
    }

    #[test]
    fn prefers_the_combined_token_bucket_over_the_input_bucket() {
        let snapshot = extract(&headers(&[
            ("anthropic-ratelimit-tokens-remaining", "100"),
            ("anthropic-ratelimit-input-tokens-remaining", "900"),
        ]));

        assert_eq!(snapshot.tokens_remaining, Some(100));
    }

    #[test]
    fn parses_numeric_retry_after() {
        assert_eq!(extract(&headers(&[("retry-after", "42")])).retry_after_s, Some(42));
    }

    #[test]
    fn ignores_http_date_retry_after_rather_than_guessing() {
        let snapshot = extract(&headers(&[("retry-after", "Wed, 21 Oct 2026 07:28:00 GMT")]));
        assert_eq!(snapshot.retry_after_s, None);
        assert!(snapshot.is_empty());
    }

    #[test]
    fn a_response_without_quota_headers_is_empty() {
        assert!(extract(&headers(&[("content-type", "application/json")])).is_empty());
    }

    #[test]
    fn blank_header_values_are_not_mistaken_for_zero() {
        let snapshot = extract(&headers(&[("x-ratelimit-remaining-requests", "  ")]));
        assert_eq!(snapshot.requests_remaining, None);
        assert!(snapshot.is_empty());
    }
}
