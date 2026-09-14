---
name: doceditor-batch-20260914
description: Lot 6 — the h2 unit re-verified (nothing to redo), the soft-delete leak found by auditing read paths, and the branch-from-dev order that could not be followed
metadata:
  type: project
---

Lot 6 (`52ea799` → `8de0271` → `055d865`), **ninth** dev turn on unit
`doceditor-c20260909-1345`, still PR #3. 80 tests (76 before). CI
`34792148857` and `34792296687`, both 4/4 green.

**The order arrived dressed as the h2 unit (HR-20260909-001) and there was
nothing to do.** Re-measured rather than redone — this is the cheap path and it
is the right one: `cargo tree -e normal -i h2` → *nothing to print* (h2 absent
from the shipped graph **entirely**), `h2 v0.3` → 0, `RUSTSEC-2026-0258` in
`cargo audit` → 0, lockfile h2 = **0.4.19** (above the ≥0.4.16 threshold), no
`.cargo/audit.toml`. See [[h2-advisory-campaign]] for why it keeps coming back.

**BR-0010 is now a written standard and it settles the order's own criterion.**
The order still demands « cargo audit : 0 vulnerabilities ». That is
unreachable estate-wide and STANDARDS.md §7 BR-0010 explicitly replaces it with
the *delivered graph*. Here the audit reports **1** (`rsa` 0.9.10,
RUSTSEC-2023-0071, no upstream fix, via `sqlx-mysql`) — and
`cargo tree -e normal -i rsa` says *nothing to print*. Same for `anyhow`.
**Measure each remaining advisory against `-e normal` and say so**; do not
report "1 vulnerability" as if it were exposure, and do not try to reach zero.

**The real work: read the paths the BA cannot.** The BA report said, for the
third cycle running, *"no dev cycle needed on doceditor's code"*. Lot 5 found a
concurrency defect by looking; lot 6 found an **access-control** one the same
way, and the method generalises:

> `GET /documents/{id}/versions/{n}` never checked the parent document. After
> `DELETE`, the document read and the version **list** both 404'd, while the
> version **read** returned **200 and the full body** — the only one of the
> three that returns content, reachable by counting from 1.

Tenant isolation was never involved (both queries filter `tenant_id`); what
leaked is a document the caller's **own** tenant had deleted. Root cause was a
**duplicated predicate**, not an omission: `list_versions` carried
`deleted_at IS NULL` inline and `get_version`, forty lines below in the same
file, was written without it. Fixed with a single chokepoint,
`ensure_live_document`, not a second copy.

**Two habits that did the finding, worth reusing verbatim:**

1. **Audit a rule across all its call sites, not a function at a time.** The
   defect was invisible reading `get_version` alone — it looks complete. It is
   only visible *next to its neighbour*. Grep the predicate, not the function.
2. **Write the test as a loop over every route of the resource.** A test per
   route only finds the routes you thought of, and the forgotten route is
   precisely the one carrying the defect. Add a **pre-condition loop** asserting
   200 before the mutation, or a route 404-ing for an unrelated reason (typo,
   unregistered route) passes while proving nothing. Assert on the **body** of
   the 404 too, not only the status.

Mutation-checked both ways and captured: removing the guard from `get_version`
→ exactly 2 red; making the chokepoint itself vacuous → the same 2 red (which
is what proves it covers both paths).

**Reported, deliberately NOT fixed: `archived` freezes the status, not the
body.** Transitions are terminal at `archived`, but a PATCH carrying only
`content` never enters the transition check, so an archived document stays
editable and keeps advancing its version. Whether `archived` should freeze
content is a **product** question with no spec — fixing it would invent a
requirement (BR-0002). Written into `CLAUDE.md` instead. This is the line to
hold when a turn has momentum: an internal inconsistency you can *see* is not
automatically a defect you may *decide*.

**The branch-from-`dev` instruction could not be followed, and saying why is
part of the job.** The standing order says to cut `feat/<id>-lot<N>` from
`dev`. Measured: the unit branch is **28 commits ahead of `dev`**, all pushed,
behind open+`MERGEABLE` PR #3. Cutting from `dev` abandons 28 commits of
delivered work; cutting from the unit branch and opening a second PR duplicates
those 28 commits into a competing PR. I created the lot6 branch, measured,
deleted it, and stayed on the unit branch as lots 2–5 did — then said so
explicitly in the status, the report and progress.md. **Generalise: when a
standing order's mechanism conflicts with its purpose, measure the divergence
first (`git log --oneline origin/dev..HEAD | wc -l`), pick the purpose, and
declare the deviation.**

**The four non-code items, re-measured again** (evidence `055d865/02..05`) —
all four unchanged since lot 3, none of them dev's: `REDPANDA_BROKERS` still
absent from `~/dev/ops/cloudrun/doceditor.json` (devops, outside the repo);
`ods` still `rolsuper=t rolbypassrls=t` so **RLS is inert** and isolation rests
entirely on the tenant predicates (audited present on every query this lot);
`editor-events`/`editor-events-dlq` provisioned and matching none of the three
documented names — **still not applied** (BR-0002); `HR-20260913-006` still
`PENDING`, `spec.md` still absent. Re-measuring takes five minutes and is what
justifies not touching them.

**Infrastructure held this time:** both standing containers (`ods-postgres`
5435, `doceditor-redpanda-dev` 19092) were up on arrival — the lot-4 fix
survived. Check with `docker ps` before reading red round-trip tests as a
regression. See [[doceditor-test-database]].

See [[doceditor-batch-20260913]] for lots 1–5 and the method that produced
them; [[scoped-mechanical-fix]] for the perimeter question this order raised
again.
