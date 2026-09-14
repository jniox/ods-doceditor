---
name: doceditor-batch-20260914-lot9
description: Lot 9 — one number used as two ceilings, so the documented maximum was unreachable; the bench that carried no JsonConfig at all; and the git checkout that ate uncommitted work
metadata:
  type: project
---

Lot 9 (`8b76869` → `c358e42`), **twelfth** dev turn on unit
`doceditor-c20260909-1345`, still PR #3. 104 tests (95 before). CI run
`34800209440` on the exact pushed sha.

**Fifth consecutive BA report with nothing code-actionable** — 18 MET, AC-000
(no `spec.md`; `HR-20260913-001` DISPATCHED but never executed, its repair
`HR-20260913-006` still `PENDING` with `resolution=None`) and AC-012 (broker
env vars absent from `~/dev/ops/cloudrun/doceditor.json`). Both re-measured in
five minutes, transcripts in `evidence/c358e42/07` and `/08`, deliberately
untouched. Then went reading a path, as in lots 5–8.

**The defect: one number wired as two different ceilings.**
`MAX_DOCUMENT_SIZE_MB` fed *both* `JsonConfig::limit` (the HTTP payload) *and*
`with_max_content_bytes` (the stored body). JSON always makes the payload
bigger than the body it carries, so the two can never be the same number.
Measured on the running binary at `MAX_DOCUMENT_SIZE_MB=1`:

| body sent | on the wire | answer |
|---|---|---|
| ceiling − 64 | 1 048 551 | 201 |
| **ceiling** | 1 048 615 | **413 `text/plain`** |
| ceiling + 1 | 1 048 616 | 413 `text/plain` |
| **ceiling / 2, all `"`** | 1 048 615 | **413 `text/plain`** |

Three defects, one status code:

1. A document of **exactly the documented maximum could not be stored.** The
   real maximum was the ceiling minus an envelope no caller can compute.
2. `validate_content`'s 422 was **unreachable from HTTP** — the framework
   always answered first. Only creation *from a template*, where the body is
   not in the payload, could reach it. Dead code that everyone read as a rule.
3. The last row names the mistake: a body of **half** the ceiling refused
   because it is made of quote characters. The ceiling was on the **encoding**,
   so the largest storable document depended on which characters were in it.

Plus: `text/plain`, the one error shape of this API no client can parse and
`docs/openapi.yaml` does not describe.

**Why nothing was red, and it is the generalisable part.** The only test of the
ceiling built an `App` carrying **no `JsonConfig` at all** and called a service
built with `with_max_content_bytes(64)`. **A hand-made bench does not merely
drift from production — it can be missing the very component under test.** Its
doc-comment claimed the ceiling was "enforced on the body, not only on the HTTP
payload"; in production it was enforced *only* on the HTTP payload. Lot 8's
lesson was "test the normalisation through the boundary that reports it"; this
is its sibling — **and the cure is not a better test, it is a shared wiring
function.** `api::payload::limits()` installs both ceilings and `main.rs` and
the tests call it, so the bench *cannot* disagree.

**The fix**: body ceiling = `MAX_DOCUMENT_SIZE_MB`; payload ceiling =
`2 × body + 64 KiB`. Two is the worst case of JSON escaping for text (`"`→`\"`,
`\`→`\\`; non-ASCII is emitted verbatim, so accented or CJK bodies do not
expand at all). 64 KiB covers the largest title+metadata this service's own
rules admit (< 7 KiB, asserted by a unit test so the constant is not a magic
number). Over the body ceiling → 422 naming the field; too long to read at all
→ 413 **in the service's shape**, naming both numbers, via
`JsonConfig::error_handler`. New `AppError::PayloadTooLarge` +
`payload_too_large` in the contract, which `tests/error_surface.rs` already
holds to exactly the codes `error.rs` emits — that guard caught the drift for
free.

**Traps paid for this lot:**

- **`git checkout -- <file>` restores from HEAD, not from before your last
  edit.** Used it to undo a mutation on `docs/openapi.yaml` and silently lost
  every uncommitted edit to that file. Copy to `/tmp` and `cp` back, always.
- **`cargo test` stops after the first failing binary**, so the first mutation
  matrix run only exercised `content_test` and looked far narrower than it was.
  `--no-fail-fast` is mandatory for a mutation matrix.
- `use actix_web::test` shadowing `#[test]` — hit again, exactly as
  [[doceditor-batch-20260914-lot8]] records. The sync unit test moved into
  `src/api/payload.rs`'s own `#[cfg(test)] mod tests`, where it belongs.
- The repo's local `.env` carries `JWT_RSA_PUBLIC_KEY_B64=__NEEDS_HUMAN__`, and
  `build_jwt_config` prefers RS256 whenever that variable is merely *present* —
  so running the binary from the repo root panics at boot. Run it from `/tmp`
  so `dotenvy` finds no `.env`.
- Probe rows are real rows: 9 documents / 9 216 KiB of quote characters went
  into the **shared** `editor` schema and were deleted before the turn ended.

See [[go-read-the-path-yourself]] — this is the fifth turn in a row where the
report said nothing was actionable and a read of one path found something real.
