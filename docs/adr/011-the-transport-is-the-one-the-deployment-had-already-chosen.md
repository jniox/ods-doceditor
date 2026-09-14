# ADR-011 — The event transport is Pub/Sub, because the deployment had already chosen it

- **Status**: Accepted
- **Date**: 2026-09-14
- **Decision**: HR-20260914-007, option A, taken 2026-09-14T11:07:01 by
  it@orbusdigital.com. Enactor named in the option text: *« Exécutant : dev,
  dans le dépôt doceditor »*.
- **Supersedes the open question of**: `spec.md` §4.3 and deviation D-1 (§12),
  which left the transport explicitly untaken. It does **not** touch §4.2: the
  topic name was settled before, and on purpose, precisely because a name is
  independent of its carrier.
- **Related**: ADR-002 (events are published for real and their absence is
  loud), ADR-003 platform (Pub/Sub push subscriptions to Cloud Run),
  HR-20260909-001 (h2 / RUSTSEC-2026-0258).

## Context

`doceditor` has produced correct CloudEvents since May 2026 and delivered none
of them. The code published with `rdkafka`; no Redpanda broker exists in
`orbus-ods-staging`; so `producer_from_config` selected the `NoopProducer` and
every event was thrown away — **silently**, which is this service's documented
failure mode: the trait returns `Ok`, the callers check it, the tests observe
that `publish` was called, and nothing anywhere is red (ADR-002).

The BA review re-classified AC-012 as PARTIAL for three cycles, each time
describing the gap as *"the deployment descriptor sets neither `REDPANDA_BROKERS`
nor `REDPANDA_TOPIC`"*. That description was true and it undersold the fact.
Re-measured directly against the **live** revision rather than against the
repo-adjacent descriptor:

```text
$ gcloud run services describe doceditor --project=orbus-ods-staging \
      --region=europe-west1                      # revision doceditor-00003-vkq
EVENT_BUS        = pubsub
PUBSUB_TOPIC     = editor-events
PUBSUB_TOPIC_DLQ = editor-events-dlq
GCP_PROJECT_ID   = orbus-ods-staging
serviceAccount   = runtime-cloud-run@orbus-ods-staging.iam.gserviceaccount.com
containerConcurrency = 80
```

Four variables, **none of which any line of `src/config.rs` read**, and none of
which appear in `~/dev/ops/cloudrun/doceditor.json` either — the descriptor had
drifted from the revision. The deployment had chosen a transport in May; the
code never learned. That asymmetry is what made the question decidable: one of
the three options required no infrastructure at all.

Two further measurements taken for this batch, because a recommendation that
rests on "no infrastructure change" has to be checked rather than asserted:

```text
$ gcloud pubsub topics list --project=orbus-ods-staging | grep editor
projects/orbus-ods-staging/topics/editor-events
projects/orbus-ods-staging/topics/editor-events-dlq

$ gcloud projects get-iam-policy orbus-ods-staging \
    --flatten='bindings[].members' \
    --filter='bindings.members:runtime-cloud-run@orbus-ods-staging.iam.gserviceaccount.com' \
    --format='value(bindings.role)'
… roles/pubsub.publisher …
```

The topics exist and the runtime service account may already publish to them.

## Decision

**Publish to Google Cloud Pub/Sub, selected by the variables the deployment
already sets.** `rdkafka` stays, for local development and for
`tests/events_roundtrip.rs`, which needs a real broker.

### 1. The transport is chosen once, and a choice that cannot be honoured stops the boot

`config::select_event_bus` is a pure function over five environment values and
returns `EventBus::{PubSub, Redpanda, Disabled}` — an enum, not a pile of
`Option`s, because each transport needs *different* values to be usable, and
holding them separately is exactly what let `EVENT_BUS=pubsub` sit unread beside
`REDPANDA_BROKERS` for four months.

`EVENT_BUS=pubsub` without `GCP_PROJECT_ID`, `EVENT_BUS=redpanda` without
`REDPANDA_BROKERS`, or an `EVENT_BUS` naming no transport this service has:
**the boot is refused**, naming the variable to set. This is the stance
`parse_max_document_size_mb` already takes (ADR-009) and here the argument is
stronger. A mis-set ceiling eventually kills an instance and leaves a trace; a
mis-set bus produces *nothing to notice*. Refusing to start is the only variant
of "something is wrong" that a silent bus can be turned into.

The historical path is unchanged: no `EVENT_BUS` at all still means "a broker
address selects Redpanda, its absence selects nothing, loudly".

### 2. REST, not gRPC — a dependency decision, not a style one

Every gRPC client for Pub/Sub pulls `tonic` and therefore `h2`, the crate this
estate is under a platform-wide decision to keep out of its shipped graph
(HR-20260909-001, RUSTSEC-2026-0258, ten repositories). Publishing is one HTTPS
POST; paying for a transitive HTTP/2 stack to make it would undo that decision
here and make this repository the one that re-opened it.

`reqwest` moves into `[dependencies]` with `default-features = false` and
`["json", "rustls-tls"]` — no `http2` feature, and `rustls` is the TLS backend
`sqlx` already brings. Measured after the change:

```text
$ cargo tree -e normal --prefix none | grep '^h2 '
(nothing)                     # 675 crates in the shipped graph, h2 in none of them
$ cargo audit
error: 1 vulnerability found!  # RUSTSEC-2023-0071 (rsa, via sqlx-mysql), unchanged
```

`tests/framework.rs` measures this rather than trusting the paragraph, and the
manifest guard is untouched.

### 3. Credentials come from the instance metadata server, and nothing else

No key file, no secret in this repository, no new variable to provision: on
Cloud Run the runtime service account's access token is served by the metadata
server, and that account already holds `roles/pubsub.publisher`. The token is
cached until a minute before it expires — a cache each spawned task copies is
not a cache, so the HTTP client, the URL and the token live behind one `Arc`
shared by the request path and the spawned publishes.

Two overrides, both Google's own names so a developer learns nothing new:
`PUBSUB_EMULATOR_HOST` (unauthenticated local emulator) and `GCE_METADATA_HOST`.

### 4. CloudEvents binary content mode, transposed onto message attributes

`spec.md` §4.3 asked for exactly this: *"ce qui remplace le producteur rdkafka
et le mode contenu binaire en en-têtes `ce_*` par des attributs de message
Pub/Sub"*. The envelope travels as attributes, the body carries the data alone,
base64 as the REST API requires, and the ordering key is the document id — the
transposition of the Kafka partition key, so a document's history keeps its
order.

The attribute names use the HTTP binding's `ce-` and not Kafka's `ce_`, and that
is not cosmetic: the platform delivers by **push subscription to Cloud Run**
(platform ADR-003), and a push subscription with payload unwrapping and metadata
writing turns each attribute into an HTTP header verbatim. `ce-type` then
arrives as the header a standard CloudEvents HTTP consumer reads; `ce_type`
would arrive as a header no SDK looks for.

**One source, two renderings.** `cloudevent_attributes()` names the envelope
once and each binding prefixes it. Two hand-written lists drift the first time
an attribute is added to one of them, and an envelope missing an attribute fails
nothing — the bus accepts it and a consumer quietly reads `None`. Same shape as
`version_repo`'s two column lists and `document_repo::search_predicate`.

## Consequences

**Measured on the running binary**, because "the producer returned `Ok`" is not
evidence — that is the entire lesson of ADR-002.

*The event actually leaves the process.* Service started with
`EVENT_BUS=pubsub GCP_PROJECT_ID=orbus-ods-staging PUBSUB_TOPIC=editor-events`
and `PUBSUB_EMULATOR_HOST` pointed at a recording endpoint; one ordinary
`POST /api/v1/documents` carrying `X-Correlation-Id: lot16-live-proof`:

```text
POST /v1/projects/orbus-ods-staging/topics/editor-events:publish        (x2)
  ce-type=com.ods.editor.document.created   ce-specversion=1.0  ce-source=/editor
  ce-tenantid=11111111-…      (from the JWT, never from the body)
  ce-correlationid=lot16-live-proof          content-type=application/json
  orderingKey=6102b045-…      (the document id)
  data(base64) -> {"document_id":"6102b045-…","title":"Contrat de prestation",…}

  ce-type=com.ods.editor.version.created … version=1, is_auto=true
```

*The failure is loud.* Same binary, `PUBSUB_EMULATOR_HOST` removed, so the real
`https://pubsub.googleapis.com` — from a host whose service account is *not*
`runtime-cloud-run` and therefore cannot publish:

```text
create: HTTP 201                       <- the caller is not held hostage
ERROR Event not published: Pub/Sub refused the event: 403 Forbidden —
      "status": "PERMISSION_DENIED", "permission": "pubsub.topics.publish",
      "resource": "projects/orbus-ods-staging/topics/editor-events"
      event_id=evt-ef3f… event_type=com.ods.editor.version.created
```

That the API answered `PERMISSION_DENIED` on the named resource — rather than a
`404` or an `INVALID_ARGUMENT` — is itself the proof that the URL and the
message body are well-formed against the real service. In the deployment the
same request is made by an account that holds `roles/pubsub.publisher`.

*The boot refusals are real:*

```text
EVENT_BUS=pubsub   (no GCP_PROJECT_ID)  -> panic: "EVENT_BUS=pubsub needs GCP_PROJECT_ID …"
EVENT_BUS=redpanda (no REDPANDA_BROKERS)-> panic: "EVENT_BUS=redpanda needs REDPANDA_BROKERS …"
EVENT_BUS=rabbitmq                      -> panic: "…names no transport this service can use.
                                                    Accepted: pubsub, redpanda (alias: kafka), none."
nothing set                             -> WARN  "No event bus is selected: document events
                                                    will be DROPPED, not published…"  producer=noop
REDPANDA_BROKERS set, EVENT_BUS unset   -> INFO  producer=redpanda topic=editor-events
```

**What this does not do.** It does not change the topic name (§4.2 settled that),
it does not provision anything, and it does not make the service publish in
staging by itself: the deployed image must be rebuilt and the revision rolled
for the new binary to be the one running. `~/dev/ops/cloudrun/doceditor.json`
lives outside this repository and still lists none of the four variables the
live revision carries — the descriptor drift is reported, not fixed here.

**`PUBSUB_TOPIC_DLQ` is read by nothing, deliberately.** A dead-letter topic is
a property of a *subscription*, not of a publisher; there is no code path in a
publisher that could write to it. A variable read but unused is how `LOG_LEVEL`
spent months having no effect on anything.

**Still open, and still not ours**: nothing in this repository bounds
concurrency (spec.md D-2), and the history remains unpaginated (D-4).
