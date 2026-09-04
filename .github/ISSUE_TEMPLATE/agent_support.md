---
name: Agent or model support
about: An agent isn't attributed, or a model isn't priced
title: ''
labels: enhancement
assignees: ''
---

## Which is it

- [ ] An agent shows up as `auto-<hash>` instead of by name (fingerprint missing)
- [ ] A model shows no cost estimate (pricing entry missing)
- [ ] The agent's calls don't reach Harnesswurm at all

## The agent or model

- Name and version:
- Which wire format it speaks: OpenAI-compatible / Anthropic-compatible / other
- How you pointed it at the proxy (env var, config field, `harnesswurm run`):

## What it actually sends

<!--
For a missing fingerprint: the `User-Agent` and the first line or two of the
system prompt, both visible in the Traffic tab. Redact anything project-specific.

For a missing price: the model id exactly as it appears, and a link to the
provider's pricing page.
-->

```
```
