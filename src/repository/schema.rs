//! Creating the service's schema when several processes start at once.
//!
//! `CREATE SCHEMA IF NOT EXISTS` is not safe against itself. `IF NOT EXISTS`
//! is a look followed by an insert, and the two are not one atomic step: two
//! connections both look, both find nothing, both insert, and the loser gets
//!
//!   duplicate key value violates unique constraint "pg_namespace_nspname_index"
//!
//! which is an error, not the no-op the clause suggests.
//!
//! It never happens on a development machine, and that is what makes it
//! expensive: on the shared `ods-postgres` instance `editor` has existed for
//! months, so the existence check short-circuits before any race can occur. It
//! needs a *fresh* database — which is what a CI service container is, and what
//! a first deployment is. It was found on 2026-09-13 by the first CI run in
//! this repository's history that ever reached the tests, where it killed three
//! of them; the identical statement in `src/main.rs` would have crash-looped
//! two Cloud Run instances starting together against an empty database.
//!
//! Serialising the racers is not enough on its own, which is worth stating
//! because an advisory lock is the tempting fix: migration 001 also runs
//! `CREATE SCHEMA IF NOT EXISTS editor`, under sqlx's *own* migration lock, so
//! a process in its migrations and a process in its startup hold two different
//! locks and can still collide. What holds whatever the racers are doing is
//! accepting that someone else won.

use crate::error::{AppError, AppResult};
use sqlx::{Executor, PgPool};

/// `unique_violation`, raised on `pg_namespace_nspname_index` when another
/// connection inserted the same schema first.
const UNIQUE_VIOLATION: &str = "23505";
/// `duplicate_schema`, the error a `CREATE SCHEMA` without `IF NOT EXISTS`
/// raises. Accepted too, so the tolerance does not depend on which of the two
/// PostgreSQL happens to pick.
const DUPLICATE_SCHEMA: &str = "42P06";

/// Ensure `schema` exists, whoever else is trying to create it right now.
///
/// Losing the race is a success: the postcondition is that the schema exists,
/// not that this connection is the one that created it. The loss is confirmed
/// rather than assumed — the schema is read back from the catalog — so this
/// stays a check of the postcondition and does not become an error swallowed
/// on the strength of its SQLSTATE.
pub async fn ensure_schema_exists(pool: &PgPool, schema: &str) -> AppResult<()> {
    if !is_plain_identifier(schema) {
        return Err(AppError::Internal(format!(
            "refusing to build a CREATE SCHEMA statement from {schema:?}"
        )));
    }

    match pool
        .execute(format!("CREATE SCHEMA IF NOT EXISTS {schema}").as_str())
        .await
    {
        Ok(_) => Ok(()),
        Err(sqlx::Error::Database(e))
            if matches!(
                e.code().as_deref(),
                Some(UNIQUE_VIOLATION | DUPLICATE_SCHEMA)
            ) =>
        {
            if schema_exists(pool, schema).await? {
                tracing::debug!(schema, "Schema was created concurrently by another process");
                Ok(())
            } else {
                Err(AppError::Internal(format!(
                    "Creating the schema {schema} reported {} but the schema is absent: {e}",
                    e.code().as_deref().unwrap_or("?")
                )))
            }
        }
        Err(e) => Err(AppError::Internal(format!(
            "Failed to create the schema {schema}: {e}"
        ))),
    }
}

async fn schema_exists(pool: &PgPool, schema: &str) -> AppResult<bool> {
    sqlx::query("SELECT 1 FROM pg_namespace WHERE nspname = $1")
        .bind(schema)
        .fetch_optional(pool)
        .await
        .map(|row| row.is_some())
        .map_err(|e| AppError::Internal(format!("Failed to read the schema {schema}: {e}")))
}

/// A lowercase, unquoted PostgreSQL identifier, and nothing else.
///
/// An identifier cannot be a bind parameter, so the name is interpolated into
/// the statement and therefore has to be checked rather than trusted.
fn is_plain_identifier(name: &str) -> bool {
    !name.is_empty()
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::is_plain_identifier;

    #[test]
    fn the_service_schema_is_a_plain_identifier() {
        assert!(is_plain_identifier("editor"));
        assert!(is_plain_identifier("_tmp_doceditor_20260913_x1"));
    }

    #[test]
    fn anything_that_could_end_a_statement_is_not() {
        for hostile in [
            "editor; DROP SCHEMA public CASCADE",
            "editor\"",
            "editor'",
            "editor public",
            "editor--",
            "Editor",
            "1editor",
            "",
        ] {
            assert!(
                !is_plain_identifier(hostile),
                "{hostile:?} must not be accepted as a schema name"
            );
        }
    }
}
