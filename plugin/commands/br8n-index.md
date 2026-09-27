---
description: Index or re-index your knowledge base
allowed-tools: Bash(br8n:*)
---

Run `br8n index`. Report how many documents were added, updated, skipped, and
pruned, and how many chunks and graph edges resulted.

If the output mentions an embedding-model mismatch, explain that the index was
built with a different model and that `br8n index --reindex` rebuilds it.
