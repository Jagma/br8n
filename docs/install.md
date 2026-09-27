# Installing br8n

This takes about ten minutes, most of it the one-time model download.

`br8n` is a single program that installs itself. Everything it creates lives
in one folder: `~/Library/Application Support/br8n` on macOS, or
`~/.local/share/br8n` on Linux. You can remove all of it with
`br8n uninstall --purge`.

## Before you start

You need:

- **A supported computer.** A Mac with Apple silicon (M1 or newer), or Linux
  on x86_64 with glibc 2.39 or newer (Ubuntu 24.04 or newer). On Windows, use
  the Linux build inside [WSL](https://learn.microsoft.com/windows/wsl/install);
  native Windows support is planned. On an Intel Mac, Linux on arm64 or an
  older Linux, [build from source](#build-from-source).
- **Ollama, installed and running.** br8n uses it to understand your notes
  on your own machine.
  - On macOS, install the app from [ollama.com](https://ollama.com), or run
    `brew install ollama`, then open it.
  - On Linux, run `curl -fsSL https://ollama.com/install.sh | sh`.

  Run `ollama list` to check that it is running.
- **At least one coding agent.** Claude Code is the main one, and its
  `claude` command must be on your PATH. br8n can also connect to Codex,
  Gemini CLI, Cursor and Claude Desktop; see
  [Connect other agents](usage.md#connect-other-agents).

## Step 1: Download and install br8n

Pick one option.

### Option A: one-line installer (recommended)

```bash
curl -fsSL https://github.com/Jagma/br8n/releases/latest/download/install.sh | sh
```

This downloads the build for your machine, checks it against its checksum and
runs `br8n install`. For an unattended install, which answers yes to every
question, add `-s -- --yes` after `sh`.

### Option B: with the GitHub CLI

You need the [GitHub CLI](https://cli.github.com), logged in with `gh auth login`.

```bash
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)  target=aarch64-apple-darwin ;;
  Linux-x86_64)  target=x86_64-unknown-linux-gnu ;;
  *) echo "no release build for this machine; build from source"; target=none ;;
esac
mkdir -p ~/br8n-download && cd ~/br8n-download
gh release download --repo Jagma/br8n --pattern "br8n-$target.*"
shasum -a 256 -c "br8n-$target.sha256"
tar -xzf "br8n-$target.tar.gz"
./br8n install
```

This downloads the build for your machine, checks it against its checksum,
unpacks it and runs the installer. On a Mac this route shows no security
warning. `gh` doesn't mark files as downloaded from the internet, so
Gatekeeper never checks the program. The `shasum` line must print `OK`; if it
doesn't, stop and download again.

You can delete `~/br8n-download` afterwards, because `br8n install` copies
itself into its own folder.

### Option C: download in your browser

1. Open the [releases page](https://github.com/Jagma/br8n/releases) and,
   from the newest release, download the two files for your machine:
   `br8n-<target>.tar.gz` and `br8n-<target>.sha256`. `<target>` is:
   - `aarch64-apple-darwin` on a Mac;
   - `x86_64-unknown-linux-gnu` on Linux.
2. In a terminal, go to the folder you downloaded them to and check the
   download. It must print `OK`:

   ```bash
   shasum -a 256 -c br8n-<target>.sha256
   ```

3. Unpack it:

   ```bash
   tar -xzf br8n-<target>.tar.gz
   ```

4. **On a Mac only:** remove the "downloaded from the internet" mark your
   browser added. br8n is not signed by an Apple developer account, so
   without this step macOS refuses to run it and says it cannot verify it:

   ```bash
   xattr -d com.apple.quarantine br8n
   ```

   If it says "No such xattr", the file wasn't marked, and that's fine.

5. Install:

   ```bash
   ./br8n install
   ```

## Step 2: Answer the installer's questions

`br8n install` does the following:

1. Copies itself into br8n's folder and puts `br8n` on your PATH.
2. Writes a starter config file.
3. Registers the br8n plugin with Claude Code.
4. Asks before downloading the embedding model (`qwen3-embedding:0.6b`,
   about 600 MB). Say yes, because br8n can't work without it.
5. Lists the other coding agents it found (Codex, Gemini CLI, Cursor,
   Claude Desktop) and asks before connecting each one.

It prints what it did. Read the last lines:

- **An `export PATH=...` line:** your shell can't find `br8n` yet. Add that
  line to your shell profile (`~/.zshrc` on a Mac, usually `~/.bashrc` on
  Linux) and open a new terminal.
- **A warning:** run `br8n install` again. It repairs whatever it can and
  names what it can't.

`br8n install --yes` answers yes to every question, including connecting
every agent it found.

## Step 3: Tell br8n what to search

br8n searches the folders you choose. It also searches your past Claude Code
and Codex sessions, but a folder of notes is where it shines. Add yours in
whichever way you prefer:

- **In the dashboard (easiest):** run `br8n dashboard`. If nothing is
  indexed yet, it opens a three-step setup: pick your folders, connect your
  agents, then index. It ends on the Search page with a first search taken
  from your own notes. If sessions were already indexed, the setup doesn't
  appear; open **Settings › Sources**, add your folders, and click
  **Re-index now**.
- **From the terminal:**

  ```bash
  br8n config set sources '["~/notes", "~/Documents/papers"]'
  br8n index
  ```

  `br8n config check` confirms the folders exist.

The first index takes a few minutes for a few hundred notes. After that,
br8n re-reads only what changed, and every new Claude Code session starts a
quick background index on its own. So you rarely need to run `br8n index`
yourself.

## Step 4: Restart your agents

- **Claude Code:** quit and start it again. The plugin loads when a session
  starts.
- **Codex:** run `/hooks` once and trust br8n's hook.
- **Claude Desktop:** restart it.
- **Gemini CLI and Cursor:** they pick br8n up in their next session.

## Step 5: Check it works

```bash
br8n status
```

The first lines show the version and the install checks, and every one should
pass. The `ollama:` line should say Ollama is reachable and the model is
present, and `documents:` should be more than zero.

Then try it:

- `br8n search "something you know is in your notes"` should list the
  matching notes.
- In Claude Code, ask about something from your notes. br8n adds the most
  relevant passages to your prompt automatically, and Claude can also search
  explicitly with the `br8n_search` tool.
- `br8n dashboard` shows the same things visually. The **Health** tab lists
  any install problem with its fix, and the **Search** tab shows exactly what
  br8n found and what it passed to Claude.

If something is off, see [Install problems](#install-problems).

## Update

```bash
br8n update
```

It downloads the newest release, checks it and installs it. The dashboard's
Health tab has the same button. Restart your agents afterwards. Once a day, a
new Claude Code session checks whether a newer release exists and tells you
when one does. Nothing installs until you run `br8n update`.

`br8n update --check` reports the installed and latest versions without
installing. To switch the daily check off:

```toml
[update]
check = false
```

## Uninstall

```bash
br8n uninstall            # removes br8n, its plugin, the PATH link, and its entries in every connected agent
br8n uninstall --purge    # also removes your br8n config and index
```

Your notes are never touched. Without `--purge`, the config and the index
stay so a reinstall picks up where you left off, and the command prints where
they are.

Two things stay behind:

- **The embedding model.** Ollama keeps it, because other tools may use it.
  Remove it with `ollama rm qwen3-embedding:0.6b`.
- **The backup schedule, if you set one** with `br8n backup schedule`.
  Remove it first with `br8n backup schedule --uninstall`.

## Build from source

Use this on an Intel Mac, on a Linux with glibc older than 2.39, or when no
release build exists for your machine. An Intel Mac has no release build
because lbug's prebuilt x86_64-apple library references `___cpu_model`,
which Apple's linker cannot resolve.

1. Install a current stable Rust toolchain with [rustup](https://rustup.rs).
2. Install a compiler:
   - On macOS, Xcode 16 or newer.
   - On Linux, GCC 13 or newer plus `libssl-dev` and `pkg-config`
     (`sudo apt install build-essential libssl-dev pkg-config` on Ubuntu).
3. Build and install:

   ```bash
   git clone https://github.com/Jagma/br8n.git && cd br8n
   cargo build --release && target/release/br8n install
   ```

Then continue at [Step 2](#step-2-answer-the-installers-questions).

The linker flags br8n needs are in `.cargo/config.toml`. If you set
`RUSTFLAGS`, cargo ignores that file and the database extensions silently
fail to load.

A source build has the same version number as the release, so `br8n update`
reports it as current. Run `br8n update` only when you want the release
build back.

## About the model

br8n turns your notes into searchable vectors with one small model, run
locally through [Ollama](https://ollama.com):

```sh
qwen3-embedding:0.6b
```

`br8n install` downloads it after asking. With `contextual = true` in
`[embed]`, br8n also needs the enrichment model named in
`[embed] enrich_model`, and `br8n install` downloads that too.

To embed on another machine, such as a GPU box or LM Studio, see
[Embedding somewhere other than local Ollama](configuration.md#embedding-somewhere-other-than-local-ollama).

## Install problems

Run `br8n status` first. It names the problem.

- **`install:` shows a failed check:** run `br8n install` again. It repairs
  every check it can and prints the ones it cannot.
- **`command not found: br8n`:** add the `export PATH=...` line the
  installer printed to your shell profile, then open a new terminal.
- **macOS says it can't verify br8n, or stops it the moment it starts:**
  the file came from a browser and still has the "downloaded" mark. Run
  `xattr -d com.apple.quarantine br8n` on the unpacked file, or use Option A
  or B, which never add the mark.
- **`gh release download` says "no assets match":** the release has no build
  for your machine. [Build from source](#build-from-source).
- **`curl` returns 404:** no release has been published yet;
  [build from source](#build-from-source).
- **`ollama: unreachable`:** start Ollama (open the app, or run
  `ollama serve`).
- **"the downloaded binary reports ... expected ...":** the release file
  doesn't match its version. Nothing was installed. Please report it.
- **"will not execute on this machine":** the release build doesn't match
  your computer. [Build from source](#build-from-source).
- **"update check failed":** there is no network, or GitHub rate-limited the
  check. `br8n update` still works once the network is back.
- **Nothing is added to Claude's prompts after an update:** restart Claude
  Code. The running session still has the old version.
