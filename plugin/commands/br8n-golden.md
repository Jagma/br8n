---
description: Build the golden set br8n bench measures against, and check it
allowed-tools: Bash(br8n:*)
---

`br8n bench` is the only objective signal for whether a change to retrieval
helped or hurt, and it measures against a golden set: queries, and the document
each one should find. `br8n golden` writes that file and checks it.

Two subcommands:

- `br8n golden init` writes a starter set to `golden.toml` beside the config,
  seeded with real uris and titles taken from the user's own index. It refuses
  to overwrite an existing file without `--force`.
- `br8n golden check` reports every problem in the set at once. Errors exit
  non-zero; title echoes are warnings, because that judgement is the user's.

Decide which the user wants from what they asked for. If they have no golden
set, run `init`; if they have one, run `check`.

After `init`, tell them what the file needs from them and why:

- Every seeded case is COMMENTED OUT and the file parses to zero cases, so
  `br8n bench` refuses it until they fill some in. That is deliberate — a case
  with an empty query is a valid case, it gets embedded and scored, and on a
  small corpus it scores as a HIT. A fabricated number inside a real recall
  figure is worse than no number.
- Completing a case takes three uncommented lines: `[[case]]`, `query`, and
  `expect`. Uncommenting two of the three is the most common mistake and
  `check` names each case it happens to.
- The query must NOT repeat the document's title. The title was indexed, so a
  query echoing it is scored on string matching rather than retrieval and will
  keep passing after a change that broke everything else. Each stub prints its
  document's title so they can ask what they would have asked having forgotten
  it.
- Negative cases (`expect_none = true`) need subjects the corpus genuinely does
  not cover. They are the only class that can calibrate the `[hook] threshold`.
  Five is a floor, not a target.

When reporting `check`, read the LAST line. It is `N errors, M warnings`, and
when any check did not run it says so and that the result is NOT a clean bill
of health — a file holding no cases reports exactly that, because nothing was
examined. Do not present that as a pass.

Then say what to do next: fix the errors, re-run `br8n golden check`, and once
it is clean run `br8n bench`. Remind them that a recall figure belongs to the
golden set AND the corpus it was measured against, so editing this file makes
the previous number incomparable.
