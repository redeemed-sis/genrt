# Repository agent assets

This directory contains reusable, checked-in procedures for humans and agents:

- `standards/` defines commit and rustdoc contracts;
- `skills/` defines discoverable task workflows.

Mandatory constraints remain in root and nested `AGENTS.md`. Skills are loaded
only when their description matches a task or a user invokes them explicitly.
Project-scoped custom agent roles live separately in `.codex/agents/`.

## Model allocation

Each subagent pins its model so changing the main session model does not
silently change the cost and reasoning profile of every delegated task.
The main orchestrator uses the model selected for the session; the project
configuration does not override that choice.

| Role | Model | Reasoning | Purpose |
| --- | --- | --- | --- |
| `architect` | `gpt-6.1-sol` | `high` | Cross-layer ownership and architecture analysis |
| `reviewer` | `gpt-6.1-sol` | `high` | Independent correctness and invariant review |
| `developer` | `gpt-6.1-sol` | `high` | Focused implementation after discovery |
| `tester` | `gpt-6.1-sol` | `medium` | Verification, regression checks, and artifact diagnosis |
| `explorer` | `gpt-6-luna` | `medium` | Bounded source and caller discovery |

Sol 6.1 is used for architecture, review, implementation, and verification.
Luna is used for bounded source and caller discovery.
Model selection does not alter sandbox permissions, the single-writer rule,
or the requirement to substantiate findings with source and test evidence.

The model identifiers and reasoning settings follow the official
[Sol 6.1](https://developers.openai.com/api/docs/models/gpt-6.1-sol) and
[Luna](https://developers.openai.com/api/docs/models/gpt-6-luna) documentation.
Model access depends on the Codex account and client used to start the agents.
