# ADR-002: Events are published for real, and their absence is loud

**Date**: 2026-09-13  **Status**: accepted

## Context

The platform rule is Event-First: every state change emits a CloudEvents v1.0
message carrying `tenant_id`. DocEditor had the shape of that — a `CloudEvent`
type, an `EventProducer` trait, call sites in the service layer — and a single
implementation, `NoopProducer`, which logged at `debug` and returned `Ok(())`.

Every document event this service had ever produced was discarded, silently, for
the whole life of the service. Nothing failed: the trait returned success, the
call sites checked it, the tests asserted that `publish` had been called. The
only way to notice was to look for the events downstream, and no consumer
existed yet to miss them.

That is the property worth naming: **a dropped event has no failure signature**.
It is not like a dropped HTTP request. It has to be made visible by design,
because nothing will report it.

## Decision

**1. `RedpandaProducer` is the implementation, and it publishes.** rdkafka
`FutureProducer`, CloudEvents v1.0 in **binary content mode** — the envelope
travels in `ce_*` message headers and the payload is the event data — which is
what lets a consumer route on `ce_type` or `ce_tenantid` without deserialising
the body.

**2. The partition key is the document id.** All events about one document land
on one partition, so their order is preserved where order means something. Using
the tenant id instead would order a tenant's whole history at the cost of
partition skew for large tenants; using a random key would lose ordering
entirely.

**3. Publishing never blocks the HTTP request.** `send_result` enqueues and
returns; delivery is confirmed on a spawned task, and a broker rejection is
logged at `error` with the event id and type. An outage must degrade into logged
drops, not into API latency.

**4. The queue is bounded** (`queue.buffering.max.messages = 100000`,
`message.timeout.ms = 10000`). An unbounded buffer turns a broker outage into an
out-of-memory kill of the API process.

**5. `NoopProducer` survives, as a fallback that announces itself.** When
`REDPANDA_BROKERS` is unset the service logs at **WARN**, in the first lines of
startup, that document events *will be dropped, not published*. When the variable
is set but the producer cannot be built, that is **ERROR**. The producer's
`name()` is part of the logged startup state.

## Alternatives considered

**Keep the no-op and wire the real producer when a broker exists.** Rejected:
that is the state this ADR is undoing. Deferred wiring is indistinguishable from
forgotten wiring six weeks later, and this service proved it.

**Fail startup when `REDPANDA_BROKERS` is unset.** Rejected, though it is the
principled option. There is no Redpanda broker in the ODS staging project at all
— `oid`, `pdf-engine` and `securemail` each carry an explicit
"REDPANDA_BROKERS intentionally unset" note in their deployment descriptor — so
a hard failure would take DocEditor out of staging to protest a platform-level
gap that DocEditor cannot close. A WARN that names the consequence is the honest
middle: the deployment works, and the log says what it is not doing.

**Structured event payloads in JSON content mode.** Rejected for now: binary
mode is what the platform's other Rust producers emit, and a consumer written
against one and fed the other sees an empty envelope.

**Publish synchronously and fail the request on a publish error.** Rejected: it
couples document creation to broker availability, which trades a silent data
loss for a loud availability loss without being asked.

## Consequences

- Events are emitted with the request's correlation id attached, so a document
  change can be followed from the HTTP access log to the bus.
- **As deployed today, staging still drops every event** — for a configuration
  reason now, not a code one, and with a WARN line saying so. Setting
  `REDPANDA_BROKERS` requires a broker to exist; provisioning one is a platform
  decision outside this service.
- `rdkafka` is taken with `cmake-build`, so librdkafka is compiled from source.
  That is why the build needs `libcurl4-openssl-dev`, and why CI and the
  Dockerfile must install the same packages — kept so by
  `tests/ci_workflow.rs`.
- **The topic name was not settled by this ADR, deliberately — and it is settled
  now. The answer is `editor-events`** (dead-letter queue `editor-events-dlq`),
  recorded here as this ADR asked whoever resolved it to do.

  Four spellings existed across four sources: `editor.events` (this repository's
  `CLAUDE.md` and the default in `src/config.rs` until 2026-09-14),
  `ods.editor.events` (the GTM brief, four times, as what products subscribe
  to), `doceditor-events` (the platform rule `{service}-events`) and
  `editor-events`. Measured on 2026-09-14 at 08:28 UTC among the 25 topics of
  `orbus-ods-staging`, **only `editor-events` exists as a provisioned
  resource** — it is already this service's topic. It is also the platform rule
  applied to the name this service carries everywhere else: the schema is
  `editor`, the source of these events is `/editor`, their types are
  `com.ods.editor.*`. `doceditor` names the deployment; `editor` names the
  domain, and the domain is what a consumer reads in `ce_type`.

  Settled by `~/dev/specs/ods-platform/specs/doceditor/spec.md` §4.2, in
  execution of **HR-20260913-001** (option A, it@orbusdigital.com), and applied
  to `src/config.rs` on 2026-09-14. `tests/event_topic_test.rs` guards it,
  because publishing to the wrong topic is silent in exactly the way this ADR is
  about and nothing else would fail.

- **Still not settled: the transport.** The code publishes with `rdkafka`; the
  provisioned resource is Cloud Pub/Sub, and no Redpanda broker exists in the
  staging project. `editor-events` is the name either way — that is why it could
  be settled first — but what replaces the `rdkafka` producer, and how the
  binary-content `ce_*` headers become Pub/Sub message attributes, is a platform
  decision (spec.md §4.3, deviation D-1).
