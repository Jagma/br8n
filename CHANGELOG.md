# Changelog

## 0.1.0 — 2026-09-27

The first public release of br8n: local, semantic search over your notes,
papers and past agent sessions, injected into your coding agent's prompts.

- **Search your own knowledge base.** Indexes Markdown, PDFs (with OCR for
  scanned pages), web pages and your past Claude Code and Codex sessions into
  a local vector, keyword and graph store. Re-indexing is incremental, so
  only changed files are read again.
- **Automatic context and on-demand search.** A prompt hook injects the most
  relevant passages when they clear a confidence gate, and an MCP tool lets
  the agent search whenever it decides to. Five quality tiers trade latency
  for recall.
- **Works with your agents.** `br8n connect` sets up Claude Code, Codex,
  Gemini CLI, Cursor and Claude Desktop, and `br8n disconnect` removes only
  what br8n added.
- **Memory.** Lessons and facts that carry across sessions, added by you or
  distilled from finished sessions, shown at the start of each session.
- **Dashboard.** `br8n dashboard` gives a knowledge graph, a search view that
  shows exactly what was injected and why, a health view with fixes for any
  problem, first-run setup, and settings for everything in the config file.
- **Local by default.** Embeddings run through Ollama on your machine, or
  through any OpenAI-compatible endpoint you point it at.
- **Encrypted backups** of your config and index to your own S3 bucket or
  Google Drive, on a schedule if you like.
- **Installs and updates itself.** `br8n install`, `br8n update` and
  `br8n uninstall`, plus a one-line installer.

Release builds are for macOS on Apple silicon and Linux on x86_64 (glibc 2.39
or newer). On Windows, use the Linux build inside WSL; native Windows support
is planned. Other platforms can build from source.
