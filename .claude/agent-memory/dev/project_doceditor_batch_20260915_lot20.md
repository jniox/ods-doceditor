---
name: doceditor-batch-20260915-lot20
description: Lot 20 — five ordinary inputs answered 500 because the database wrote the refusal; the half-measured finding I reported instead of coding, and the tool trap that put a real NUL byte in three source files
metadata:
  type: project
---

**Lot 20 (2026-09-15, commit `4c520b2`, branch `feat/doceditor-c20260909-1345-lot2`,
PR #4).** Fifteenth BA report in a row with nothing code-actionable: AC-012 PARTIAL,
gap = a Cloud Run traffic promotion.

**Why that one really was not mine, and how I checked.** Lots 14 and 16 both found a
decision the report called *pending* that the JSON said was *taken and unrouted, with
`enactor: "dev"`*. So I read the JSON again. This time: HR-20260915-002 is `RESOLVED`,
`enacted: null`, `deferNote: "un resolver est déjà en vol"`, and — the deciding field —
**`options[0].enactor = "resolver"`**. The executor is named and it is not dev. The
check must still be run every time; the answer is what varies, not the question. Live
traffic re-measured anyway: 100 % on `doceditor-00003-vkq`, 0 % on `-00004-jon`.
HR-20260915-003 is `DONE` and injected **BR-0019** — which I did apply, by fixing the
published contract (below).

**The defect, found by reading a path.** `U+0000` is legal JSON (`"\u0000"`), a legal
query value (`%00`), and the one character PostgreSQL will not store — `22021` in
`text`, `22P05` in `jsonb`. Nothing looked for it, so five fields carried it to the
database and **the database wrote the reply**: `title`, `content`, a `metadata` value,
a **nested metadata key**, a snapshot `comment` and `?search=%00` each answered
`500 internal_error`, with an ERROR log line each. Measured on the running binary
before any code was written.

Why a 500 is the wrong answer three times over, and worth a lot on its own: it says
*our fault, try again* about a request that can never succeed (a client with a retry
policy retries for ever); it names no field, so there is nothing to act on; and it
pages somebody for an input a caller chooses at will. The contract already had the
answers — 422 for a body field, 400 for a query parameter.

Third instance here of **a constraint living in the COLUMN rather than in the code**
(lot 10's `VARCHAR(500)`, lot 11's `tsvector` budget, the encoding now). The cure is
the repo's usual one: one named rule (`domain::text::nul_at` + `nul_refusal`) crossed
by all five sites. The site nobody would have added by hand is the **nested metadata
key** — the `^[a-z][a-z0-9_]{0,63}$` pattern is stated by the contract about the
object's *own* keys, so `{"kind": {"nested\u0000key": 1}}` crossed every check.

**Two tests that pass before the fix, on purpose**, and both earned their place:
the **premise** (bind `U+0000` to `$1::text` → `22021`, to `$1::jsonb` → `22P05`, asked
of PostgreSQL rather than asserted in prose) and the **width** (`U+0001` must still
round-trip). Without the second, the next reader turns a one-character bound into a
sanitiser and ADR-001 quietly dies.

**The finding I did NOT code, and why it is the more interesting half.**
`X-Correlation-Id` is adopted verbatim at any length and published as
`ce-correlationid` on every event — measured: 8 000 bytes, 60 000 characters, all
republished intact (actix refuses the head above ~128 KiB with a `431`). Google
documents a Pub/Sub limit of **1 024 bytes per attribute value**, which would make
every event of such a request `INVALID_ARGUMENT` — *silently* dropped, this service's
signature failure. I could not measure that half from this host, and I logged the three
attempts: the API resolves the topic **before** validating the message (so publishing
to a non-existent topic cannot discriminate), this host has no `pubsub.publisher` on
staging (authorisation precedes validation too), and no emulator is installed. So it is
reported in `CLAUDE.md`, not fixed: bounding an id the contract calls "adopted when
supplied", on an unverified premise, is overturning a neighbouring decision in passing.

**A second thing verified and deliberately left alone.** `JWT_RSA_PUBLIC_KEY_B64` is
the only variable `config.rs` reads without the blank filter this repo applies
everywhere else, so a blank value panics the boot instead of being treated as absent.
Making it "consistent" would let a deployment with an empty RSA key and
`JWT_ALLOW_HS256=true` **fall back to HS256 in silence**. Refusing to boot is the safer
of the two. An inconsistency you can see is not automatically a defect you may fix.

**Tool trap, and it is mine to remember:** writing `\u0000` inside a tool's JSON input
inserts a **real NUL byte** into the file. Three source files carried one before I
hexdumped the line (`od -c`); `grep -P '\x00'` reported nothing because it treats the
file as binary. Rust compiles a NUL in a comment without a word. Check with `od`, and
write `\\u0000` when the literal text is what you want.

Hygiene: 9 probe documents created, 9 deleted the same turn (lot 19's lesson — leftover
fixtures look exactly like traffic); 0 leftover `doceditor-roundtrip-*` topics.
Evidence: `~/dev/ops/reviews/doceditor/evidence/4c520b2/` (5 transcripts + index).
Report: https://reports.dev.orbusdigital.com/run-dev/20260915-d58f64c66526.html

See [[go-read-the-path-yourself]] for the checklist this came off, and
[[doceditor-batch-20260915-lot19]] for the previous turn's instrument lesson.
