---
name: operational-not-code
description: "Operational, outside the repository" is a claim to measure, not a category to file under — the reframing that closed a 7-cycle finding on doceditor and the same one on oid
metadata:
  type: feedback
---

Before writing **"operational, outside the repository"** — or `BLOCKED_EXTERNAL`, or "remaining
fix is a role change / a secret rotation / a deployment step" — ask what the **process itself**
can do about the condition: drop its own privileges, provision its own role, refuse to serve.

**Why:** measured twice, on two services, within 24 hours.

- `oid`, 2026-09-13: an A05 security finding closed three cycles running as "operational — rotate
  the secret". Migration 020 created `oid_app` and the pool ran `SET ROLE` on every connection.
  Rows visible without tenant context: 5 → 0, `DATABASE_URL` unchanged.
- `doceditor`, 2026-09-14 (lot 7): **the identical finding, filed identically for seven BA
  cycles.** AC-011, MEDIUM, "remaining fix is an operational role change (non-superuser,
  non-BYPASSRLS), not code." Every cycle re-ran the same `psql` and got the same `t | t`.

The remediation on file had **no possible actor** — a repository cannot rotate a secret — so the
finding was re-measured and re-filed instead of fixed. Underneath sat a premise wrong in one word,
inherited from an ADR and copied by every review since: *"PostgreSQL applies no policy to a
SUPERUSER role."* True of the **effective** role, not of the one that **authenticated**. The
privileges in the connection string are a **ceiling, not a floor**, and a process may stand below
its own ceiling.

**How to apply:**

1. **A diagnosis repeated identically across cycles is not a confirmation — it is the signal that
   a premise is being copied instead of measured.** Re-measuring costs five minutes; in both cases
   that is where the fix was.
2. Take the measurement **by hand, before writing code**, and vary exactly one thing. Here: same
   database, same rows, same tenant context, only `current_user` changes — `1278` rows as `ods`,
   `0` as `editor_app`, and `1302 across 964 tenants` as `ods` *with the same context set*. That
   last line is what makes the argument unanswerable.
3. Keep the operational hardening as an **improvement, not a prerequisite**. Pointing
   `DATABASE_URL` at an unprivileged LOGIN role is still strictly better — nothing can
   `RESET ROLE` back. Saying so keeps the ADR honest and stops the fix reading as a workaround.

See [[doceditor-batch-20260914-lot7]] for the mechanics, and [[doceditor-batch-20260913]] for the
sibling habit: when a BA report says nothing is actionable, go read the path yourself.
