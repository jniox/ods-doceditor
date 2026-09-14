---
name: doceditor-batch-20260914-lot11
description: Lot 11 — the search index decided what could be stored, the read path that would have inherited the same error, and why the bound had to be named once in SQL
metadata:
  type: project
---

Lot 11 (`c1009f8` → `6b7f3a5`), **fourteenth** dev turn on unit
`doceditor-c20260909-1345`, still PR #3. 125 tests (118 before), 22 binaries.

**Seventh consecutive BA report with nothing code-actionable** — 19 MET, AC-000
(no `spec.md`) and AC-012 (`cloudrun/doceditor.json` sets no broker variable).
Both re-verified in five minutes from the HR JSON files themselves
(`HR-20260913-001` still `DISPATCHED` with no execution, its repair
`HR-20260913-006` still `PENDING`/`resolution: null`) and from
`business-rules.md` (BR-0001…BR-0016, none mentions doceditor). Untouched on
purpose — BR-0002. Then read a path, as in lots 5–10.

**The defect: the full-text INDEX decided what could be STORED.** Migration 006
indexed `title || ' ' || content` whole. A `tsvector` cannot hold more than
1 048 575 bytes of lexemes, and an index expression that raises makes the
*insert* raise. What fills that budget is the **vocabulary** of the text, not
its length — duplicates merge, distinct words do not. Measured on PostgreSQL 17
before writing anything:

| body | contents | before |
|---|---|---|
| 348 893 B | every word distinct | stored |
| 708 893 B | every word distinct | stored |
| **798 893 B** | every word distinct | **`ERROR: string is too long for tsvector`** |
| 1 888 894 B | every word distinct | ERROR (2 598 012 B of lexemes) |
| 10 050 000 B | repetitive prose | stored |

So a 9.6 MiB contract of ordinary prose stored fine while a 0.8 MB annex of
reference codes — a pasted export, a generated appendix — answered
`500 {"error":"internal_error"}`, against a **published ceiling of 10 MB**
(`MAX_DOCUMENT_SIZE_MB`). The third batch in a row where a limit turned out to
be applied to a different quantity from the one its name promises: payload vs
body (lot 9), bytes vs characters (lot 10), now *the index* vs *the document*.
The checklist question that found it: **what else, besides the code that says
"limit", can refuse this write?** An expression index is code.

**The fix is a named projection, and the naming is the load-bearing part.**
Migration 009: `editor.searchable_text(title, content)` = `left(title || ' ' ||
content, 250000)`, `IMMUTABLE`, `RETURN`-body form (parsed at creation, so no
`search_path` reinterpretation under an index); old index dropped, new GIN index
built on the function; `document_repo::search_predicate()` sends the same call.

**Truncating only the index would have MOVED the failure, not removed it** —
measured under mutation, not assumed: with the bounded index in place and the
old inline predicate restored, `GET /documents?search=…` answers **500** for any
tenant owning one large document, because a bitmap heap scan *rechecks* the
condition on the heap row. Same lesson as lot 9's `payload::limits()`: one
function that production and the tests both call, except here the second reader
is PostgreSQL itself — and it confirms the match by choosing
`idx_documents_searchable_fts` for the predicate (`EXPLAIN`, seqscan off).

**250 000 characters is measured, not chosen.** `left()` counts characters, the
limit counts bytes of lexemes — exactly the conversion that produced lot 10.
Worst alphabet (distinct accented tokens): 576 628 B, half the limit; 900 000
characters overflows at 1 314 648 B. The test re-measures in three alphabets so
the constant cannot be raised blind.

**Mutations, clean partition:** old inline predicate → 3 red (including a `500`
on a GET); bound raised to 900 000 → 3 red; original red run (old index) → 5 red
with `string is too long for tsvector (1478008 bytes)` verbatim. Transcripts in
the evidence folder, including the prettiest proof: **the index migration 006
built can no longer be created at all**, because the documents it used to refuse
now exist (`CREATE INDEX …` inside a rolled-back transaction → the same ERROR).

**Traps worth reusing:**

- **Do not touch an old migration file.** sqlx stores a checksum per version;
  editing `006` to add a comment would raise `VersionMismatch` on every existing
  database. A fresh database still builds 006's unbounded index and 009 drops
  it — wasteful, correct, and the only safe shape.
- `docker exec` needs **`-i`** for a heredoc; without it psql reads nothing and
  the command prints nothing at all, which looks like a hung container.
- Running the binary locally picks up the repo's **`.env`** through `dotenvy`,
  and that file carries `JWT_RSA_PUBLIC_KEY_B64=__NEEDS_HUMAN__` → panic at
  boot. `env -u` does not help (dotenv then fills the hole). Copy the binary and
  run it from `/tmp`.
- `#[test]` resolves to `actix_web::test` in any file that does
  `use actix_web::test` — lot 8's trap, hit again.
- `CREATE INDEX CONCURRENTLY` is impossible in a sqlx migration (they run in a
  transaction). Said so in the ADR rather than discovered later.
- 43 probe documents and 44 versions (48 MB) removed from the shared `editor`
  schema before the turn ended.

See [[go-read-the-path-yourself]] — seventh turn in a row.
