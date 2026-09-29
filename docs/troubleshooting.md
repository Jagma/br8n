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

**Claude Code says br8n is not installed.** The plugin is installed but the
`br8n` program is not, or the plugin can't find it. Install br8n with the
[install guide](install.md). The plugin looks for `br8n` on the PATH Claude
Code started with, then in br8n's own folder.

**br8n's context arrives twice, or `br8n status` says a second br8n plugin is
installed.** Claude Code has br8n's plugin from two places, for example from a
plugin directory and from `br8n install`, and both copies run the hooks. Run
`br8n install`: it keeps its own copy and removes the other.

**Everything returns empty and nothing seems to work.** Confirm Ollama is
running: `ollama list`. If it's not, `br8n`'s embed calls fail
silently by design — the hook is built to degrade to no injection rather than
error out mid-prompt — so this failure mode looks identical to "nothing
matched." Start Ollama and try again.
