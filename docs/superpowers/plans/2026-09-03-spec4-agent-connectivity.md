# Spec §4 Agent Connectivity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fresh opencode and Claude Code runs attribute correctly with zero manual setup, show real cost instead of `≥ $x`, and have copy-paste wrapper recipes that are verified, not guessed.

**Architecture:** Verified fingerprint entries (User-Agent + system-prompt substrings sourced from each agent's public source or live Traffic-tab capture, cited in the commit) plus refreshed `pricing.yaml` flagships, plus unit-tested `harnesswurm run` flag parsing/env passing and documented recipes. No proxy logic change; precedence `/r/…` > headers > fingerprints untouched.

**Tech Stack:** Rust backend (`fingerprints.rs`, `pricing.rs`, `bin/harnesswurm.rs`) + yaml + markdown docs

## Global Constraints

- Rust commands from `harnesswurm/backend`: `cargo test` (173 tests) must stay green; `cargo clippy --lib --bins` introduces no new warnings.
- Never run bare `cargo fmt`; no formatting churn.
- Every changed line must trace to this plan; no drive-by refactors.
- Never fabricate UA strings, system prompts, env-var names, or prices: every value cites its source (URL + line, or Traffic-tab capture) in the commit message. If a value cannot be verified, leave it out and report it as a concern instead of guessing.
- Don't log prompt or response content anywhere new.
- Attribution precedence unchanged: `/r/…` run prefix > `X-*-ID` headers > fingerprints.

---

### Task 1: Verified fingerprints for opencode (+ re-check claude-code)

**Files:**
- Modify: `harnesswurm/backend/fingerprints.yaml`
- Modify: `harnesswurm/backend/src/fingerprints.rs` (tests only)

**Interfaces:**
- Consumes: `FingerprintTable::parse`, `match_user_agent`, `match_system_prompt` (existing, unchanged)
- Produces: new table entries consumed by the unchanged attribution path

- [ ] **Step 1: Find opencode's real outgoing markers (verify, don't guess)**

Locate opencode's User-Agent and system-prompt opening from its public source (e.g. `sst/opencode` on GitHub: search for `User-Agent`/`user-agent` in the fetch/provider code, and the system-prompt template's opening lines). If this environment has live Traffic-tab captures or an installed opencode binary whose `--help`/source can be inspected, prefer that. Record exact substrings + source URL + line (or capture description).

Re-check the existing `claude-code` entry (`user_agents: ["claude-cli"]`, `system_prompts: ["You are Claude Code"]`) against the same kind of source. Default: keep it. Extend only with source-cited evidence.

- [ ] **Step 2: Add entries + unit tests mirroring the existing style**

`fingerprints.yaml` (keep file order = priority, claude-code first unless evidence demands otherwise):
```yaml
  - agent: "opencode"
    user_agents: ["<verified-ua-substring>"]
    system_prompts: ["<verified-prompt-opening>"]
```

Tests in `fingerprints.rs` mirroring `system_prompt_matching_finds_the_agents_own_name` (~line 201):
```rust
#[test]
fn opencode_user_agent_matches() {
    let t = FingerprintTable::parse(
        "fingerprints:\n  - agent: opencode\n    user_agents: [\"<verified-ua-substring>\"]\n",
    ).unwrap();
    assert_eq!(t.match_user_agent("...<full UA sample from source>..."), Some("opencode"));
    assert_eq!(t.match_user_agent("some-other-agent/1.0"), None);
}

#[test]
fn opencode_system_prompt_matches() {
    let t = FingerprintTable::parse(
        "fingerprints:\n  - agent: opencode\n    system_prompts: [\"<verified-prompt-opening>\"]\n",
    ).unwrap();
    assert_eq!(t.match_system_prompt("<full prompt opening from source>"), Some("opencode"));
    assert_eq!(t.match_system_prompt("You are a helpful assistant."), None);
}
```
(`match_user_agent` lowercases both sides per `fingerprints.rs:76`; write the full UA sample exactly as the source shows it.)

- [ ] **Step 3: Verify**

Run: `cargo test fingerprints`
Expected: all PASS including the 2 new tests

Run: `cargo clippy --lib --bins`
Expected: only the 3 pre-existing warnings

- [ ] **Step 4: Commit (cite sources)**

```bash
git add harnesswurm/backend/fingerprints.yaml harnesswurm/backend/src/fingerprints.rs
git commit -m "Attribute opencode traffic out of the box" -m "UA/prompt substrings verified against <source URL>. Re-checked claude-code entry: <kept|extended because ...>."
```

### Task 2: Pricing refresh for the models these agents hit

**Files:**
- Modify: `harnesswurm/backend/pricing.yaml`
- Modify: `harnesswurm/backend/src/pricing.rs` (tests only, extend existing)

**Interfaces:**
- Consumes: `PricingTable::estimate_cost` longest-prefix matching (unchanged)
- Produces: priced models so opencode/Claude runs show cost; unpriced stays excluded from ranking (never $0-wins)

- [ ] **Step 1: Verify current prices (no guessing)**

Check the current provider pricing pages (Anthropic + OpenAI) for the flagship models these agents actually call (Claude Sonnet/Opus/Haiku current generation, GPT current flagships). Compare against `pricing.yaml`'s starter set (gpt-4o/mini/3.5-turbo, claude-opus/sonnet/haiku-4-5). For each addition or correction record the page + number. The `name` field must be the longest stable prefix that matches dated variants (e.g. `claude-sonnet-4-5` matches `claude-sonnet-4-5-20250929`); keep `provider`, add `cache_write_per_million`/`cache_read_per_million` where the page quotes them.

- [ ] **Step 2: Update yaml + extend pricing tests**

Entries keep the existing shape:
```yaml
  - name: "<verified-prefix>"
    provider: "<openai|anthropic>"
    input_per_million: <n>
    cache_read_per_million: <n>
    output_per_million: <n>
```
Extend the existing longest-prefix tests in `pricing.rs` with one dated-variant case per added prefix (e.g. `<prefix>-20250929` resolves to the new entry). Do not remove existing entries.

- [ ] **Step 3: Verify**

Run: `cargo test pricing`
Expected: PASS including new cases

Run: `cargo test`
Expected: 173+ PASS, 0 fail

- [ ] **Step 4: Commit (cite sources)**

```bash
git add harnesswurm/backend/pricing.yaml harnesswurm/backend/src/pricing.rs
git commit -m "Refresh pricing for current flagship models" -m "Verified against <provider page URLs>. <Added|corrected> entries: <list>."
```

### Task 3: Wrapper parsing tests + verified recipes

**Files:**
- Modify: `harnesswurm/backend/src/bin/harnesswurm.rs` (tests + doc-comment/USAGE recipe lines)
- Modify: `harnesswurm/README.md` (recipes section — locate the existing `harnesswurm run` docs first)

**Interfaces:**
- Consumes: `run(args: Vec<String>) -> Result<ExitCode, String>` (unchanged signature)
- Produces: tested flag parsing + env passing; recipes users copy-paste

- [ ] **Step 1: Write failing unit tests for flag parsing and env passing**

Append `#[cfg(test)] mod tests` to `bin/harnesswurm.rs` (check first whether one exists; if it does, extend it):
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn missing_subcommand_is_an_error() {
        assert!(run(args("--agent x -- echo hi")).is_err());
    }

    #[test]
    fn missing_separator_is_an_error() {
        assert_eq!(
            run(args("run --agent x echo hi")).unwrap_err(),
            "no '--' separating the flags from the command to run"
        );
    }

    #[test]
    fn missing_agent_is_an_error() {
        assert!(run(args("run -- echo hi")).is_err());
    }

    #[test]
    fn unknown_flag_is_an_error() {
        assert!(run(args("run --agent x --bogus y -- echo hi")).is_err());
    }

    #[test]
    fn anthropic_base_url_reaches_the_child() {
        let code = run(args("run --agent recipe-test --experiment recipe -- printenv ANTHROPIC_BASE_URL")).unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn openai_base_url_reaches_the_child() {
        let code = run(args("run --agent recipe-test --experiment recipe -- printenv OPENAI_BASE_URL")).unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
    }
}
```
(`printenv VAR` exits non-zero when the var is missing, so SUCCESS proves the env reached the child. `printenv` exists on the Linux dev/CI machines this repo targets; the two env tests print the URL to test stdout — a URL, not traffic content.)

- [ ] **Step 2: Run to verify they fail, then pass**

Run: `cargo test --bin harnesswurm`
Expected first: FAIL with "no function or associated item named `run` accessible" or test module missing → then PASS (7 tests) after adding the module. (`run` is a private fn in the same file, so `use super::*` reaches it.)

- [ ] **Step 3: Verify env-var support in each agent's docs before documenting**

Confirm from current docs which base-URL env vars each agent honors (Claude Code: `ANTHROPIC_BASE_URL` — long documented; opencode: check opencode.ai/docs for its Anthropic/OpenAI provider endpoint overrides). Only document vars you verified; report unverified ones as concerns instead of writing them down.

- [ ] **Step 4: Add recipes (bin doc-comment + README)**

Extend the `//!` doc-comment examples in `bin/harnesswurm.rs`:
```text
//! harnesswurm run --experiment issue-1284 --agent opencode -- opencode run "Fix the login redirect"
//! harnesswurm run --experiment issue-1284 --agent claude-code -- claude -p "Fix the login redirect"
```
(Adjust the subcommands to what Step 3 verified; never invent flags.)

In `harnesswurm/README.md`, find the existing `harnesswurm run` section and append a `### Zero-setup recipes` subsection with the same two commands plus the one-line explanation that attribution travels in the base URL so no headers are needed. Keep the file's existing markdown style.

- [ ] **Step 5: Verify + commit**

Run: `cargo test`
Expected: 180+ PASS (173 + 7 new), 0 fail

```bash
git add harnesswurm/backend/src/bin/harnesswurm.rs harnesswurm/README.md
git commit -m "Test wrapper parsing and document agent recipes" -m "Flag errors and env passing covered; opencode and claude-code recipes verified against <doc URLs>."
```

## Deferred (explicitly not in this plan)

- Cursor/Aider/Kilo/Codex fingerprints (next connectivity slice; same Task-1 method applies).
- Hot-edit for non-provider yamls (restart-to-apply stays).
- `/v1/models` + `count_tokens` recording (forwarded-not-recorded by design).
