# Repository agent assets

This directory contains reusable, checked-in procedures for humans and agents:

- `standards/` defines commit and rustdoc contracts;
- `skills/` defines discoverable task workflows.

Mandatory constraints remain in root and nested `AGENTS.md`. Skills are loaded
only when their description matches a task or a user invokes them explicitly.
Project-scoped custom agent roles live separately in `.codex/agents/`.
