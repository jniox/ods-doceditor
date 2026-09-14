# ADR-008 — `metadata` is bounded in every shape it can take

- **Status**: accepted
- **Date**: 2026-09-14
- **Supersedes nothing. Extends**: ADR-006 (the search index bound), ADR-007 (the
  history list is a projection without bodies)

## Context

`docs/openapi.yaml` has always bounded `metadata` in prose: *at most 20 keys;
keys match `^[a-z][a-z0-9_]{0,63}$`; string values are at most 256 characters.*

`src/api/payload.rs` relies on that sentence by name. `ENVELOPE_ALLOWANCE_BYTES`
— the `+ 64 KiB` of the payload ceiling `2 × MAX_DOCUMENT_SIZE_MB + 64 KiB` — is
documented as covering *"the largest envelope **this service's own validation
admits** — a 500-byte title, twenty metadata keys of 64 bytes holding 256-byte
values, a `template_id`, and the field names and punctuation around them. That
is under 7 KiB; 64 KiB leaves an order of magnitude."*

Neither claim was true.

`DocumentService::validate_metadata` checked string lengths with
`if let Some(s) = value.as_str()`, which is a statement about the values of the
metadata object and about nothing else. Measured on the running binary (release,
`MAX_DOCUMENT_SIZE_MB=10`, HS256, port 8199):

```text
POST metadata {"resume": 257 × 'a'}                  -> 422 "must be at most 256 characters"
POST metadata {"resume": {"inner": 257 × 'a'}}       -> 201
POST metadata {"tags": [257 × 'a']}                  -> 201
POST metadata {"resume": {"inner": 5 000 000 × 'a'}} -> 201
```

The container decided whether the rule existed. And no rule bounded the object
as a whole, so the only remaining ceiling was the payload one — 20 MiB — derived
from the allowance whose own premise this paragraph quotes. The unit test
guarding that premise computed the envelope from the **prose** of the rules
(`500 + 20 * (64 + 256 + 6) + 36 + 200`) and compared it to the constant: an
arithmetic restatement checked against itself, green while the service admitted
three hundred times the number it asserted. Same shape as the payload test of
ADR-005's batch, which exercised an `App` carrying no `JsonConfig` at all.

### What it cost

Not a refused request. `DocumentSummary` drops `content` on purpose (ADR-007)
and keeps `metadata`, so a page multiplies this field by up to a hundred.
Measured on the same binary — thirty documents of 10 MB of metadata each, thirty
ordinary `201`s with empty bodies, then one list:

```text
GET /api/v1/documents?per_page=30 -> 200, 300 010 939 bytes, peak RSS 945 MiB
```

against the **512 MiB** `ops/cloudrun/doceditor.json` allocates. That is an OOM
kill of the instance, not a failed request: on Cloud Run every other tenant's
in-flight request dies with it, and one caller reaches it with ordinary calls.

It is the fifth batch running in which a cost was attached to a quantity other
than the one its name promised — after the payload/body split, the byte/character
bound, the search index, and the version history. This one is the same ending as
ADR-007, reached through the one column that read had kept.

## Decision

1. **`domain::metadata::Metadata` is a parsed value**, like `Title`, `Comment`
   and `Pagination`. It can only be built by `parse`, `document_repo` takes
   **it** rather than a `serde_json::Value`, and both handlers parse at the
   boundary. There is no longer a path that writes an unchecked object into the
   `jsonb` column.

2. **The character bound holds at every depth.** The rule is stated about string
   values; the shape of the container is not part of the rule. The walk is
   iterative — `serde_json` bounds parsing depth, but a domain type should not
   owe its stack safety to whoever built the value.

3. **`MAX_METADATA_BYTES = 32 KiB` bounds the serialised object.** Derived, not
   chosen: half of `ENVELOPE_ALLOWANCE_BYTES`, which leaves the title its own
   worst case with an order of magnitude to spare, and still admits five times
   the ~6.5 KiB the per-key rules describe. The two rules are **not redundant**:
   an array of a million admissible strings breaks no per-value rule, and one
   5 000-character string breaks no size rule at these magnitudes.

4. **`payload.rs`'s test reads the constants the domain enforces.** That is the
   load-bearing part rather than the tidy one: the day someone raises the
   metadata bound past what the payload ceiling reserves, the test goes red
   instead of the instance going down.

5. **The size is counted into a sink, not into a buffer.** `serde_json::to_vec`
   would allocate a second copy of the very object suspected of being too large,
   and would have to serialise all 20 MiB of it to discover that it is. A
   counting `io::Write` that errors past the budget is exact and stops early.

## Consequences

Measured after the change, on the same binary, same conditions:

```text
POST metadata {"resume": 257 × 'a'}                  -> 422 value for key 'resume' … (got 257)
POST metadata {"resume": {"inner": 257 × 'a'}}       -> 422 value for key 'resume' … (got 257)
POST metadata {"tags": [257 × 'a']}                  -> 422 value for key 'tags' … (got 257)
POST metadata {"resume": {"inner": 5 000 000 × 'a'}} -> 422 must serialise to at most 32768 bytes
POST metadata {"tags": [250 × 'a'] × 1000}           -> 422 must serialise to at most 32768 bytes
POST metadata {"kind":"contract","stage":"p4", …}    -> 201
```

and the worst page a caller can now construct — a hundred documents each
carrying the largest admissible metadata (31 760 bytes):

```text
GET /api/v1/documents?per_page=100 -> 200, 3 197 951 bytes, 64 ms, peak RSS 23 MiB
```

300 MB and 945 MiB become 3.2 MB and 23 MiB, and the second figure is a
**bound** — `per_page × MAX_METADATA_BYTES` — where the first was a function of
what a caller chose to store.

### What this does not buy, said plainly

- **The transient cost of a single request is unchanged.** A request is parsed
  into a `serde_json::Value` by the extractor before any rule of this ADR runs,
  so a 20 MiB envelope still costs 20 MiB-plus of RAM for the length of one
  request. That cost is bounded by the payload ceiling, which is an existing,
  documented, deliberate limit. What this ADR removes is the *persistent,
  multiplied* cost: what a page may weigh, however many callers stored however
  much.
- **Rows written before this batch are not affected.** Validation is on write.
  The dev instance was checked and holds none (`SELECT count(*) … WHERE
  length(metadata::text) > 32768` → 0 after the probes were removed); a
  production instance that held some would still serve expensive pages until
  they were rewritten. No migration is proposed, because none is needed here and
  a guess would be worse than a measurement.
- **`MAX_METADATA_BYTES` is a bound on this service's own envelope, not a
  product decision about what metadata is for.** If a product needs more than
  32 KiB of structured data per document, the field for that is `content`, which
  is bounded at `MAX_DOCUMENT_SIZE_MB` and excluded from the list projection on
  purpose.

## Alternatives rejected

- **Bound only the size.** It stops the crash and leaves a documented rule
  evadable by typing `[` — the next reviewer re-derives the same finding.
- **Bound only the strings, at every depth.** It makes the contract true and
  leaves the page unbounded: a million 256-character strings satisfy it.
- **Drop `metadata` from `DocumentSummary`.** It would bound the page, and it
  would change the published contract for every existing client — a product
  decision, with no spec to authorise it. Bounding the field is the repair;
  removing it from a response is not.
