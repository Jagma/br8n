# Troubleshooting


**No results.** The index may not exist yet, or may be empty. Run
`/br8n-index` (or `br8n index`) and check the reported document count.
`br8n status` also shows document and chunk counts directly.

**A file never turns up in search.** `br8n status` lists what the last index
skipped and why. A scanned PDF is skipped only when OCR could not run; the
message names the missing library and how to point `br8n` at it. The
background indexer's full output goes to a log beside the index.

**Results look stale, or `br8n status` reports a model mismatch.** The index
was built with a different embedding model than the one currently configured.
Embeddings from different models are not comparable, so `br8n` refuses to
mix them. Rebuild with `br8n index --reindex`.

**Everything returns empty and nothing seems to work.** Confirm Ollama is
running: `ollama list`. If it's not, `br8n`'s embed calls fail
silently by design — the hook is built to degrade to no injection rather than
error out mid-prompt — so this failure mode looks identical to "nothing
matched." Start Ollama and try again.
