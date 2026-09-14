---
name: doceditor-batch-20260914-lot10
description: Lot 10 — a bound named in characters and applied in bytes, the rename that stored what it had not validated, and a hypothesis measured then discarded before any code was written
metadata:
  type: project
---

Lot 10 (`89888b9` → `d5b31f3`), **thirteenth** dev turn on unit
`doceditor-c20260909-1345`, still PR #3. 118 tests (104 before), 21 binaries.
CI run `34803138341` on the exact pushed sha.

**Sixth consecutive BA report with nothing code-actionable** — 19 MET, AC-000
(no `spec.md`; `HR-20260913-001` DISPATCHED **24.0 h** without execution, its
repair `HR-20260913-006` still `PENDING`/`resolution: null`) and AC-012
(`~/dev/ops/cloudrun/doceditor.json` sets neither broker variable). Both
re-measured in five minutes, transcripts in `evidence/d5b31f3/01-03`,
deliberately untouched. Then read a path, as in lots 5–9.

**The defect: a bound named in characters, applied in bytes — and a value
validated that was not the value stored.** `title`, `comment` and metadata
string values are bounded in *characters* by `docs/openapi.yaml` (`maxLength`)
and by `VARCHAR(500)` (migrations 002/003). The code used `str::len()`.
Measured on the running binary:

| request | before | after |
|---|---|---|
| `POST` title = 500 × `é` | 422 | **201** |
| largest accepted accented title | **250** chars | 500 |
| largest accepted CJK title | **166** chars | 500 |
| `POST` title = 501 × `a` | 422 | 422 (still bounded) |
| `PATCH` title = `' '` + 500 × `a` | **500 internal_error** | **200** |
| `PATCH` title = `'   Contrat   '` | stored `'   Contrat   '` | stored `'Contrat'` |
| `POST` title = `'   Contrat   '` | stored `'Contrat'` | stored `'Contrat'` |
| `POST` comment = 400 × `é` | 422 | **201** |

Four faults, one fault:

1. **The documented maximum was unreachable in every alphabet but ASCII.** The
   largest storable title was 500, 250 or 166 characters depending on its
   contents — the *exact* shape of lot 9 at another site, so the question "what
   quantity is this limit really applied to" earns its place as a checklist item.
2. **A legitimate rename answered 500.** `update_document` validated
   `title.trim()` and handed the **untrimmed** string to the repository one call
   below; 501 characters into `VARCHAR(500)` is `22001 value too long`, rendered
   as `internal_error`. Nobody had written a test that renamed to a *long* title.
3. Created trimmed, renamed padded: the same title stored two ways.
4. The message said "characters" while the code counted bytes.

**The fix is a type, not a better `if`.** `domain::text::Title` / `Comment` can
only be built by `parse`, carry the normalised form, and **`document_repo` and
`version_repo` take them instead of `&str`** — the repository is the only writer
of those columns, so the guarantee is complete rather than conventional. Same
cure as `Pagination` (lot 8), stated in `CLAUDE.md`. Handler parses on PATCH,
service parses on POST and on `create_version` (whose rule left `versions.rs`).
`tests/common` gained `title()` / `comment()` so fixtures build the same value
the wire does. Mutation matrix: 4 mutations, clean partition, nothing bleeds.

**A hypothesis measured and discarded before a line of code.** rdkafka 0.36's
`impl Drop for BaseProducer` really does `purge(queue().inflight())` before
flushing (read in the registry source), so "events accepted just before a Cloud
Run instance dies are silently lost" looked airtight — and it is the exact
family this repo already burned four months on. **It does not reproduce**:
`ThreadedProducer::drop` joins its polling thread first, and that window is
enough for a local broker to acknowledge everything (1/1, 5/5, 50/50 on three
attempts each, plus flush and alive controls). Cost: ~15 minutes and a throwaway
`tests/zz_drop_probe.rs`, deleted. Worth every minute — the "fix" would have
been a `Drop` impl justified by a premise, plus a test that is flaky by
construction. See [[operational-not-code]]: measure the premise, including when
the library source seems to prove it.

**Traps and notes:**

- `cargo` is not on `PATH` in this shell: `export PATH="$HOME/.cargo/bin:$PATH"`
  first, or every command dies with "No such file or directory".
- A `cd` inside a compound Bash command **moves the session's working
  directory**; use absolute paths.
- My first search probe reported `search=Reference -> total=0` and looked like
  "full-text search is entirely broken". It was **my probe**: an earlier step had
  renamed that document. Re-measured cleanly, search is fine (`Contrat`,
  `contrat`, `prestation`, `Facture` all hit). *Check the fixture before
  reporting the service.*
- `?search=` (empty) answers `200 total=0` on a tenant that owns documents —
  same family as the unknown `?status=` fixed in lot 8, much more benign,
  reported not fixed.
- Probe rows are real rows: 39 documents and 40 versions removed from the shared
  `editor` schema before the turn ended.

See [[go-read-the-path-yourself]] — sixth turn in a row.
