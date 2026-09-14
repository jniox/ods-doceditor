use actix_web::body::MessageBody;
use actix_web::dev::{ServiceRequest, ServiceResponse};
use actix_web::http::header::{HeaderName, HeaderValue};
use actix_web::middleware::Next;
use actix_web::{dev::Payload, Error, FromRequest, HttpMessage, HttpRequest};
use std::future::{ready, Ready};
use std::time::Instant;
use tracing::Instrument;
use uuid::Uuid;

use crate::correlation::{self, CORRELATION_ID_HEADER, SOURCE_SERVICE_HEADER, TENANT_ID_HEADER};

/// What the caller told us about itself.
#[derive(Debug, Clone)]
pub struct RequestContext {
    /// Always present: supplied by the caller, or minted here.
    pub correlation_id: String,
    /// The calling service, when it named itself.
    pub source_service: Option<String>,
}

impl FromRequest for RequestContext {
    type Error = Error;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _payload: &mut Payload) -> Self::Future {
        // Falls back to a fresh id rather than failing: a handler reached
        // without the middleware is a wiring mistake, not a client error.
        ready(Ok(req
            .extensions()
            .get::<RequestContext>()
            .cloned()
            .unwrap_or_else(|| RequestContext {
                correlation_id: Uuid::new_v4().to_string(),
                source_service: None,
            })))
    }
}

/// Adopt the caller's correlation id (or mint one), expose it to handlers, to
/// the logs and to the events, and echo it back on the response.
///
/// Echoing matters as much as reading: the caller can only join its own logs to
/// ours if it learns which id we used when it did not supply one.
pub async fn correlate(
    req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, Error> {
    let header = |name: &str| {
        req.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };

    let correlation_id =
        header(CORRELATION_ID_HEADER).unwrap_or_else(|| Uuid::new_v4().to_string());
    let source_service = header(SOURCE_SERVICE_HEADER);

    req.extensions_mut().insert(RequestContext {
        correlation_id: correlation_id.clone(),
        source_service: source_service.clone(),
    });

    let span = tracing::info_span!(
        "http_request",
        correlation_id = %correlation_id,
        source_service = source_service.as_deref().unwrap_or("-"),
        // Logged for traceability only. The tenant acted upon is the JWT claim.
        tenant_id_header = header(TENANT_ID_HEADER).as_deref().unwrap_or("-"),
        method = %req.method(),
        path = %req.path(),
    );
    let echoed = correlation_id.clone();
    let started = Instant::now();

    // `.instrument()` and not `span.enter()`: a guard held across an `.await`
    // stays active on the thread while the task is parked, which attaches the
    // span to whatever unrelated work the executor runs next. Instrumenting the
    // future makes the span follow the task instead of the thread.
    let mut res = correlation::scope(correlation_id, next.call(req))
        .instrument(span.clone())
        .await?;

    // One access log per request, inside the span, so the correlation id is
    // actually in the logs. A span under which no event is emitted records
    // nothing: before this line the correlation id reached the response header
    // and the events, and nothing else.
    span.in_scope(|| {
        tracing::info!(
            status = res.status().as_u16(),
            duration_ms = started.elapsed().as_millis() as u64,
            "request completed"
        )
    });

    if let Ok(value) = HeaderValue::from_str(&echoed) {
        res.headers_mut()
            .insert(HeaderName::from_static("x-correlation-id"), value);
    }

    Ok(res)
}
