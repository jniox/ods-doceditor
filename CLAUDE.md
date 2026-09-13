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
- Domain models: `src/domain/` (Document, DocumentVersion, DocumentStatus)
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

## Multi-Tenancy
- Auth via JWT Bearer token (RS256 production, HS256 dev/test)
- AuthUser extractor validates JWT and extracts tenant_id + user_id from claims
- The tenant comes from the JWT claim, **never** from a body field or a header.
  `X-Tenant-Id` is read for logging only.
- All DB queries use `begin_tenant_tx` which sets `app.tenant_id` for RLS
- All queries include explicit `tenant_id` filter (defense-in-depth)
- RLS is `ENABLE`d **and** `FORCE`d on every table (migration 007); policies use
  the missing_ok form of `current_setting` and carry a `WITH CHECK`.
- **A SUPERUSER or BYPASSRLS role still bypasses all of it.** `ods` on the dev
  instance is both, so RLS is inert there and isolation rests on the tenant
  predicates. The service measures this at startup and logs a WARN naming the
  role. Deploy with a non-superuser role without BYPASSRLS.
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

## Environment Variables
`.env.example` lists exactly what `src/config.rs` reads — keep the two in step.
- `DATABASE_URL` (required)
- `SERVER_HOST` (default: 0.0.0.0), `SERVER_PORT` (default: 8087)
- `LOG_LEVEL` (default: info; `RUST_LOG` wins when set)
- `MAX_DOCUMENT_SIZE_MB` (default: 10 — bounds the payload and the body)
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
