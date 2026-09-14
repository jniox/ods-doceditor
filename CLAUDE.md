# doceditor

## Stack
Rust (Actix-web 4, sqlx 0.8, rdkafka 0.36)

`actix-web` is declared **twice** (dependencies + dev-dependencies) and both
entries must keep `default-features = false` without `http2`: the dev entry
alone is enough to pull `h2` 0.3 back into `Cargo.lock`, which is the file
`cargo audit` reads. See HR-20260909-001 and `tests/framework.rs`.

## Project
ods-platform

## Architecture
- Domain models: `src/domain/` (Document, DocumentVersion, DocumentStatus, and the
  parsed values the boundary builds: `pagination::Pagination`, `text::Title`,
  `text::Comment`, `metadata::Metadata`)
- Service layer: `src/service/` (DocumentService — business logic, orchestrates repo + events)
- API handlers: `src/api/` (HTTP handlers, auth extractor, health)
- Repository: `src/repository/` (PostgreSQL via sqlx, RLS via tenant_context)
- Events: `src/events/` (CloudEvents v1.0 via Redpanda/Kafka)
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
`MAX_DOCUMENT_SIZE_MB` bounds the **document body as stored**. The HTTP payload
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

### Measured and NOT fixed: the three settings that only make sense together
`MAX_DOCUMENT_SIZE_MB=10`, `memory: 512Mi` and an unbounded number of concurrent
requests are related, and nothing relates them. Measured, with the fix in place:
**6 concurrent full-size content saves survive (peak 418 MiB); 10 kill the
instance.** The `RETURNING` half of the fix is real but does not move that
threshold (433 → 418 MiB at 6 concurrent; 10 is fatal either way) — the dominant
cost there is the buffered request payload plus its parsed `String`, not the row
that comes back, and this repo does not claim otherwise. Raising the body
ceiling to 50 MB would make **two** simultaneous saves fatal. The remedies — a
smaller ceiling, more instance memory, a Cloud Run concurrency bound, an
in-process admission limit — are a sizing decision with no spec, and
`ops/cloudrun/doceditor.json` is outside this repository. Reported, not guessed.

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
it matches on the prefix and the timestamp only, because `editor.events` and the
cluster's internals live on that same broker. Measured: 9 leftovers → 0, 7/7
green, and a run now ends with exactly as many topics as it started with.

## Environment Variables
`.env.example` lists exactly what `src/config.rs` reads — keep the two in step.
- `DATABASE_URL` (required)
- `SERVER_HOST` (default: 0.0.0.0), `SERVER_PORT` (default: 8087)
- `LOG_LEVEL` (default: info; `RUST_LOG` wins when set)
- `MAX_DOCUMENT_SIZE_MB` (default: 10 — the **body**; the payload is derived)
- `REDPANDA_BROKERS` (**unset means events are dropped**, logged at WARN)
- `REDPANDA_TOPIC` (default: editor.events)
- `JWT_RSA_PUBLIC_KEY_B64` (base64-encoded RSA PEM, production)
- `JWT_ALLOW_HS256` (true/false, dev only)
- `JWT_SECRET` (required when JWT_ALLOW_HS256=true)
- `JWT_ISSUER`, `JWT_AUDIENCE` (optional)

### Open question, not to be guessed
The topic name has three values across the sources of truth: `editor.events`
(this file and the current default), `ods.editor.events` (GTM brief, named four
times as what products subscribe to) and `{service}-events` (global platform
rule). Publishing to the wrong one is silent. Escalated, not decided here.
