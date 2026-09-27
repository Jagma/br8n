---
description: Search your knowledge base and show the matching excerpts
argument-hint: <query>
allowed-tools: Bash(br8n:*)
---

Run `br8n search "$ARGUMENTS" --quality 3` and present the results grouped by
source document. For each result show the title, heading path, and a short
excerpt, and keep the source uri so the user can open it.

If there are no results, say so plainly and suggest `/br8n-index` in case the
knowledge base has not been indexed yet.
