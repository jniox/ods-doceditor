# doceditor

## Stack
Rust (Actix-web 4, sqlx 0.8, rdkafka 0.36, reqwest 0.12 for Pub/Sub)

`actix-web` is declared **twice** (dependencies + dev-dependencies) and both
entries must keep `default-features = false` without `http2`: the dev entry
alone is enough to pull `h2` 0.3 back into `Cargo.lock`, which is the file
`cargo audit` reads. See HR-20260909-001 and `tests/framework.rs`. `reqwest`
answers to the same rule for the same reason — `default-features = false`, no
`http2` — which is why the Pub/Sub producer talks REST and not gRPC (ADR-011).

**An advisory against a crate this service COMPILES fails the build; the
lockfile's tally does not.** `cargo audit` reads `Cargo.lock`, which is wider
than the binary — it pins the optional dependencies of our dependencies — so
its "1 vulnerability found" has, every time it has been examined here, named a
crate nothing compiles (`rsa` through `sqlx-mysql`, `anyhow`, `spin`). The
judgement is the intersection with `cargo tree -e normal` — that is BR-0010, and
it lives in `~/dev/ops/standards/STANDARDS.md` §7 rather than in the project's
`business-rules.md`, which is why lot 17 reported it as a phantom citation. It is
a command rather than a habit (`scripts/audit-delivered-graph.sh`, CI job
`Advisories`),
and `tests/advisories.rs` keeps that command wired and non-vacuous. It exists
because on 2026-09-14 RUSTSEC-2026-0285 landed on `rustls` 0.23.40 — shipped
here through `sqlx` since the service was written, and through `reqwest` since
ADR-011 — and **nothing in the repository went red**: the only dependency guard
names `h2` 0.3, and a guard shaped for one crate says nothing about the next
one. Take the fix, not the ignore; there is no `.cargo/audit.toml` here and
ADR-012 says why.

## Project
ods-platform

## Architecture
- Domain models: `src/domain/` (Document, DocumentVersion, DocumentStatus, and the
  parsed values the boundary builds: `pagination::Pagination`, `text::Title`,
  `text::Comment`, `metadata::Metadata`, and the one rule the query string
  crosses: `query::supplied`)
- Service layer: `src/service/` (DocumentService — business logic, orchestrates repo + events)
- API handlers: `src/api/` (HTTP handlers, auth extractor, health)
- Repository: `src/repository/` (PostgreSQL via sqlx, RLS via tenant_context)
- Events: `src/events/` — `producer` (the envelope, the trait, Redpanda/Kafka)
  and `pubsub` (Cloud Pub/Sub over REST, the transport HR-20260914-007 chose)
- Config: `src/config.rs` (env-based config)
- Error: `src/error.rs` (AppError with ResponseError impl)
- Entrypoint: `src/main.rs`

## API Endpoints
- `GET /health` — liveness probe
- `GET /ready` �� readiness probe (checks DB)
- `POST /api/v1/documents` — create document (body via `content`, or `template_id`)
- `GET /api/v1/documents` — list documents (paginated, filterable)
- `GET /api/v1/documents/{id}` — get document
- `PATCH /api/v1/documents/{id}` — update document (title, status, metadata, content)
- `DELETE /api/v1/documents/{id}` — soft-delete document
- `POST /api/v1/documents/{id}/versions` �� create version snapshot
- `GET /api/v1/documents/{id}/versions` — list versions
- `GET /api/v1/documents/{id}/versions/{version}` — get specific version (body included)

The published contract is `docs/openapi.yaml` (OpenAPI 3.1, validated in CI per
BR-0007). `tests/openapi_test.rs` fails the build if a route is added without
being documented — update both, or neither.

## Input the service normalises, and what it then reports
**The list response describes the page that was SERVED, never the one that was
ASKED for**, and that is a single value rather than a convention to remember:
`domain::pagination::Pagination` is built at the boundary, travels through the
service into the repository, and renders the response. Anything that adds a
paginated endpoint builds one too, instead of clamping in one layer and echoing
in another — which is exactly how this broke. Until 2026-09-14
`DocumentService` clamped (`page.max(1)`, `per_page.clamp(1, 100)`) while the
handler echoed `query.page` / `query.per_page`, so `?per_page=1000` answered
`"per_page": 1000` above at most a hundred documents (the published contract
says `maximum: 100`) and `?page=0` answered `"page": 0` above the first page: a
client computing `ceil(total / per_page)` sees one page of a hundred, and one
walking `0, 1, 2` reads the first page twice. Nothing was red — the clamp test
called the *service*, which is the one place the response is not visible.

**And the order the pages are drawn from is TOTAL**, which is the other half
of "a page is what was served": `ORDER BY updated_at DESC` alone ties whenever
one transaction writes two documents — `now()` is the transaction timestamp —
and PostgreSQL then returns tied rows in whatever order the plan produces,
choosing a different plan for different offsets. Measured over 2 000 tied
documents, `OFFSET 0` ran an index scan and `OFFSET 1900` a sort; walking all
twenty pages returned 2 000 rows holding **1 999** documents — one twice, one
never. Today's data has no ties at all (7 074 documents, zero), so the list was
stable by a property of the traffic rather than of the query. It is
`updated_at DESC, id DESC` now. Anything paginating here orders by something
unique; see ADR-013 and `tests/list_stability_test.rs`.

`Pagination::offset()` is **saturating, not `*`**. `(page - 1) * per_page`
overflows for a page a caller can type: `page=9223372036854775807` panicked the
debug build in `document_repo::list_documents` and, on the release build the
Dockerfile produces, wrapped to `OFFSET -200`, which PostgreSQL refuses
(`ERROR: OFFSET must not be negative`) — a 500 on a request that only means
"past the end". Past the end is an empty page, at any magnitude.

**Two guards that used to be vacuous, and the shape they share.**
`validate_metadata` states every BR-029 rule about *keys*, so `as_object()`
answering `None` skipped all of them and returned `Ok`: a string, a number, a
boolean or an array went into the `jsonb` column unvalidated, in a field the
contract types as an object. It now refuses a non-object (422) before anything
else. Likewise an unknown `?status=` filtered nothing and answered `200` with an
empty page — the one reply that is both wrong and plausible, since it reads as
"you own no documents"; it is a `400` now, as `PATCH` always was for the same
word. Both have the same shape: **a check that says nothing about the inputs it
was not shaped for is not a check.** See `tests/list_contract_test.rs`.

**And the third one, which had the same shape one level down.** The rule
`metadata` string values are at most 256 characters was applied with
`if let Some(s) = value.as_str()` — a statement about the values of the object
and about nothing else, so the *container* decided whether the rule existed:
measured on the running binary, `{"resume": 257 × 'a'}` was a `422` and
`{"resume": {"inner": 5 000 000 × 'a'}}` a `201`. Nothing bounded the object as a
whole either, so the only ceiling left was the payload one — 20 MiB — which is
itself *derived* from an allowance documented as covering "the largest envelope
this service's own validation admits … under 7 KiB". The premise was false by a
factor of three hundred, and the unit test guarding it restated the prose of the
rules and compared it to itself.

What that cost is not a refused request. `DocumentSummary` drops `content` on
purpose and **keeps `metadata`**, so a page multiplies it by up to a hundred:
thirty documents of 10 MB of metadata — thirty ordinary `201`s — answered
`GET /documents?per_page=30` with **300 010 939 bytes and a peak RSS of 945 MiB**
against the deployment's 512 MiB, i.e. an OOM kill of the instance, the same
ending as the version history above and through the one column that read kept.
Now: `domain::metadata::Metadata` is parsed at the boundary, the character bound
holds **at every depth**, and `MAX_METADATA_BYTES` (32 KiB) bounds the serialised
object. The two are not redundant — an array of a million admissible strings
breaks no per-value rule, one 5 000-character string breaks no size rule — and
32 KiB is *derived*: half of `ENVELOPE_ALLOWANCE_BYTES`, the number the payload
ceiling was already computed from. `payload.rs`'s test now reads the constants
the domain enforces, so the two cannot drift apart again. The worst page a caller
can build is `per_page × 32 KiB`: measured at 100 documents, **3 197 951 bytes,
64 ms, 23 MiB**. See ADR-008 and `tests/metadata_bounds_test.rs`.

(Re-measured on 2026-09-14 at the deployment's own limits, because the number
above bounds a single request and the platform serves 80 at once: **80
concurrent reads of that worst page** — 3 236 451 bytes each — answer `200` ×80
inside a 512 MiB cgroup, peak **95 MiB**. The hypothesis that the heaviest page
kills the instance at the platform's default concurrency is **false**; lot 13
bounded it correctly. Published because a measurement that refuses to credit a
hypothesis is worth as much as one that confirms it.)

## What a caller types is parsed here — the query string and the path included
**Every refusal this service puts on the wire carries its own error shape**, and
`api::payload::limits()` — the single function `main.rs` and the tests both call
— is where that is made true: `JsonConfig`, `PayloadConfig`, `PathConfig`,
`QueryConfig` and the answer to an unmatched route. They are one call because
they are one promise; an extractor wired without its error handler exempts
itself from the published contract in silence, and nothing goes red.

It had not been true. `docs/openapi.yaml` publishes exactly one error body and
AC-031 says its enumeration is exact — yet, measured on the running binary,
seven answers were outside it: `?page=abc`, `?per_page=5.5`, an `int64`
overflow and `?page=` all answered `400 text/plain "Query deserialize error: …"`,
`/documents/not-a-uuid` and `…/versions/abc` answered `404 text/plain`, and an
unmatched route answered `404` with **no body and no content type at all**. A
generated client reading `response.json()["message"]` gets a parse failure
instead of the message. Same seam as the payload ceiling of lot 9, two
extractors over, and invisible for the same reason: `tests/error_surface.rs`
reads `src/error.rs` and the contract, never the wire. The statuses did not
change — only the shape.

**And on the same query string: a parameter whose value is blank is a parameter
that was not supplied.** `?page=&per_page=&status=&search=` is one gesture — a
form submitted with nothing typed into it — and it used to have four answers:
`400 text/plain` for `page` and `per_page`, `400 application/json` for `status`,
and, worst because it is silent, `200` **with an empty page** for `search`, so a
tenant owning documents was told it owned none. The rule is not invented: it is
the one `api::middleware::correlate` already applies to headers, and it now
lives in `domain::query::supplied`, which the four parameters cross. `page` and
`per_page` therefore arrive as **strings** and are parsed by
`Pagination::parse`, which names the parameter at fault instead of quoting
serde.

**No wider than that.** A value that is not blank travels exactly as it was
sent: `?status=published%20` is still the `400` `tests/list_contract_test.rs`
chose for it on purpose, and `?page=%2020%20` is still refused. Trimming those
would have overturned a neighbouring decision while claiming to repair this one
— which is not something a batch gets to decide in passing. See ADR-010 and
`tests/query_contract_test.rs`, which reads the permitted error codes out of
`docs/openapi.yaml` rather than restating them.

## The fields a human types: characters, and one value that travels
`title`, `comment` and metadata string values are bounded in **characters** —
that is what `maxLength` counts in `docs/openapi.yaml` and what `VARCHAR(500)`
counts in migrations 002 and 003. Rust's `str::len()` counts bytes, and using it
here made the documented maximum depend on the alphabet: measured on the running
binary, the largest storable title was **500** ASCII characters, **250** accented
ones or **166** Chinese ones, on a product whose own examples read *Contrat de
prestation*. A 400-character comment was refused by a rule named "500
characters".

The other half was worse. `update_document` validated `title.trim()` and handed
the **untrimmed** string to the repository, so renaming a document to a
500-character title with a leading space stored 501 characters into
`VARCHAR(500)`: `22001 value too long`, surfaced as `500 {"error":"internal_error"}`
on a legal request. Creating trimmed and renaming untrimmed also stored the same
title two different ways.

`domain::text::Title` and `domain::text::Comment` exist so that cannot recur:
they can only be built by parsing, they carry the normalised form, and
`document_repo` and `version_repo` take **them** rather than `&str`. A value that
is validated and a value that is stored can only diverge while they are two
values — the same cure as `Pagination` above. Anything that adds a bounded text
field parses it into a type and hands the repository that type; it does not add
a third `if x.len() > N`. See `tests/text_bounds_test.rs`, which asks the
question in four alphabets.

## Two ceilings, and why they are not one number
`MAX_DOCUMENT_SIZE_MB` bounds the **document body as stored** — 2 MB by
default, and that number is sized against the instance rather than chosen (see
ADR-009 and the section below). The HTTP payload
that carries one is bounded separately and higher — `payload_ceiling()` in
`src/api/payload.rs` returns `2 × body + 64 KiB` — because JSON wraps the body
in quotes, escapes some of its characters (`"` → `\"`, `\` → `\\`) and puts the
title and the metadata beside it.

**Setting both to the same number, which is what `main.rs` did until
2026-09-14, makes the documented maximum unreachable.** Measured on the running
binary at `MAX_DOCUMENT_SIZE_MB=1`: a body of `ceiling - 64` was created (201),
a body of exactly the ceiling answered `413 text/plain`, and so did a body of
**half** the ceiling made of quote characters — the limit was being applied to
the encoding, so the largest storable document depended on which characters were
in it, a number no caller can compute. `DocumentService::validate_content` and
its `422` were unreachable from HTTP entirely; only the template path could
reach them.

Two refusals now, and they say different things: over the body ceiling is a
`422` naming the field and the limit; a request too long to read at all is a
`413`, in this service's own error shape rather than actix's `text/plain`.

`api::payload::limits()` installs both, and **`main.rs` and the tests call that
same function**. That is the load-bearing part. The ceiling used to be tested by
an `App` carrying no `JsonConfig` at all, against a service built with
`with_max_content_bytes(64)` — a bench from which the boundary that refuses
first does not exist. A limit tested at the layer that enforces it, from a
vantage point the caller never occupies, is green in the suite and wrong on the
wire. See `tests/size_limit_test.rs`.

## A third ceiling, on the index and not on the document
**Full-text search covers the first 250 000 characters of a document; the
document itself is stored whole, at any size `MAX_DOCUMENT_SIZE_MB` admits.**
A PostgreSQL `tsvector` cannot exceed 1 048 575 bytes of lexemes, and migration
006 indexed `title || ' ' || content` entire — so the index expression, which is
evaluated on every insert and update, decided whether a row could be **stored**.
What fills that budget is the *vocabulary* of the text, not its length: measured
on PostgreSQL 17, a body of **798 893 bytes** of distinct reference codes was
refused (`string is too long for tsvector`) while **10 050 000 bytes** of
ordinary repetitive prose went in. A pasted export or a generated appendix of
1 MB therefore answered `500 {"error":"internal_error"}` on a request that broke
no published rule, against a documented ceiling of 10 MB. The third limit in
three batches to be applied to a different quantity from the one its name
promises — after the payload/body split above and the byte/character bound
before it.

Migration 009 names the searchable projection once —
`editor.searchable_text(title, content)`, the first `INDEXED_PREFIX_CHARS`
characters of the title followed by the body — and builds the GIN index on it.
**`document_repo` sends that same function, through `search_predicate()`, and
that is load-bearing rather than tidy**: truncating only the index moves the
failure to the read, where a bitmap heap scan rechecks the condition on the heap
row and raises the identical error on `GET /documents?search=…` for every tenant
owning one large document. Anything that touches full-text search here goes
through that one function; it does not inline `title || ' ' || content` again.

250 000 is measured, not chosen: `left()` counts characters while the limit
counts bytes of lexemes, so the worst case (distinct accented tokens) was
measured at 576 628 bytes — half the limit — and 900 000 characters overflows.
See ADR-006 and `tests/search_index_test.rs`, which asks the question in three
alphabets, forces the sequential-scan plan the index would otherwise hide, and
fails if the code's bound and the migration's ever diverge.

## Reading a history: the list is a projection, the body-bearing read is one
**`GET /documents/{id}/versions` returns summaries and the history is NOT
paginated, so the projection is what bounds its cost — there is nothing else.**
`version_repo` names two column lists and they are not interchangeable:
`VERSION_SUMMARY_COLUMNS` (the list, and `insert_version`'s `RETURNING`) and
`VERSION_COLUMNS` (only `get_version`, whose purpose is to hand a prior body
back). The list returns `domain::document::DocumentVersionSummary`, which has no
`content` field at all — a body cannot leak into it by a forgotten column,
the same reason `Title` and `Pagination` are types.

Until 2026-09-14 the list selected `content` for every version and the handler
threw it away one layer up, while the published contract said — and still says —
*"Bodies are not included."* Measured on the running binary at the deployment's
own limits (`ops/cloudrun/doceditor.json`: **512 MiB**; `MAX_DOCUMENT_SIZE_MB`:
**10**), a document at the published ceiling with 55 versions — 54 content
PATCHes, an afternoon of autosaves — answered `GET …/versions` with an
**OOM kill of the process**, in 0.7 s, on a request whose answer is 9 423 bytes
of JSON. That is not a failed request: on Cloud Run it takes the *instance*
down, so every other tenant's in-flight request dies with it, and one caller
triggers it with two ordinary calls. After the split: `200`, 9 423 bytes, 55 ms,
6.3 → 7.1 MiB. Fourth batch running in which a cost was attached to a quantity
other than the one its name promised. See ADR-007 and
`tests/history_read_test.rs`, which asks PostgreSQL — two histories whose bodies
differ by four orders of magnitude must cost the same to list — rather than
trusting this paragraph.

### The three settings that only make sense together — one of them is now set
`MAX_DOCUMENT_SIZE_MB`, `memory: 512Mi` and the number of requests served at
once are one setting in three parts, and for months nothing related them. The
third part is the one nobody had written down: `ops/cloudrun/doceditor.json`
sets no `--concurrency`, so **Cloud Run's own default of 80 applies**, and that
is the number the deployment has to survive.

Measured on the running binary in a cgroup at the deployment's 512 MiB, N
concurrent full-size saves of **distinct** documents:

```text
10 MB, N=10 -> 200 ×10, peak 451 MiB      10 MB, N=12 -> oom-kill, MainPID=0
 4 MB, N=80 -> oom-kill, MainPID=0         3 MB, N=80 -> 200 ×80, peak 475 MiB
 2 MB, N=80 -> 200 ×80, peak 340 MiB
```

At 10 MB, **eleven ordinary saves killed the instance** — not the requests, the
instance, so every other tenant's in-flight request with it. HR-20260914-001
(option A, it@orbusdigital.com, executor named as *dev, in the doceditor
repository*) lowered the ceiling: **the default is 2 MB since 2026-09-14**, and
the number is the measurement's, not a taste — 3 MB survives 80 with 7% of the
memory to spare, which is not a margin, and 4 MB does not survive. See ADR-009.

Three drifts closed with it, because a ceiling stated in four places is a
ceiling that will be lowered in three. `DocumentService::new` carried its own
`10 * 1024 * 1024` beside `config.rs`'s own `"10"`; `main.rs` computed
`mb * 1024 * 1024` — non-saturating — one line before handing the result to a
function that saturates *and says why*; and `.parse().unwrap_or(10)` turned
`MAX_DOCUMENT_SIZE_MB=2MB` into 10 MB in silence. Now: one constant
(`config::DEFAULT_MAX_DOCUMENT_SIZE_MB`), one saturating conversion
(`AppConfig::max_document_bytes`), and a value this service cannot honour —
including `0` — **refuses the boot** instead of being replaced by a different
one. An operator who lowers a safety ceiling and is not obeyed learns nothing
until an instance dies.

`tests/document_ceiling_test.rs` holds the code's constant, `.env.example`, the
contract's number and the contract's *unit* against each other. The unit matters
as much as the number: `content` is bounded in **bytes** and therefore carries no
`maxLength`, which counts characters — the trap of the title bound, one field
over.

**Still open, and deliberately**: nothing in this repository bounds concurrency.
2 MB makes the platform's *default* concurrency safe; it does not make the
service safe at any concurrency. Options C (`--concurrency` on Cloud Run) and D
(an in-process admission limit) of the same human review remain available and
were not chosen — and C lives outside this repository. Documents already stored
above 2 MB are untouched: the bound is checked on bodies a caller *sends*, so an
existing 9 MB document stays readable, renamable and snapshottable.

## An update writes the columns it was given

**`PATCH` sends PostgreSQL only the columns the caller named; everything else
is `COALESCE($n, column)`, and that is load-bearing rather than tidy.** A column
bound to a parameter is a column PostgreSQL stores afresh, so binding the old
body back re-TOASTs it: new chunks, new write-ahead log, and the previous chunks
dead until autovacuum. Until 2026-09-15 `update_document` read the whole row
under its lock — body included — and wrote every column back, so **renaming a
document rewrote the document**. Measured on the running binary, on a body of
8 000 000 incompressible bytes: `PATCH {"title": …}` wrote **9 087 272 bytes of
WAL in 506 ms**, and the same rename written as a partial `UPDATE` wrote
**299 120 in 120 ms** — thirty times less. A status change and a metadata change
cost the same 9 MB. What remains at 299 kB is the index work every update here
owes and cannot avoid (`updated_at` is indexed, so nothing on this table is ever
a HOT update, and the GIN expression index of ADR-006 is re-evaluated); it is
identical before and after.

The same statement stopped *reading* the body too. `UPDATE_LOCK_COLUMNS` is
`status` — the transition to judge and the existence of a live row to answer
404, nothing else — where the locking `SELECT … FOR UPDATE` used to ask for
`DOC_COLUMNS`. Under contention that read was the visible cost: 20 concurrent
renames produced 19 sqlx *slow statement* warnings, the locking read taking up
to **10.0 s** because each waiter detoasted and shipped 8 MB before deciding
anything. **The lock itself has not moved** — same row, same order as
`version_repo::create_version`, ADR-004 untouched; only its projection changed.
`current_version` is now advanced by PostgreSQL inside that statement and
`insert_version` takes the number from the `RETURNING`, so there is no longer a
second spelling of "the next version" anywhere.

The hypothesis measurement refused: this was **not** an instance killer. Forty
concurrent renames of that 8 MB document in a 512 MiB cgroup answered `200`
forty times before (peak 384 MiB) and after (238 MiB) — the repair buys headroom
and log volume, not a rescue. Said out loud because a benefit overstated is how
the next reader inherits a false premise. See ADR-014 and
`tests/update_cost_test.rs`, which asks PostgreSQL 17's
`pg_column_toast_chunk_id` whether the body moved rather than trusting this
paragraph — a per-row fact, immune to what the rest of the suite writes in
parallel, unlike a WAL delta.

## Content and versions
A document carries a `content` body (HTML/Markdown/JSON — the product brings its
own editor, DocEditor owns the storage and the history). Creation writes
**version 1**; every change to `content` advances `current_version` and writes
the matching immutable version row in the same transaction. `is_auto` is `true`
for the snapshots the service takes itself, `false` for an explicit
`POST .../versions`. `yjs_state`/`yjs_snapshot` remain reserved for the
collaborative CRDT layer, which is not built — do not store text in them.

**Both write paths take `FOR UPDATE` on the document row before reading
`current_version`, and that lock is not optional.** The next version number is
computed in Rust between two statements; without the lock two concurrent saves
claim the same number and the `UNIQUE (document_id, version)` of migration 003
turns the loser into a 500, while a save racing an explicit snapshot *deadlocks*
(the two paths write the same two tables in opposite orders). Any new path that
advances `current_version` takes the same lock on the same row first. See
ADR-004 and `tests/concurrency_test.rs`. Content is last-write-wins; what is
guaranteed is that no edit leaves the history and no request fails.

**A soft-delete hides the document AND its whole history, on every read path.**
`deleted_at IS NULL` is not a per-query detail: the history reads go through
`version_repo::ensure_live_document`, and any new read of a document or of its
versions calls it too. This is a single chokepoint on purpose. Until
2026-09-14 the predicate was written inline in `list_versions` and simply
forgotten in `get_version` forty lines below, so a deleted document answered
404 on `GET /documents/{id}` and on `GET /documents/{id}/versions` while
serving its **full body** on `GET /documents/{id}/versions/{n}` — the only one
of the three that returns content, and reachable by counting from 1. Tenant
isolation was never involved; what leaked is a document the caller's own tenant
had deleted. See `tests/deletion_test.rs`, which asks the question of every
read path rather than of the one route someone thought about.

**Open, not decided: the history is unpaginated.** The contract returns every
version; ten thousand of them now answer with ~1.5 MB of summaries instead of
gigabytes of bodies, which is survivable where the old behaviour was not, but it
is still unbounded. A default page size would silently truncate history for
existing clients — a product decision, not a repair.

**Open, not decided: `archived` freezes the status but not the body.** Status
transitions are terminal at `archived` (`can_transition_to`), yet a PATCH
carrying only `content` never enters the transition check, so an archived
document can still be edited and still advances its version. Whether
`archived` should freeze content is a product question with no spec to answer
it — do not "fix" it by guessing; it is reported, not resolved.

## Multi-Tenancy
- Auth via JWT Bearer token (RS256 production, HS256 dev/test)
- AuthUser extractor validates JWT and extracts tenant_id + user_id from claims
- The tenant comes from the JWT claim, **never** from a body field or a header.
  `X-Tenant-Id` is read for logging only.
- All DB queries use `begin_tenant_tx` which sets `app.tenant_id` for RLS
- All queries include explicit `tenant_id` filter (defense-in-depth)
- RLS is `ENABLE`d **and** `FORCE`d on every table (migration 007); policies use
  the missing_ok form of `current_setting` and carry a `WITH CHECK`.
- **A SUPERUSER or BYPASSRLS role bypasses all of that — so the service does not
  run as one.** `ods` (the role in `DATABASE_URL` here and on CI) is both, and
  for seven review cycles that made the policies decorative. It no longer does:
  PostgreSQL evaluates policies against the **effective** role, so migration 008
  provisions `editor_app` (NOLOGIN, unprivileged) and the serving pool runs
  `SET ROLE editor_app` on **every connection**. Boot says which posture is live:
  `INFO … enforced … role=editor_app session_role=ods`. Measured: an unscoped
  `SELECT` sees 1278 rows as `ods` and **0** as `editor_app`. See ADR-005.
- Two pools, and the split is not cosmetic. The **boot** pool keeps the
  connection string's privileges — it creates the schema, runs the migrations,
  probes the role — then closes. The **serving** pool drops into `editor_app`.
  Anything administrative belongs on the first; a request never does.
- `tests/common::setup_test_pool` is wired like the serving pool, runtime role
  included, so the whole suite runs under enforced RLS. Use `setup_admin_pool`
  for DDL and for fixtures that model an operator. Seeding a **platform**
  template (`tenant_id IS NULL`) is refused by the `WITH CHECK` on purpose and
  escalates with `SET LOCAL ROLE NONE`; a tenant's own template is written under
  its own context.
- Pointing `DATABASE_URL` at a plain LOGIN role that is neither SUPERUSER nor
  BYPASSRLS remains **better** — it removes the privileges from the connection
  string, so nothing can `RESET ROLE` back to them. It is no longer a
  prerequisite for the control to exist.
- All events include tenant_id, and the correlation id of their request.

## Observability
- Structured JSON logs; `LOG_LEVEL` is applied (`RUST_LOG` takes precedence).
- `X-Correlation-Id` is adopted or minted by the `correlate` middleware, echoed
  on the response, put in the tracing span and attached to every event.

## Database
```
PostgreSQL 17 — ods-postgres container
Host: 127.0.0.1:5435          <-- 5435, NOT 5433
User: ods / Password: ods-dev-2026 / DB: ods
Schema: editor
Tables: documents, document_versions, templates
Connection: postgres://ods:ods-dev-2026@127.0.0.1:5435/ods?options=-c%20search_path%3Deditor%2Cpublic
```

On this host **5433 is another project's container**. It answers and then
rejects the `ods` role with `28P01`, so pointing at it makes a configuration
mistake look like broken code. Trust `docker ps`, not an older copy of this doc.

The instance is **shared** with a dozen other ODS schemas. Two consequences that
have each cost a work unit:
- `search_path` must be pinned in the DSN **and** the `editor` schema must exist
  before `migrate!` runs, or `_sqlx_migrations` lands in `public` and the suite
  dies on `VersionMissing(N)` from another service's history. Both the binary
  and `tests/common/` do this; read `VersionMissing` as "shared migration
  table", never as a corrupt migration.
- Anything checking `pg_policies`/`pg_class` must filter on `schemaname`.
  Migration 005 did not, matched `securemail.templates`, and silently skipped
  creating the policy on `editor.templates`.

## Migrations
All migrations are idempotent (CREATE TABLE IF NOT EXISTS, CREATE INDEX IF NOT EXISTS).
This is required because the dev DB is shared across services.

**`sqlx::migrate!` embeds the directory at COMPILE time, and adding a new file
does not by itself invalidate the build.** A brand-new migration therefore looks
like it "did not run": the binary still carries the previous set, `_sqlx_migrations`
stops at the old version, and the tests that need it fail while the SQL on disk
is perfectly correct. Touch a file that uses the macro (`src/main.rs`,
`tests/common/mod.rs`) — or `cargo clean -p ods-doceditor` — before concluding
anything about a migration you just wrote. Cost the first 20 minutes of the
migration-008 batch; CI never sees it because CI always builds from scratch.

Migration 008 also creates a **role**, which is cluster-wide while sqlx's
migration lock is per-database — so its `IF NOT EXISTS` catches `duplicate_object`
as well, the same race `CREATE SCHEMA IF NOT EXISTS` has (see
`src/repository/schema.rs`). Every step of it also catches `insufficient_privilege`
and downgrades to a WARNING: a database whose admin role lacks `CREATEROLE` must
still migrate and still boot.

## Tests
```bash
# 5435. The line above this block spent months saying 5433, which is another
# project's container on this host — the trap the Database section describes.
export DATABASE_URL="postgres://ods:ods-dev-2026@127.0.0.1:5435/ods?options=-c%20search_path%3Deditor%2Cpublic"
export JWT_ALLOW_HS256=true JWT_SECRET=test-secret-for-doceditor-jwt-validation-only
export REDPANDA_BROKERS=127.0.0.1:19092
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

`tests/events_roundtrip.rs` needs a **real broker** and does not skip without
one: this service published four months of CloudEvents into a `NoopProducer`
and nothing failed, so "the producer returned Ok" is not evidence.

The broker is a **standing container on the dev host**, not one you start and
remove around a run:

```bash
docker run -d --name doceditor-redpanda-dev --restart unless-stopped \
  -p 127.0.0.1:19092:19092 \
  docker.redpanda.com/redpandadata/redpanda:v24.2.7 \
  redpanda start --smp 1 --overprovisioned --node-id 0 --check=false \
    --mode dev-container --kafka-addr PLAINTEXT://0.0.0.0:19092 \
    --advertise-kafka-addr PLAINTEXT://127.0.0.1:19092
# ready check
docker exec doceditor-redpanda-dev rpk cluster info --brokers 127.0.0.1:19092
```

**Why standing, measured on 2026-09-13.** Three runners run this suite and only
two of them bring a broker. CI starts its own per run (`.github/workflows/ci.yml`,
`docker run`, then sets `REDPANDA_BROKERS`); a developer types the line above.
The **ADLC pipeline** (`~/dev/ops/adlc-v2/scripts/test-runner.sh`) does neither:
it runs `cargo test --all` on this host with no environment, and it provisions
Postgres (`lib/db-schema-guard.sh`) but nothing for the bus. With no broker up,
`doceditor-test.log` showed 70 green and 3 red in 15 s and the service was
declared FAIL for an infrastructure gap, not a code one. Removing the container
after a local run re-creates that, so it stays up — it is the same class of
prerequisite as the `ods-postgres` container on 5435.

`REDPANDA_BROKERS` overrides the address; the fallback in `tests/common/mod.rs`
is what the pipeline and a bare `cargo test` land on, and the failure it raises
now carries the `docker run` line itself (`tests/events_roundtrip.rs`).

**A standing container and a test that never cleaned up compound, and they did.**
This broker allocates one file descriptor per partition and refuses every create
past its FD ceiling — 204 under `--smp 1 --overprovisioned`. `events_roundtrip.rs`
created three uniquely-named topics per run and deleted none, so roughly 65 runs
were enough to turn the suite red for a reason that is in no diff: `InvalidPartitions`
on three of four tests, indistinguishable from a code regression. 186 leftovers
had to be pruned by hand on 2026-09-14, and that cost a review cycle. Two halves
now, because either alone leaks: a `RoundTripTopic` guard deletes its topic on
`Drop` — so a *failing* run, which is the one a reviewer repeats, costs the broker
nothing — and every run first sweeps `doceditor-roundtrip-*` topics older than an
hour, which is what survives a `kill -9`. The sweep is the reason a saturated
broker heals on the next run instead of staying red until someone re-derives why;
it matches on the prefix and the timestamp only, because `editor-events` and the
cluster's internals live on that same broker. Measured: 9 leftovers → 0, 7/7
green, and a run now ends with exactly as many topics as it started with.

## Environment Variables
`.env.example` lists exactly what `src/config.rs` reads — keep the two in step.
- `DATABASE_URL` (required)
- `SERVER_HOST` (default: 0.0.0.0), `SERVER_PORT` (default: 8087)
- `LOG_LEVEL` (default: info; `RUST_LOG` wins when set)
- `MAX_DOCUMENT_SIZE_MB` (default: **2** since HR-20260914-001 — the **body**,
  in bytes; the payload ceiling is derived from it, and a malformed or zero
  value now refuses the boot rather than falling back)
- `EVENT_BUS` (`pubsub` | `redpanda`/`kafka` | `none`; **unset keeps the
  historical behaviour**. A transport named but not usable **refuses the boot**)
- `GCP_PROJECT_ID`, `PUBSUB_TOPIC` (`EVENT_BUS=pubsub`; topic defaults to
  `editor-events`). `PUBSUB_EMULATOR_HOST` and `GCE_METADATA_HOST` override the
  Pub/Sub and metadata origins — Google's own names. `PUBSUB_TOPIC_DLQ` is set
  on the deployed revision and deliberately **not read**
- `REDPANDA_BROKERS` (**unset means events are dropped**, logged at WARN)
- `REDPANDA_TOPIC` (default: **editor-events** since spec.md §4.2)
- `JWT_RSA_PUBLIC_KEY_B64` (base64-encoded RSA PEM, production)
- `JWT_ALLOW_HS256` (true/false, dev only)
- `JWT_SECRET` (required when JWT_ALLOW_HS256=true)
- `JWT_ISSUER`, `JWT_AUDIENCE` (optional)

### The topic name, settled — and the transport, still not
**The canonical topic is `editor-events`, with `editor-events-dlq` beside it.**
It had four spellings across four sources and exactly one of them exists as a
provisioned resource: measured 2026-09-14 08:28 UTC among the 25 topics of
`orbus-ods-staging`, `editor-events` is there and `editor.events` (this file and
the old default), `ods.editor.events` (GTM brief, four times) and
`doceditor-events` (the `{service}-events` rule) are not. It is also that rule
applied to the name this service already carries everywhere: schema `editor`,
event source `/editor`, types `com.ods.editor.*`. `doceditor` names the
deployment; `editor` names the domain, and the domain is what consumers read.

Settled by `~/dev/specs/ods-platform/specs/doceditor/spec.md` §4.2 executing
HR-20260913-001, and applied here on 2026-09-14: `src/config.rs` defaults to it,
`.env.example` sets it, ADR-002 records it, `tests/event_topic_test.rs` guards
it. **That guard is the whole point** — publishing to the wrong topic returns
`Ok` and fails nothing, which is how this service published four months of
events into a `NoopProducer`.

### And the transport, settled on 2026-09-14: **Cloud Pub/Sub**

**The deployment had already chosen it; the code had never learned.** The live
Cloud Run revision (`doceditor-00003-vkq`, May 2026) carries `EVENT_BUS=pubsub`,
`PUBSUB_TOPIC=editor-events`, `PUBSUB_TOPIC_DLQ` and `GCP_PROJECT_ID` — four
variables no line of `src/config.rs` read, while the code published with
`rdkafka` into a staging project that has no Redpanda broker. So the
`NoopProducer` was selected and four months of correct CloudEvents were thrown
away, in the silence ADR-002 describes. Settled by **HR-20260914-007** (option
A, 2026-09-14, it@orbusdigital.com, *"Exécutant : dev, dans le dépôt
doceditor"*); recorded in ADR-011.

`config::select_event_bus` chooses once, from the environment, and **refuses the
boot rather than falling back**: `EVENT_BUS=pubsub` without `GCP_PROJECT_ID`,
`EVENT_BUS=redpanda` without `REDPANDA_BROKERS`, or a word naming no transport,
all stop the process naming the variable to set. That is deliberately stricter
than elsewhere. A mis-set ceiling eventually kills an instance and leaves a
trace; **a mis-set bus produces nothing to notice at all**, and refusing to
start is the only variant of "something is wrong" a silent bus can be turned
into. `EVENT_BUS` unset keeps the historical behaviour exactly: a broker address
selects Redpanda, its absence selects nothing, loudly.

**REST and not gRPC**, and that is a dependency decision rather than a style:
every gRPC client for Pub/Sub pulls `tonic` and therefore `h2`, which this
estate is under a platform decision to keep out of its shipped graph
(HR-20260909-001). `reqwest` is therefore in `[dependencies]` with
`default-features = false` and no `http2` — measured after the change,
`cargo tree -e normal` holds 675 crates and **h2 is in none of them**, and
`cargo audit` still reports exactly the one pre-existing `rsa` advisory
(BR-0010). Do not "simplify" this by taking reqwest's defaults.

**Credentials are not in this repository and never will be**: on Cloud Run the
runtime service account's token comes from the instance metadata server, and
`runtime-cloud-run@orbus-ods-staging` already holds `roles/pubsub.publisher`
(measured 2026-09-14) — which is why this option needed no infrastructure
change. `PUBSUB_EMULATOR_HOST` and `GCE_METADATA_HOST` override the two origins;
both names are Google's, not ours.

**One envelope, two bindings.** `cloudevent_attributes()` names the CloudEvents
attributes once; Kafka prefixes them `ce_`, Pub/Sub `ce-`. The hyphen is the
HTTP binding's and it is load-bearing: the platform delivers by push
subscription to Cloud Run, and a subscription with payload unwrapping turns each
attribute into an HTTP header verbatim, so `ce-type` is the header a standard
CloudEvents consumer reads. Anything that adds an attribute adds it to that one
function; `tests/pubsub_transport_test.rs` holds the two bindings against each
other, and talks to a **socket** rather than to `src/` — a test that compares
two sources cannot see what goes on the wire.

**`PUBSUB_TOPIC_DLQ` is read by nothing, on purpose**: a dead-letter topic is a
property of a *subscription*, not of a publisher.

**Still open, and outside this repository**: `~/dev/ops/cloudrun/doceditor.json`
lists none of the four variables the live revision carries — the descriptor has
drifted from the revision — and the deployed image must be rebuilt for this code
to be the code that runs.
