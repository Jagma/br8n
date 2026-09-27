---
name: br8n-memory
description: Use when the user corrects you ("no, don't…", "always…", "never…", "we do X here"), states a durable fact about themselves or their setup, says "remember this" or "forget that", or when a substantial piece of work ends with a decision worth keeping. Saves and retracts memories that reach future sessions.
---

# Remembering across sessions

`br8n_remember` saves a memory; `br8n_forget` retracts one by id. Memories are
retrieved by relevance on later prompts (they appear inside `<br8n-context>` with a
`(memory: kind, date, scope, id …)` line) and lessons are additionally shown at the
start of every session in a `<br8n-lessons>` block for the matching project.

## When to save

- **lesson**: the user corrected you or stated a rule about how they want work done.
  One imperative sentence, their intent verbatim: "Never comment code unless asked."
  Use `confidence: 100` only when they said it explicitly.
- **fact**: something durable about the user or their environment that you would
  otherwise have to be told again: where a vault lives, which package manager a repo
  uses, who owns a service.
- **episode**: at the end of substantial work, what was decided and why, in two or
  three sentences. br8n also distills episodes from finished sessions on its own, so
  only save one when the decision is worth stating precisely.

Do not save transient task state, anything already in the codebase, or a guess. If you
are not sure the user meant it as a standing rule, ask before saving.

## Scope

Omit `scope` for a rule about this repository. Pass `scope: "global"` only when the rule
clearly applies everywhere ("always squash before opening a PR").

## Retracting

When the user says a lesson no longer applies, call `br8n_forget` with the id shown in
`<br8n-lessons>` or in the `(memory: …)` line, then confirm what was removed.
