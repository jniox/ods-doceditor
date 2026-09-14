---
name: doceditor-batch-20260914-lot14
description: Lot 14 — the first turn with a canonical spec, and two human decisions that were taken, dispatched and never executed; the ceiling sized against the platform's own default concurrency
metadata:
  type: project
---

Lot 14 (`3255187` → `feaed8e`), **seventeenth** dev turn on unit
`doceditor-c20260909-1345`, still PR #3. 158 tests (154 before), 25 binaries.

**The turn's real find was not in the code: it was in two JSON files.** The BA
report was the tenth in a row with nothing code-actionable (19 MET, AC-000
missing, AC-012 partial, both declared "outside this repository"). But reading
the human reviews it cited showed **two decisions already taken by
it@orbusdigital.com and never enacted**, both naming *dev, in the doceditor
repository* as the executor:

- **HR-20260914-001** (resolved 08:16:33, dispatched 08:20:59) — option A,
  lower `MAX_DOCUMENT_SIZE_MB`. The dispatcher wrote its own failure into the
  file: `enactError: {"reason": "verbe inconnu : doceditor sizing A"}`.
- **HR-20260913-001 → HR-20260913-006** — write the spec *and fix the topic
  name*. The resolver ran it **during this turn**: `spec.md` appeared on disk
  while I was working, and its §4.2 settles `editor-events` and explicitly names
  `src/config.rs` as a source to rewrite.

**Read the HR JSON, not the BA's summary of it.** The report said of the sizing
decision only that it was "dispatched and left unexecuted"; the JSON carried the
option text, the measurement, and `"enactor": "dev"`. Three turns of "nothing
actionable" would have continued indefinitely otherwise. And **re-check the
specs directory even when fourteen cycles say it is empty** — it was not, by
then.

**The ceiling, and the number nobody had written down.** `MAX_DOCUMENT_SIZE_MB`,
`memory: 512Mi` and the request concurrency only make sense together; the third
term existed nowhere because `ops/cloudrun/doceditor.json` sets no
`--concurrency`, so **Cloud Run's own default of 80** is in force. That is the N
the deployment must survive, and it turns the sizing question into arithmetic:

| ceiling | N=80 |
|---|---|
| 10 MB | dies at **N=12** already (N=10: 451 MiB) |
| 4 MB | `Result=oom-kill, MainPID=0` |
| 3 MB | 200 ×80, peak **475 MiB** — 93% of the cap, not a margin |
| 2 MB | 200 ×80, peak **340 MiB**; 338 on the shipped default |

So the human's "ex. 2 Mo" is the largest value that survives the platform's own
default, and the ADR says so rather than restating the choice. **When a decision
offers an example value, measure whether the example is the right one** — the
answer here made the decision stronger, not weaker.

**Two bench shapes, and only one is realistic.** N threads PATCHing the *same*
document serialise on the `FOR UPDATE` lock (lot 5's fix), so they hold their
payloads but not the SQL; N threads on *distinct* documents do not serialise.
The first survived N=14 at 10 MB, the second died at N=12. Lot 12's published
numbers (6 survive / 10 fatal) came from a slightly different bench and a
different host load; the *ordering* is identical and the conclusion unchanged,
which is the honest way to report a discrepancy rather than quietly restating
the older figure.

**What the same lever also revealed, one layer down.** Three independent
spellings of one ceiling: `config.rs`'s `"10"`, `DocumentService`'s own
`10 * 1024 * 1024`, and `.env.example`. Plus `main.rs` doing
`mb * 1024 * 1024` — non-saturating — *one line before* handing the result to
`payload_ceiling`, a function that saturates and whose doc-comment explains why
("a service must not depend on an operator not typing a large one"). And
`.parse().unwrap_or(10)`, which turns `MAX_DOCUMENT_SIZE_MB=2MB` into 10 MB in
silence. A safety ceiling that ignores the operator is worse than one that
refuses to boot: it dies later, with the configuration file still reading as
though it had been obeyed.

**The contract named the variable and never the number.** `docs/openapi.yaml`
said "bounded by `MAX_DOCUMENT_SIZE_MB`" three times — the one fact a client
integrating against it needed was the one fact withheld. It now says
`2097152 bytes`, and deliberately carries **no `maxLength`** on `content`:
that keyword counts characters and would publish a ceiling three times too large
for a Chinese body. Lot 10's byte/character trap, one field over, avoided rather
than repeated.

**Traps worth reusing:**

- `pkill -f "/tmp/.../ods-doceditor"` **kills the shell running it** — the
  pattern matches the invoking command line. The heredoc that followed silently
  never ran. Use `pgrep`, then `kill` by pid.
- A Python `urllib` probe of an over-ceiling payload reports `Broken pipe`, not
  `413`: it writes the whole body before reading, and actix answers and closes
  early. `curl` (with or without `Expect: 100-continue`) gets a clean
  `413 application/json` naming both numbers. **Check the client before
  reporting the server** — same lesson as the fixture that had been renamed.
- `jwt.encode({... "aud": ""})` is rejected `401` by `jsonwebtoken`; `aud: None`
  or omitted works. Cost ten minutes of "why is my probe unauthorised".
- Splitting one working tree into two honest commits, when four files change for
  both reasons: snapshot the final files, script the revert of one theme, run the
  suite in that state (154 green), commit, restore, run again (158), commit. Two
  minutes of test time for a history a reviewer can read.
- 1.4 GB of bench bodies went into the shared `editor` schema and came back out;
  `psql` count before and after is in the evidence.

**Left for someone else, deliberately.** The spec was written at 08:2x from the
BA report and therefore still says `MAX_DOCUMENT_SIZE_MB` "(défaut 10)" and
lists D-2 as *Ouvert*. The acceptance criteria themselves (AC-019, AC-024) are
phrased in terms of the *variable* and stay satisfied, so nothing is contradicted
— but four descriptive lines are stale and BR-0002 requires the rewrite. Named
line by line in the status and the run report rather than edited: the document
is forty minutes old and belongs to the spec-writer.

See [[go-read-the-path-yourself]] — and note the amendment it needs: the tenth
report with nothing actionable was the one where the actionable work was in the
decisions the report *cited*.
