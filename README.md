# shellshelf

`shellshelf` is a CLI, localhost web interface, and bundled Codex skill for storing, searching, and sharing reusable shell commands across personal shelves and team repositories, with optional private-repo sync for your own shelves across machines and a web workbench for running stored `curl` commands and inspecting responses.

The name is meant to evoke a shelf of reusable shell commands without borrowing software-library terminology.

## Installation

### MacOS
Install with Homebrew:

```bash
brew install vcali/tap/shellshelf
```

### Linux
On Linux, install the published release binary tarball:

```bash
VERSION="v0.2.8" # replace with the release you want
curl -LO "https://github.com/vcali/shellshelf/releases/download/${VERSION}/shellshelf-x86_64-unknown-linux-gnu.tar.gz"
curl -LO "https://github.com/vcali/shellshelf/releases/download/${VERSION}/shellshelf-x86_64-unknown-linux-gnu.tar.gz.sha256"
sha256sum --check shellshelf-x86_64-unknown-linux-gnu.tar.gz.sha256
tar -xzf shellshelf-x86_64-unknown-linux-gnu.tar.gz
install -Dm755 shellshelf ~/.local/bin/shellshelf
```

Make sure `~/.local/bin` is on your `PATH`.

## Quickstart

###
Default config lives at `~/.shellshelf/config.json`.

Search within a shelf or across shelves:

```bash
> $ shellshelf csv
=== LOCAL / AWK ===

[1] Print csv files with awk
awk -F ',' '{print $2}' <filename>
```

Connect a shared GitHub-backed repo once:

```bash
shellshelf --add-repo https://github.com/acme/shared-shellshelf.git
```

Connect a private GitHub-backed personal repo for your shelves:

```bash
shellshelf --add-personal-repo https://github.com/acme/private-shellshelf.git
```

`--add-personal-repo` now bootstraps non-interactively by default: it seeds an empty personal repo from local shelves, imports from the repo when local shelves are empty, and merges matching shelf files command-by-command when both sides already have data.

Search shared team shelves:

```bash
shellshelf --repo /path/to/shared-shellshelf --team platform -s curl webhook
shellshelf --repo /path/to/shared-shellshelf --all-teams -s curl stripe webhook
```

When a shared repo is configured, unscoped reads default to local shelves plus all team shelves:

```bash
shellshelf --list-shelves
shellshelf httpbin
```

Start the localhost web interface:

```bash
shellshelf --web
shellshelf --web --web-port 4920
```

## Highlights

- Search by keywords and shelf names instead of exact text only
- Keep personal shelves local while browsing shared team shelves from the same tool
- Sync your personal shelves through a private repository without mixing them into shared team reads
- Open pull requests for shared-shelf writes without leaving the CLI
- Launch a localhost web interface with a tree explorer and editable request workbench
- Run stored `curl` commands in the web UI with inline text, image, and video previews
- Ship a bundled Codex skill so agents can search shelves before reinventing commands
- Import exported Postman collections into shelves when you need a starting point
- Avoid shell-history dumping; commands stay intentionally curated

## Web Interface

The web interface:

- browses local shelves and any configured shared repository shelves
- uses a Postman-like tree explorer for local shelves and shared team shelves
- loads stored commands into an editable workbench with a description field
- can create shelves and save new or edited commands back into the selected shelf
- runs `curl` commands only, never arbitrary shell commands
- shows request headers alongside response headers for executed `curl` requests
- previews text, images, animated images, and video responses inline when the response content type supports it
- leaves non-`curl` commands browseable and editable, but disabled for execution
- defaults to a Dracula-inspired theme, with optional `solarized-dark`, `solarized-light`, and `giphy` themes configurable in `config.json`

Web config example:

```json
{
  "web": {
    "port": 4920,
    "theme": "dracula"
  }
}
```

## More CLI Basics

Add a command locally:

```bash
shellshelf -s curl -a "curl -I https://api.github.com/users/octocat"
```

If `personal_repo` is configured, local CLI and web writes also sync the changed shelf into that personal repository and push it directly.

Add a command with a short description:

```bash
shellshelf -s git -a "git log --oneline --graph -20" \
  --description "Compact recent history graph"
```

List a shelf:

```bash
shellshelf -s curl -l
```

List available shelves:

```bash
shellshelf --list-shelves
shellshelf --repo /path/to/shared-shellshelf --team platform --list-shelves
shellshelf --repo /path/to/shared-shellshelf --all-teams --list-shelves
```

If `default_shelf` is configured, you can omit `-s` for normal reads and writes. If it is not configured, `shellshelf` falls back to the built-in `default` shelf.

Create a shelf explicitly:

```bash
shellshelf --create-shelf git
shellshelf --repo /path/to/shared-shellshelf --team platform --create-shelf aws
```

Seed a configured personal repo from your existing local shelves:

```bash
shellshelf --sync-personal
```

Import an exported Postman collection into a new shelf:

```bash
shellshelf --import-postman ./postman-api.json
shellshelf --target-shelf postman-api-v2 --import-postman ./postman-api.json
shellshelf --repo /path/to/shared-shellshelf --team platform --import-postman ./platform-api.json
```

## Team Usage

Shared storage keeps ownership at the team level and organization at the shelf level:

```text
shared-shellshelf/
  teams/
    platform/
      shelves/
        curl.json
        aws.json
    payments/
      shelves/
        curl.json
```

Basic team-scoped usage:

```bash
shellshelf --repo /path/to/shared-shellshelf --team platform -s curl -a \
  "curl https://api.example.com/platform/health"

shellshelf --repo /path/to/shared-shellshelf --team platform -s curl -l
```

Publish a shared write as a pull request:

```bash
shellshelf --repo /path/to/shared-shellshelf --team platform -s curl -a \
  "curl https://api.example.com/platform/health" \
  --open-pr

shellshelf --repo /path/to/shared-shellshelf --team platform --create-shelf aws \
  --open-pr --base-branch main --pr-branch feat/platform-aws
```

`--open-pr` is available on shared `--add`, `--create-shelf`, and `--import-postman` writes. It checks that the shared checkout is clean, fetches and rebases onto the base branch, creates or reuses a publish branch, commits the changed shelf file, pushes it, and opens a pull request with `gh`.

Cross-team search within one shelf:

```bash
shellshelf --repo /path/to/shared-shellshelf --all-teams -s curl stripe webhook
```

Default local-plus-team output is grouped by source:

```text
=== LOCAL / CURL ===

[1] Fetch Octocat profile
curl https://api.github.com/users/octocat

=== SHARED / PLATFORM / CURL ===

[1] Platform health check
curl -X POST https://api.example.com/platform/health \
  -H "Authorization: Bearer $TOKEN"
```

## Config

Default config location:

```text
~/.shellshelf/config.json
```

Minimal GitHub-backed config:

```json
{
  "default_shelf": "curl",
  "shared_repo": {
    "mode": "github",
    "github_repo": "acme/shared-shellshelf",
    "teams_dir": "teams",
    "default_team": "platform",
    "auto_update_repo": true,
    "auto_update_interval_minutes": 15
  },
  "default_list_limit": 20
}
```

GitHub-backed shared usage requires:

- `gh` installed and authenticated
- `git` available locally
- a clean shared checkout before `--open-pr`

You can write the `shared_repo.mode = "github"` config for that automatically with:

```bash
shellshelf --add-repo https://github.com/acme/shared-shellshelf.git
shellshelf --add-repo acme/shared-shellshelf
```

Once a shared repo is configured, default read commands include local shelves plus all teams unless you explicitly narrow with `--team`, `--all-teams`, `--local-only`, `--shared-only`, or `shared_repo.default_team`.

For personal repos, you can override the default add-time bootstrap behavior with:

```bash
shellshelf --add-personal-repo acme/private-shellshelf --personal-repo-bootstrap skip
shellshelf --add-personal-repo acme/private-shellshelf --personal-repo-bootstrap merge
shellshelf --add-personal-repo acme/private-shellshelf --personal-repo-bootstrap push
shellshelf --add-personal-repo acme/private-shellshelf --personal-repo-bootstrap pull
```

If the managed personal checkout later drifts ahead of, behind, or diverges from `origin`, `shellshelf` prints warning lines with ready-to-run `git` commands for push/pull and an `Inspect:` command that shows the commit and diff overview. Those remote-status checks re-fetch at most once per `personal_repo.sync_check_interval_minutes` and default to 30 minutes.

## Documentation

- Detailed CLI and config reference: [`docs/reference.md`](docs/reference.md)
- Technical overview and code structure: [`docs/technical-overview.md`](docs/technical-overview.md)

## Releases

Create and push a semver tag to publish a GitHub Release automatically.

- Tags use the format `v<crate-version>` and must match `Cargo.toml`
- Commit the matching `Cargo.lock` update before tagging so `cargo build --locked` succeeds in release CI
- The Homebrew tap is `vcali/tap`
- Automatic tap updates require the `HOMEBREW_TAP_TOKEN` repository secret
- The GitHub release is created before the tap update so published formula URLs are live immediately
- Release assets currently include:
  - `shellshelf.rb` for tap publication and manual formula use
  - `shellshelf-x86_64-unknown-linux-gnu.tar.gz`
  - `shellshelf-x86_64-apple-darwin.tar.gz`
  - `shellshelf-aarch64-apple-darwin.tar.gz`
  - matching `.sha256` checksum files

## Sensitive Data

`shellshelf` stores commands as provided. If a command contains live tokens, cookies, or other credentials, shared repository mode can expose them to teammates or commit history. Secret detection and redaction are still planned, not implemented.

Postman import has the same caveat. Imported headers or raw bodies are stored as-is.
Supported body modes currently include raw bodies and common multipart form-data requests.

The web interface keeps response bodies in-memory for the active process so it can render previews. It does not persist response payloads back into shelf storage.

## Development

Build:

```bash
cargo build
```

Run locally during development:

```bash
cargo run -- -s curl -l
cargo run -- --web
```
