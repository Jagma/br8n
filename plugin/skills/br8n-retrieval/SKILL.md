---
name: br8n-retrieval
description: Use when the user refers to something they have previously written, read, saved, or worked on — "my notes on X", "that paper about Y", "how did I solve this last time", "what did we decide" — or when a question depends on their personal context rather than general knowledge. Also use before saying you lack context about the user's own past work.
---

# Searching the user's knowledge base

The `br8n` plugin indexes the user's notes, papers, saved articles, and past
Claude Code sessions into a local vector-graph database.

## When to search

Search when the answer depends on something *this user* wrote or read:

- "my notes on X", "that article I saved", "the paper about Y"
- "how did I solve this before", "what did we decide about Z"
- Any question where their own prior decisions are the answer

Do **not** search for general knowledge, for facts already in the conversation,
or for things about the current codebase — normal file tools are better for code.

## How to search

Call `br8n_search` with a natural-language query. Prefer the user's own phrasing
over keywords; the index is semantic and heading-aware, so full questions work
better than single terms.

`quality` controls depth, 0 to 4. Omit it for the default. Raise it to 4 and
search again only when a first pass returns thin or off-target results.

## Following the graph

Every result carries a `uri`. Passing that uri to `br8n_related` returns notes
linked to it — wikilinks the user wrote, and adjacent sections of the same
document. Use it when a result is clearly near the answer without containing it;
this is often how you find the note the user actually meant.

## Reading results

Results are excerpts, not whole documents. When an excerpt looks central to the
answer, read the full file at its `uri` rather than guessing at the surrounding
context.

Cite the source title when you use a result, so the user can tell what came from
their own notes and what came from you.

## Context that appears without you asking

A `<br8n-context>` block may be prepended to a user prompt automatically. It is
retrieved, not user-written — treat it as reference material, and if it is
irrelevant to what they actually asked, ignore it rather than working it in.
