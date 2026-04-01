---
title: feat: Include shelf names in search matching
type: feat
status: completed
date: 2026-04-01
---

# feat: Include shelf names in search matching

## Overview

Extend search so shelf metadata participates in match eligibility. A query such as `shellshelf media upload` should be able to find an `upload` command stored under shelf `media` even when the command text and description do not themselves contain `media`.

## Problem Frame

Current search only matches against per-command keywords, raw command text, and optional description text. Shelf names are used for routing and output headers, but not for matching, so cross-shelf searches miss results that users expect when they naturally include the shelf name in the query.

## Requirements Trace

- R1. Shelf names must be considered during search matching so `shellshelf media upload` can find commands stored in shelf `media`.
- R2. Existing AND semantics must be preserved: each query term must match the same result through command content, description, or shelf metadata.
- R3. Search scope rules must remain unchanged for local, `--shared-only`, `--local-only`, `--team`, and `--all-teams` reads.
- R4. Output grouping, section headers, and local/shared duplicate hiding must remain unchanged.
- R5. User-facing and maintainer-facing docs must describe the new search semantics and examples.
- R6. The implementation must ship with unit and integration coverage, plus the normal Rust validation suite.

## Scope Boundaries

- No ranking or relevance-order changes.
- No on-disk JSON schema or migration work.
- No changes to shelf creation, shelf naming rules, or shared repository write flows.
- No expansion of search scope beyond the shelves already selected by existing CLI/config rules.

## Context & Research

### Relevant Code and Patterns

- `src/cli.rs` defines positional search keywords and the existing CLI contract.
- `src/app.rs` owns search routing, cross-shelf traversal, output section construction, and local/shared duplicate hiding.
- `src/config.rs` loads local/team/all-team command sets and currently delegates search matching to `CommandDatabase::search`.
- `src/database.rs` stores per-command keywords and implements current AND-based matching.
- `tests/integration_tests.rs` already covers shelf-scoped search, local all-shelf search, and team all-shelf search, making it the right place for CLI behavior regressions.

### Institutional Learnings

- There is no `docs/solutions/` knowledge base in this repo, so the practical constraints come from the current code, docs, and tests.
- Cross-shelf search is already a documented contract; shelf-name-aware matching needs to work across the same local/shared read modes rather than only in one code path.

### External References

- None. The repo already has strong local patterns for CLI search behavior, storage layout, and docs structure.

## Key Technical Decisions

- Pass shelf context into search matching instead of changing persisted command records. This keeps the JSON format stable and limits the blast radius to the search path.
- Keep shelf names as another match surface under the existing AND semantics. A query term may be satisfied by shelf metadata or by command/description content, but all terms must still resolve against the same candidate result.
- Apply the same shelf-aware behavior anywhere the code already knows the shelf name, including active-shelf searches and all-shelf traversal. This avoids special cases between `-s <shelf> ...` and shelf-discovery search paths.
- Preserve current output order, grouping, and duplicate hiding. This work should change eligibility, not presentation.
- Normalize shelf names using the same lowercasing/token-style matching expectations users already get from command search, while still covering separator-heavy shelf names such as `media-tools`, `media_tools`, and `media.api`.

## Open Questions

### Resolved During Planning

- Should shelf-name matching require a storage migration? No. Shelf names are already known at load time, so the safer design is contextual matching in the read path.
- Should a shelf-only query like `shellshelf media` return commands from shelf `media`? Yes. Once shelf names are part of the searchable surface, that becomes the expected AND-preserving behavior.
- Should this change alter ranking or flatten grouped output? No. Existing deterministic sectioned output remains the contract.

### Deferred to Implementation

- Exact helper names and whether the shared search context is represented as a small struct, a helper function, or an optional argument extension on the current search API.
- Whether the final normalization helper should live in `src/database.rs` or be shared from the keyword extraction layer after touching the concrete code.

## High-Level Technical Design

> *This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce.*

```text
query keywords
  -> resolve selected shelves using existing local/shared scope rules
  -> for each shelf:
       derive searchable shelf context from the shelf name
       evaluate each command with:
         every query term matches command keywords/text/description
         OR the shared shelf context for that same shelf
       if any commands match:
         render them in the existing section for that shelf
  -> run existing local/shared duplicate hiding
  -> print existing grouped output
```

## Implementation Units

- [x] **Unit 1: Extend matching to accept shelf context**

**Goal:** Make the core search predicate able to satisfy query terms from shelf metadata as well as command-local data.

**Requirements:** R1, R2

**Dependencies:** None

**Files:**
- Modify: `src/database.rs`
- Modify: `src/keywords.rs`
- Test: `src/database.rs`

**Approach:**
- Extend the current search helper so callers can provide per-shelf search context without mutating stored command data.
- Reuse existing keyword/token extraction expectations for shelf names rather than inventing a separate matching model.
- Preserve current partial-match and case-insensitive behavior.

**Patterns to follow:**
- Existing AND-based matching in `src/database.rs`
- Existing token extraction helpers in `src/keywords.rs`

**Test scenarios:**
- Happy path: a command under shelf `media` matches `media upload` when only `upload` exists in the command content.
- Happy path: a shelf-only query such as `media` returns commands from shelf `media`.
- Edge case: separator-heavy shelf names such as `media-tools`, `media_tools`, and `media.api` match queries for both the full shelf string and component terms like `media` and `tools`.
- Edge case: case-insensitive queries still match mixed-case shelf names.
- Error path: unrelated shelf names do not cause false positives when the command content does not satisfy the remaining terms.

**Verification:**
- The core matching layer can prove shelf-aware AND semantics without any persistence-format change.

- [x] **Unit 2: Thread shelf-aware matching through local and shared search flows**

**Goal:** Apply the new shelf-aware predicate across active-shelf and cross-shelf search flows without changing scope or presentation.

**Requirements:** R1, R2, R3, R4

**Dependencies:** Unit 1

**Files:**
- Modify: `src/app.rs`
- Modify: `src/config.rs`
- Test: `tests/integration_tests.rs`

**Approach:**
- Update local, team, default shared, and all-team loaders to pass the current shelf name into the search predicate.
- Keep the existing scope-selection logic intact; this unit should only change how a command qualifies once a shelf is already being searched.
- Preserve empty-section suppression, section headers, and post-filter duplicate hiding.

**Patterns to follow:**
- Existing cross-shelf local loading in `src/app.rs`
- Existing shared scope loaders in `src/config.rs`
- Existing duplicate hiding in `src/app.rs`

**Test scenarios:**
- Happy path: `shellshelf media upload` returns only the `media` shelf section when the matching command text only contains `upload`.
- Happy path: `shellshelf --repo <repo> --team platform media` returns every command in the `media` shelf for that team.
- Happy path: `shellshelf --repo <repo> --all-teams media` returns grouped shared sections only for matching shelves across teams.
- Happy path: `shellshelf -s media upload` still finds commands in the active shelf, and `shellshelf -s media media upload` also succeeds through shelf-aware matching.
- Edge case: `--local-only`, `--shared-only`, `--team`, and `--all-teams` preserve current scope boundaries.
- Edge case: empty shelves remain suppressed from output when no commands match.
- Integration: local/shared duplicate hiding still removes the local copy when both results matched because of the same shelf name.

**Verification:**
- CLI searches behave the same as today except that shelf names now participate in match eligibility across the existing scopes.

- [x] **Unit 3: Add regression coverage for the new search contract**

**Goal:** Lock the new behavior down with focused unit and end-to-end tests.

**Requirements:** R1, R2, R3, R4, R6

**Dependencies:** Unit 2

**Files:**
- Modify: `src/database.rs`
- Modify: `tests/integration_tests.rs`
- Test: `src/database.rs`
- Test: `tests/integration_tests.rs`

**Approach:**
- Add unit coverage for shelf-context matching combinations and separator normalization.
- Add integration coverage for local-only, team-scoped, default mixed local/shared, and all-team search flows.
- Include at least one regression that proves duplicate hiding still works when shelf matching is the only reason the local/shared copies were eligible.

**Execution note:** Start with failing tests for the new search contract before changing the production matching path.

**Patterns to follow:**
- Existing search integration tests in `tests/integration_tests.rs`
- Existing focused unit tests in `src/database.rs`

**Test scenarios:**
- Happy path: local all-shelf search by shelf name plus command term.
- Happy path: default combined local + shared search by shelf name plus command term.
- Happy path: team-scoped shared search by shelf name only.
- Happy path: all-team shared search by shelf name only with preserved section headers.
- Edge case: explicit `-s <shelf>` searches continue to behave correctly with and without repeating the shelf term in the query.
- Edge case: partial shelf tokens do not leak across unrelated shelves.
- Integration: identical local/shared commands still dedupe after matching via shelf context.

**Verification:**
- The test suite covers every documented search mode affected by the new contract.

- [x] **Unit 4: Update search documentation and delivery notes**

**Goal:** Document the new search behavior everywhere users and maintainers currently learn search semantics.

**Requirements:** R5, R6

**Dependencies:** Unit 2

**Files:**
- Modify: `src/cli.rs`
- Modify: `README.md`
- Modify: `docs/reference.md`
- Modify: `docs/technical-overview.md`
- Modify: `DEVELOPMENT_PLAN.md`

**Approach:**
- Update the CLI help text if needed so positional keyword search is described consistently with shelf-aware matching and all-shelf search behavior.
- Update the reference docs’ search behavior section and examples to explicitly say shelf names participate in matching.
- Update the technical overview so maintainers understand that shelf metadata is now part of the search context even though command storage remains unchanged.
- Update the README examples so the user-facing CLI story reflects the new expected query shape.
- Refresh the local-only development plan to record the feature as planned/completed at execution time.

**Patterns to follow:**
- Existing user-facing CLI examples in `README.md` and `docs/reference.md`
- Existing maintainers’ runtime notes in `docs/technical-overview.md`

**Test scenarios:**
- Test expectation: none -- documentation-only changes, but the examples and prose must align with the implemented search contract and validation outcomes.

**Verification:**
- Repo docs consistently describe shelf-aware search and point to examples that match the shipped behavior.

## System-Wide Impact

- **Interaction graph:** Search remains a CLI-only read path spanning `src/cli.rs`, `src/app.rs`, `src/config.rs`, and `src/database.rs`.
- **Error propagation:** No new external failure modes are expected; matching remains an in-memory filter over already-loaded JSON-backed records.
- **State lifecycle risks:** The change must not require rewriting existing shelf files or re-indexing persisted data.
- **API surface parity:** User-facing CLI docs, help text expectations, and examples must reflect the new search contract.
- **Integration coverage:** Cross-scope local/shared searches and duplicate hiding need explicit integration coverage because unit tests alone will not prove them.
- **Unchanged invariants:** Shelf selection rules, result grouping, deterministic output order, and shared duplicate hiding remain unchanged.

## Risks & Dependencies

- Main risk: introducing false positives or broadening scope accidentally by treating shelf metadata as a global OR instead of per-shelf context.
- Main mitigation: keep matching AND-based per candidate command and add integration tests across every search scope.
- Main dependency: the final implementation should preserve the existing JSON format and avoid touching unrelated Postman, create-shelf, or shared checkout flows.

## Validation Strategy

- All new behavior is covered by targeted unit and integration tests.
- The project validation suite passes with the completed change: tests, format check, clippy with warnings denied, and docs build.
