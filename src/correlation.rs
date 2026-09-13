//! Request correlation, carried implicitly.
//!
//! `X-Correlation-Id` is cross-cutting: it concerns the HTTP edge, the logs and
//! the events, and threading it as an argument through every service and
//! repository signature would touch code that has no business knowing about it.
//! It lives in a task-local instead, set once by the middleware for the
//! duration of the request — the same shape `tracing` uses for spans.
//!
//! Outside a request there is no value, and none is invented: a background job
//! must not look like somebody's HTTP call.

/// Inbound/outbound header carrying the trace identifier across ODS services.
pub const CORRELATION_ID_HEADER: &str = "X-Correlation-Id";

/// Inbound header naming the calling service.
pub const SOURCE_SERVICE_HEADER: &str = "X-Source-Service";

/// Inbound header carrying the tenant, for logging and cross-checking only.
///
/// The tenant the service ACTS on always comes from the validated JWT claim,
/// never from this header — a caller must not be able to change tenant by
/// setting a header.
pub const TENANT_ID_HEADER: &str = "X-Tenant-Id";

tokio::task_local! {
    static CORRELATION_ID: String;
}

/// Run `future` with `correlation_id` attached to the current task.
pub async fn scope<F>(correlation_id: String, future: F) -> F::Output
where
    F: std::future::Future,
{
    CORRELATION_ID.scope(correlation_id, future).await
}

/// The correlation id of the request being served, if there is one.
pub fn current() -> Option<String> {
    CORRELATION_ID.try_with(|id| id.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn there_is_no_correlation_id_outside_a_scope() {
        assert_eq!(current(), None);
    }

    #[tokio::test]
    async fn the_scope_makes_the_id_readable_without_passing_it_around() {
        let seen = scope("corr-7".to_string(), async { current() }).await;
        assert_eq!(seen.as_deref(), Some("corr-7"));
        assert_eq!(current(), None, "the scope must not outlive the request");
    }
}
