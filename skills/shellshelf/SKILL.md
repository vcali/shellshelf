---
name: shellshelf
description: Find, retrieve, render, and save reusable shell commands with shellshelf. Use when Codex needs a likely-existing curl, git, aws, kubectl, API, or operational command; should avoid re-deriving repeated commands; needs a parameterized command template; or should store a reusable command in a local or shared team shelf.
---

# Shellshelf

Use machine-readable output and bounded searches. Shellshelf returns command text but never executes it.

## Workflow

1. Search before inventing or adding:

```bash
shellshelf <service> <intent> --limit 3 --json
shellshelf --team platform <service> <intent> --limit 3 --json
```

Use `--local-only`, `--shared-only`, `--team`, or `--all-teams` only when scope matters. Add `-s <shelf>` only when the shelf is known.

2. Reuse a named result by its returned `ref`:

```bash
shellshelf --get local/curl/github-user --raw
shellshelf --get shared/platform/aws/tail-logs --json
```

3. Render a named template by supplying every reported parameter:

```bash
shellshelf --render local/curl/github-user --arg user=octocat --raw
```

Treat the rendered output as a proposed command. Inspect it and follow normal execution/approval rules; `shellshelf` does not run it.

4. If no suitable result exists, save the verified command with a stable name:

```bash
shellshelf -s curl --name github-user \
  --add 'curl "https://api.github.com/users/{{user}}"' \
  --description 'Fetch a GitHub user' --json

shellshelf --team platform -s aws --name tail-logs \
  --add 'aws logs tail {{log_group}} --since {{since}}' \
  --description 'Tail service logs' --open-pr --json
```

Use lowercase names containing letters, digits, dots, underscores, or hyphens. Prefer names for anything likely to be reused. An exact duplicate `--add` can attach a name to a legacy unnamed command.

## What to Store

Store a command when retrieving it later will save meaningful discovery or reconstruction work and it remains understandable without the current conversation.

- Store verified, non-obvious commands that are likely to recur and remain useful outside the current task.
- Use a template when only a few values vary between runs.
- Prefer a repository script, Make target, or task runner for multi-step logic, branching, or behavior tightly coupled to repository internals.
- Do not store trivial commands, one-off paths or IDs, temporary debugging commands, incomplete fragments, unsafe destructive commands, or commands containing live secrets.

## Guardrails

- Never store live tokens, cookies, passwords, or API keys. Keep references such as `$TOKEN` or use template parameters.
- Use `{{parameter}}` only on named commands. All parameters are required strings.
- Search first and avoid near-duplicates.
- Use shared `--open-pr` when a team command should be reviewed.
- Use `--list-shelves --json` only when search cannot identify the right shelf.
- Use human output only when presenting results to a person; prefer `--json` or `--raw` for agent work.
