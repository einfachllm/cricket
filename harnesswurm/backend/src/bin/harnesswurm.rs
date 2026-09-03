//! `harnesswurm run` — point a real coding agent at the proxy for one
//! experiment, without asking the agent to send any header it can't send.
//!
//! The attribution (agent, experiment, session) is carried in the base URL
//! itself — the one knob every agent exposes — and this wrapper is what
//! puts it there:
//!
//! ```text
//! harnesswurm run --experiment issue-1284 --agent kilo -- kilo code
//! harnesswurm run --experiment issue-1284 --agent claude-code -- claude
//! harnesswurm run --experiment issue-1284 --agent opencode -- opencode run "Fix the login redirect"
//! harnesswurm run --experiment issue-1284 --agent claude-code -- claude -p "Fix the login redirect"
//! ```
//!
//! Both agents then appear as runs of the same experiment, ready to compare
//! in Analytics.

use harnesswurm_backend::build_run_prefix;
use std::process::{Command, ExitCode};

const USAGE: &str = "usage: harnesswurm run [FLAGS] -- <command> [args...]
  --agent <name>        name the agent's calls are recorded under (required)
  --experiment <id>     group this run with others on the same task
  --session <id>        override the generated session id
  --provider <name>     send to this providers.yaml entry instead of the default
  --addr <host:port>    proxy address (default: $HARNESSWURM_ADDR or 127.0.0.1:8081)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(args) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("harnesswurm: {message}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run(args: Vec<String>) -> Result<ExitCode, String> {
    let mut rest = args;
    if rest.first().map(String::as_str) != Some("run") {
        return Err("expected the 'run' subcommand".to_string());
    }
    rest.remove(0);

    let split = rest
        .iter()
        .position(|arg| arg == "--")
        .ok_or_else(|| "no '--' separating the flags from the command to run".to_string())?;
    let (flags, command) = rest.split_at(split);
    let command = &command[1..];
    let program = command
        .first()
        .ok_or_else(|| "no command after '--'".to_string())?;

    let mut agent: Option<String> = None;
    let mut experiment: Option<String> = None;
    let mut session: Option<String> = None;
    let mut provider: Option<String> = None;
    let mut addr =
        std::env::var("HARNESSWURM_ADDR").unwrap_or_else(|_| "127.0.0.1:8081".to_string());

    let mut i = 0;
    while i < flags.len() {
        let flag = flags[i].as_str();
        let value = flags
            .get(i + 1)
            .ok_or_else(|| format!("flag '{flag}' needs a value"))?
            .clone();
        match flag {
            "--agent" => agent = Some(value),
            "--experiment" => experiment = Some(value),
            "--session" => session = Some(value),
            "--provider" => provider = Some(value),
            "--addr" => addr = value,
            other => return Err(format!("unknown flag '{other}'")),
        }
        i += 2;
    }

    let agent = agent.ok_or_else(|| "an agent name is required: --agent <name>".to_string())?;
    let session = session.unwrap_or_else(|| {
        // Unique per invocation, but named so a row in the comparison reads
        // as "the kilo attempt of issue-1284" rather than as hex noise.
        let slug = experiment.as_deref().unwrap_or("run");
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        format!(
            "{slug}-{agent}-{started:x}-{:x}",
            std::process::id() & 0xffff
        )
    });

    let prefix = build_run_prefix(&agent, experiment.as_deref(), &session, provider.as_deref())?;
    let anthropic_base = format!("http://{addr}{prefix}");
    let openai_base = format!("http://{addr}{prefix}/v1");

    println!("agent      {agent}");
    if let Some(experiment) = &experiment {
        println!("experiment {experiment}");
    }
    println!("session    {session}");
    if let Some(provider) = &provider {
        println!("provider   {provider}");
    }
    println!("ANTHROPIC_BASE_URL  {anthropic_base}");
    println!("OPENAI_BASE_URL     {openai_base}");
    println!("running: {}", command.join(" "));

    let status = Command::new(program)
        .args(&command[1..])
        .env("ANTHROPIC_BASE_URL", &anthropic_base)
        .env("OPENAI_BASE_URL", &openai_base)
        .status()
        .map_err(|e| format!("could not start '{program}': {e}"))?;

    Ok(match status.code() {
        Some(code) => ExitCode::from(code as u8),
        // Killed by a signal rather than exited: report failure rather than
        // success, and let the human read the 137-style detail from the
        // shell, which saw the same wait status.
        None => ExitCode::FAILURE,
    })
}

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
