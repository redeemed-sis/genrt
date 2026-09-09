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
| `architect` | `gpt-6-astra` | `high` | Cross-layer ownership and architecture analysis |
| `reviewer` | `gpt-6-astra` | `high` | Independent correctness and invariant review |
| `developer` | `gpt-5.6-terra` | `high` | Focused implementation after discovery |
| `tester` | `gpt-5.6-terra` | `medium` | Verification, regression checks, and artifact diagnosis |
| `explorer` | `gpt-5.6-luna` | `medium` | Bounded source and caller discovery |

Astra is reserved for architecture and review judgments. Routine discovery,
implementation, and verification retain the lighter model assignments.
Model selection does not alter sandbox permissions, the single-writer rule,
or the requirement to substantiate findings with source and test evidence.

The Astra identifier and reasoning setting follow the
[official model documentation](https://developers.openai.com/api/docs/models/gpt-6-astra).
Model access depends on the Codex account and client used to start the agents.
