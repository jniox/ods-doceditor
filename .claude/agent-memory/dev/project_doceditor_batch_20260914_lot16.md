---
name: doceditor-batch-20260914-lot16
description: Lot 16 — the second decision in two batches that was taken, dispatched and never routed; the deployment had chosen a transport in May and the code never learned, so four months of events went nowhere in silence
metadata:
  type: project
---

Lot 16 (`5868607` → `2957a2c`), **nineteenth** dev turn on unit
`doceditor-c20260909-1345`, still PR #3. 192 tests (170 before), 27 binaries,
CI 4/4 including Docker Build (run `34839176789` — checked on purpose, because
this batch adds a dependency). Twelfth BA report in a row with nothing
code-actionable (31/32 MET, AC-012 PARTIAL).

**The turn's work was in the decision the report cited — again.** The report was
written at 11:00 and said HR-20260914-007 was `PENDING`, *"not fixable by a
doceditor dev cycle alone"*. Read as JSON at 11:3x:

```text
status            DISPATCHED
resolvedAt        2026-09-14T11:07:01     resolution.by  it@orbusdigital.com
resolution        "doceditor transport A"
options[0].enactor  "dev"   ("Exécutant : dev, dans le dépôt doceditor")
enactError        {"by":"dispatcher","reason":"verbe inconnu : doceditor transport A"}
```

Identical shape to lot 14's `"verbe inconnu : doceditor sizing A"`. **Twice in
two batches**, so it is a rule now, not an anecdote: a decision taken but
unrouted is indistinguishable from a pending one *from one level up*, and the
BA report's paraphrase is exactly that level. `business-rules.md` re-read whole:
8 `BR-xxxx`, **none mentions doceditor** — no criterion contradicted, no spec
defect on that ground.

**The defect.** The code published with `rdkafka`; the live revision
`doceditor-00003-vkq` has carried `EVENT_BUS=pubsub`, `PUBSUB_TOPIC=editor-events`,
`PUBSUB_TOPIC_DLQ` and `GCP_PROJECT_ID` since **May 2026** — four variables no
line of `src/config.rs` read — and no Redpanda broker exists in staging. So the
`NoopProducer` was selected and every event was thrown away, in the silence
ADR-002 describes. **The deployment had chosen; the code never learned.**

**Measure the premise of the recommended option before coding it.** Option A was
recommended as "no infrastructure change". Three commands, five minutes:

```text
gcloud run services describe doceditor   -> the four variables, AND
                                            containerConcurrency=80 set EXPLICITLY
                                            (ADR-009's third term, until now inferred)
gcloud pubsub topics list                -> editor-events + -dlq exist, 0 subscriptions
gcloud projects get-iam-policy …         -> runtime-cloud-run ALREADY has
                                            roles/pubsub.publisher
```

That third line is what made the option free, and **it is deducible from no file
in the repository**. The descriptor `~/dev/ops/cloudrun/doceditor.json` lists
none of the four — it had drifted from the revision, which is the BA's own
lesson of the same day.

**What was built.** `config::select_event_bus` chooses once, as an **enum rather
than a pile of `Option`s** — holding them separately is literally what let
`EVENT_BUS=pubsub` sit unread beside `REDPANDA_BROKERS` for four months — and
**refuses the boot** on a choice it cannot honour. Stricter than
`MAX_DOCUMENT_SIZE_MB` deliberately: a mis-set ceiling eventually kills an
instance and leaves a trace; **a mis-set bus produces nothing to notice**, so
refusing to start is the only signal available. `EVENT_BUS` unset keeps the old
behaviour byte for byte.

`events::pubsub` speaks **REST, not gRPC**: every gRPC Pub/Sub client pulls
`tonic` → `h2`, which HR-20260909-001 keeps out of the shipped graph across ten
repos. `reqwest` enters `[dependencies]` with `default-features = false` +
`["json","rustls-tls"]`. Re-measured after: `cargo tree -e normal` = **675
crates, h2 in none of them**; `cargo audit` still exactly the pre-existing `rsa`
advisory. Adding an HTTP client is precisely the moment to re-measure BR-0010.

**One envelope, two bindings.** `cloudevent_attributes()` names the CloudEvents
attributes once; Kafka prefixes `ce_`, Pub/Sub `ce-`. The hyphen is load-bearing
(a push subscription with payload unwrapping turns each attribute into an HTTP
header verbatim, so `ce-type` is what a CloudEvents SDK reads). Same cure as
`version_repo`'s two column lists. `PUBSUB_TOPIC_DLQ` is read by **nothing**: a
dead-letter topic is a property of a *subscription*, not of a publisher.

**Evidence taken on the binary, both directions.** One ordinary
`POST /api/v1/documents` → two CloudEvents on
`/v1/projects/orbus-ods-staging/topics/editor-events:publish`, `ce-tenantid`
from the JWT, `ce-correlationid` adopted from the header, `orderingKey` = the
document id. Then the same binary against the **real** `pubsub.googleapis.com`
from a host whose account may not publish: `201` to the caller and an **ERROR
per event** naming `PERMISSION_DENIED` **on the named resource** — and that the
API resolved the resource (rather than `404` or `INVALID_ARGUMENT`) is itself
proof the URL and body are well-formed. Before this batch the same situation
produced nothing at all.

**Traps and mechanics:**

- A `let` closure `|v: Option<&str>| v.map(str::trim).filter(|v| !v.is_empty())`
  does not compile (lifetime `'1` must outlive `'2`); a nested `fn` does.
- `tokio::spawn` inside `EventProducer::publish` must clone an **`Arc` of the
  shared client**, not rebuild a producer from cloned fields: the latter
  compiles, passes every test, and silently makes the token cache one
  metadata-server call per event.
- Running the binary from the repo root loads `.env`, whose
  `JWT_RSA_PUBLIC_KEY_B64=__NEEDS_HUMAN__` panics the boot. Run it from
  elsewhere (`cd /tmp/...`) — `dotenvy` does not override real env vars but it
  does add that one.
- `gcloud pubsub topics get-iam-policy` needs a permission the agent host lacks;
  `gcloud projects get-iam-policy --flatten` on the service account answers the
  same question and does work.
- The probe publish to the real topic was refused 403 and the topic has **0
  subscriptions** — nothing written, nothing readable. Worth saying in the
  evidence rather than leaving a reader to wonder.
- Mutation-tested the new wire tests three ways (`ce-`→`ce_`, non-2xx read as
  success, `Metadata-Flavor` dropped): three distinct failures, then green.

**Left open, outside this repository, and reported not fixed:** the descriptor
drift; the deployed image (`doceditor:baseline`, May) must be rebuilt for this
code to run; and **BR-0002** — `spec.md` §4.3 and D-1 still call the transport
untaken, and **option A named no spec-writer unit** (B and C did), so unless
someone rewrites them citing HR-20260914-007 the BA will re-flag AC-012 next
cycle. Say that in the status rather than editing a file outside the repo.

See [[go-read-the-path-yourself]] for the running checklist this extends, and
[[doceditor-batch-20260914-lot14]] for the first instance of the same unrouted
decision.
