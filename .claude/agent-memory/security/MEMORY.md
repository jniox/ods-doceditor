# Security Agent Memory — doceditor + oid cross-service notes

## Reviews Conducted
- v1 (c0ec8cc): FAIL, severity=HIGH. CRITICAL: no JWT validation (header forgery). No body size limit. LIMIT/OFFSET SQL interpolation.
- v2 (c0ec8cc): FAIL, severity=HIGH. Same issues confirmed.
- v3 (01a9c33, 827c97b): PASS (concerns), severity=MEDIUM. All CRITICAL issues fixed.

## Current Debt (v3)
- DT-007 MEDIUM: actix-cors in Cargo.toml but .wrap(Cors::...) never called in main.rs
- DT-008 MEDIUM: tracing-actix-web in Cargo.toml but TracingLogger not wired — no correlation ID
- DT-009 LOW: .gitignore missing *.pem, *.key, *.cert
- DT-005 MEDIUM: no RBAC — any tenant user can delete/publish (unchanged from v2)

## Key Architecture Notes
- JWT: RS256 (JWT_RSA_PUBLIC_KEY_B64) preferred; HS256 gated via JWT_ALLOW_HS256=true
- Tenant isolation: begin_tenant_tx (RLS) + explicit tenant_id filter on every WHERE clause
- Body limits: JsonConfig + PayloadConfig wired in main.rs using max_document_size_mb
- All SQL uses sqlx parameterized queries (.bind()) — no format! interpolation in queries
- RLS policies in migration 005_enable_rls.sql cover documents, document_versions, templates
- TEST_SECRET in extractors.rs is cfg(test) only — safe

## Pattern: Dependency Declared But Never Wired
actix-cors and tracing-actix-web are in Cargo.toml but absent from the App builder.
Check for this pattern in future reviews: grep for .wrap( in main.rs vs declared middleware deps.
