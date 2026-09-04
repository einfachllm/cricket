## What this changes

<!-- One or two sentences. What is different after this PR? -->

## Why

<!-- The problem being solved. Link the issue: Fixes #NN -->

## How it was verified

<!-- Say what you actually ran, not what should pass. -->

- [ ] `cargo test --locked` (in `harnesswurm/backend`)
- [ ] `python3 scripts/clippy_baseline.py` — no new clippy warnings
- [ ] `npm run typecheck` (in `desktop`)
- [ ] `npm test` (in `desktop`)
- [ ] Checked by hand in the app:

<!-- Describe the manual check, or say why none was needed. -->

## Notes for the reviewer

<!--
Anything that isn't obvious from the diff: a decision you weren't sure about,
a behaviour change, a follow-up you deliberately left out.

If this touches attribution, routing, the DB schema or a yaml format, say what
happens to an existing install on upgrade.
-->
