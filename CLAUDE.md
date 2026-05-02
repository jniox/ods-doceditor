# doceditor

## Stack
Rust (Actix-web 4, sqlx 0.8, rdkafka 0.36)

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
- `POST /api/v1/documents` — create document
- `GET /api/v1/documents` — list documents (paginated, filterable)
- `GET /api/v1/documents/{id}` — get document
- `PATCH /api/v1/documents/{id}` — update document (title, status, metadata)
- `DELETE /api/v1/documents/{id}` — soft-delete document
- `POST /api/v1/documents/{id}/versions` �� create version snapshot
- `GET /api/v1/documents/{id}/versions` — list versions
- `GET /api/v1/documents/{id}/versions/{version}` — get specific version

## Multi-Tenancy
- Auth via JWT Bearer token (RS256 production, HS256 dev/test)
- AuthUser extractor validates JWT and extracts tenant_id + user_id from claims
- All DB queries use `begin_tenant_tx` which sets `app.tenant_id` for RLS
- All queries include explicit `tenant_id` filter (defense-in-depth)
- RLS policies on all tables enforce tenant isolation
- All events include tenant_id

## Database
```
PostgreSQL 17 — ods-postgres container
Host: 127.0.0.1:5433
User: ods / Password: ods-dev-2026 / DB: ods
Schema: editor
Tables: documents, document_versions, templates
Connection: postgres://ods:ods-dev-2026@127.0.0.1:5433/ods
```

## Migrations
All migrations are idempotent (CREATE TABLE IF NOT EXISTS, CREATE INDEX IF NOT EXISTS).
This is required because the dev DB is shared across services.

## Tests
```bash
DATABASE_URL="postgres://ods:ods-dev-2026@127.0.0.1:5433/ods" JWT_ALLOW_HS256=true JWT_SECRET=test-secret-for-doceditor-jwt-validation-only cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

## Environment Variables
- `DATABASE_URL` (required)
- `SERVER_PORT` (default: 8087)
- `REDPANDA_BROKERS` (default: localhost:9092)
- `REDPANDA_TOPIC` (default: editor.events)
- `MAX_DOCUMENT_SIZE_MB` (default: 10)
- `JWT_RSA_PUBLIC_KEY_B64` (base64-encoded RSA PEM, production)
- `JWT_ALLOW_HS256` (true/false, dev only)
- `JWT_SECRET` (required when JWT_ALLOW_HS256=true)
- `JWT_ISSUER` (optional)
- `JWT_AUDIENCE` (optional)
