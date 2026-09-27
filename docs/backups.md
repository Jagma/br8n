# Backups


`br8n backup` copies three things to Amazon S3, Google Drive, or both:

| What | Why it is worth keeping |
|---|---|
| `config.toml` | Your tuned thresholds, tiers and weights. Re-deriving them means re-running `br8n bench`. |
| `golden.toml` | Your bench set, written by hand. Nothing regenerates it. |
| The index | Rebuildable, but a large corpus takes hours of embedding to rebuild. |

That is all it copies. Your notes, your `sources` folders and your Claude Code
session transcripts are **not** backed up. Treat the backup as a warm-start
cache plus your tuning, not as protection for your data. It matters on a new
machine: `br8n index` removes every document whose source file it cannot
find, so an index restored onto a machine that lacks the original files is
pruned away by the next indexing run. Back up your notes the way you already do
(a git remote, Time Machine), and use this to skip the re-embedding.

Uploads are content-addressed. After the first run, only what changed is sent,
so a daily backup of an unchanged setup transfers almost nothing.

## Encryption

Backups are encrypted on your machine before upload, and encryption is on by
default. Generate the key first:

```
br8n backup init
```

It prints the key once and asks you to store a copy somewhere other than this
machine. **If you lose the key, the backups are unrecoverable.** There is no
recovery path, by design. Set `encrypt = false` under `[backup]` if you would
rather not carry that risk.

## Amazon S3

```toml
[backup]
enabled = true
targets = ["s3"]

[backup.s3]
bucket = "my-br8n-backups"
region = "eu-west-1"
profile = "default"
```

Credentials come from the named profile in `~/.aws/credentials`, **not** from
environment variables. That is deliberate: cron runs with almost no
environment, so an `AWS_ACCESS_KEY_ID` exported in your shell does not exist
when the backup actually runs. Objects go under `prefix` (default `br8n/`)
with storage class `storage_class` (default `STANDARD_IA`).

## Google Drive

Drive needs an OAuth client of your own:

1. In the Google Cloud console, create a project and enable the Drive API.
2. Under **APIs & Services > OAuth consent screen**, set the publishing status
   to **In production**. Do not leave it in *Testing*: Google expires refresh
   tokens for apps in Testing after 7 days, and your backups would stop about
   a week later with no other symptom.
3. Under **Credentials**, create an OAuth client ID of type *Desktop app* and
   download the JSON.

```toml
[backup]
enabled = true
targets = ["drive"]

[backup.drive]
client_secret_file = "~/.config/br8n/drive-client.json"
```

Then authorize once, interactively:

```
br8n backup auth drive
```

It opens a browser for consent, creates a folder named `br8n backups` in your
Drive, and prints a `folder_id = "..."` line to add under `[backup.drive]`.
Let it create the folder. `br8n` asks only for the `drive.file` scope, which
lets it see nothing but files it created itself: the rest of your Drive stays
invisible to it, and so does any folder you make by hand. A backup run never
opens a browser. If the token cannot be refreshed, the run fails and names the
command to re-run.

## Running it

Check the setup before trusting it. `check` clears the AWS and Google
environment variables first, so a configuration that works only in your shell
fails here instead of at 3am:

```
br8n backup check
br8n backup
```

Then schedule it:

```
br8n backup schedule --at "0 13 * * *"
```

Pick an hour the machine is awake. Cron does not run while a laptop is asleep
and does not catch up when it wakes, so a 3am job on a closed laptop never
fires. `br8n status` shows how long it has been since every target last
succeeded, which is how you notice.

`br8n backup schedule --uninstall` removes the entry, and running `schedule`
again replaces it rather than adding a second one. `br8n backup --help` lists
every subcommand. Each run writes one line to `db.backup.log` beside the index
and exits 0 when it backed up, 1 when something failed, and 2 when there was
nothing to do (no backup configured, or another run in progress).

Each run keeps 30 generations of your files and the last 2 index snapshots
(`keep_generations` and `keep_index`). The index gets a shorter history on
purpose: it is large, it never dedupes, and it is a cache.

## Restoring

```
br8n backup status         # last success, and the generations each target holds
br8n restore --dry-run     # count what would be written, and write nothing
br8n restore               # config.toml and golden.toml
br8n restore --index       # the index as well, if it is compatible
```

A restore reads from the first target in `targets`. Existing files are left
alone unless you pass `--force`. `--index` is refused outright if the backup's
index was built with a different embedding model, width or chunk size: those
embeddings are not comparable, so the restored index would silently return
nonsense. Restore without `--index` and run `br8n index --reindex` instead.

Never backed up: your encryption key, your Drive token, the index lock, the
progress file, the logs, and the half-built (`db.new`) and previous (`db.old`)
index generations.
