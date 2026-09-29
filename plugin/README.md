# br8n for Claude Code

br8n gives Claude Code your own knowledge base: your notes, papers, saved web
pages and past Claude Code and Codex sessions, indexed and searched on your
own machine. Before each prompt it adds the most relevant passages, but only
when they clear a confidence gate, and Claude can search the knowledge base
itself through br8n's MCP tools.

## You also need the br8n program

This plugin connects br8n to Claude Code. The indexing and searching are done
by the `br8n` program, which you install separately:

```bash
curl -fsSL https://github.com/Jagma/br8n/releases/latest/download/install.sh | sh
```

The [install guide](https://github.com/Jagma/br8n/blob/main/docs/install.md)
lists what you need first (a Mac with Apple silicon or Linux on x86_64, and
[Ollama](https://ollama.com)) and other ways to install. Until the program is
installed, the plugin says so at the start of each session and otherwise does
nothing.

## What the plugin adds

- A prompt hook that adds relevant passages from your knowledge base to each
  prompt.
- A session-start hook that shows your br8n memories and refreshes the index
  in the background.
- MCP tools to search the knowledge base, follow its links, re-index it, and
  save and forget memories.
- Slash commands, among them `/br8n-search`, `/br8n-status` and `/br8n-index`.

## Your data stays on your machine

br8n embeds your files with a local model through Ollama and keeps its index
on your disk. It has no telemetry and sends your notes and prompts to no
service. It goes online only to check for a newer release, to fetch the OCR
model the first time it reads a scanned PDF, to fetch pages you add with
`br8n add <url>`, and to back up to your own S3 bucket or Google Drive if you
turn that on.

br8n is free software under the
[GNU AGPL v3.0](https://github.com/Jagma/br8n/blob/main/LICENSE).
