# Security Policy

## Supported versions

Harnesswurm is pre-1.0 and has no tagged releases yet. Only the current `main`
branch is supported — fixes land there, and there are no backports.

## Reporting a vulnerability

**Please do not open a public issue for a security problem.**

Report it privately through GitHub:

1. Go to the [Security tab](https://github.com/einfachllm/harnesswurm/security/advisories/new)
   of this repository.
2. Click **Report a vulnerability** and describe the issue.

Please include what you can of the following — it makes triage much faster:

- What an attacker can do, and what access they need to do it.
- Steps to reproduce, or a proof of concept.
- The commit you tested, your OS, and how you were running the app (desktop
  build or standalone backend).

You can expect an acknowledgement within about a week. We'll let you know
whether the report is accepted, and tell you when a fix has landed. If you'd
like credit in the fix's release notes, say so and name how you want to be
credited.

Please give us a reasonable window to ship a fix before disclosing publicly.

## Threat model, and what is out of scope

Harnesswurm is a **local developer tool**. It binds to `127.0.0.1:8081` by
default and assumes the machine it runs on is trusted. It has no
authentication, no authorization and no multi-tenancy, by design.

That means the following are known properties rather than vulnerabilities:

- **The proxy is unauthenticated.** Anything that can reach the bind address
  can read the recorded traffic through the HTTP API. Do not bind it to a
  public interface (`BIND_ADDR`); if you must reach it remotely, put it behind
  something that does the authenticating.
- **Prompt and response bodies are stored on disk** in the SQLite database, so
  you can inspect what was sent. They age out after 30 days
  (`HARNESSWURM_TRAFFIC_RETENTION_DAYS`; `0` keeps them forever). The database
  file is not encrypted — protect it with filesystem permissions.
- **API keys are forwarded upstream, not stored.** Request *headers* are not
  written to the database; only bodies are. A report showing a credential
  reaching the database, the logs, or any other persistent sink *is* in scope
  and we want to hear about it.
- **Providers you configure are trusted.** `providers.yaml` points the proxy
  wherever you tell it to, including plain HTTP to a local model server. It is
  configuration, not an untrusted input.

Things that **are** in scope: credential leaks into logs or the database,
anything a *proxied response body* can make the app do (the captured traffic is
attacker-influenced data and must never be treated as code), path handling in
the `/r/…` and `/p/…` prefixes that reaches outside the intended route,
requests that get forwarded to a provider other than the one addressed, and SQL
injection through any recorded field.
