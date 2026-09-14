---
name: scoped-mechanical-fix
description: When a task arrives scoped to one named file, stay inside it and report adjacent faults instead of fixing them — and expect an idle-watchdog to have committed foreign files onto the branch
metadata:
  type: feedback
---

When a work order names the file (and sometimes the function) to change and calls itself "mechanical
and bounded", treat the boundary as the deliverable, not as a suggestion. Fix what is named; for
everything adjacent that is genuinely wrong, **write it down in the status and the report** rather
than fixing it.

**Why:** these orders execute an already-taken human decision (`HR-…`). Anything outside the named
file is a decision that was not taken, and a reviewer cannot tell an authorised change from an
opportunistic one inside the same commit. On HR-20260909-035 the order went further and pre-empted
the tempting over-corrections: it forbade removing the fallback (the "or better" variant of the
decision only held coupled to an option that was *not* retained — removing it alone would have made
the suite permanently red under the pipeline), forbade touching `.env`/`.env.example`, and forbade
adding `dotenvy` to the test binary even though that was the root reason the correct `.env` was
useless. All three would have looked like improvements in the diff.

**How to apply:** before committing, run `git status --short` and `git diff --cached --name-only`
and check the staged set is *exactly* the named path — never `git add -A`. When the order
pre-authorises a specific remedy for a foreseeable complication, use **that** remedy and nothing
wider; if it does not suffice, stop and report rather than inventing scope. See
[[doceditor-test-database]] for the complication that actually fired.

**An idle watchdog commits for you — check `git log`, not just `git status`.** This repo is swept by
an auto-committer that lands foreign working-tree files as `wip: auto-commit <branch> (idle Ns)`.
On 2026-09-12 it had already committed `.env.example` (5433→5435) onto the unit branch — a file
*both* the standing order and HR-20260909-035 excluded. A clean `git status` therefore does **not**
mean the branch is clean: compare against the remote (`git log --oneline origin/<branch>..HEAD`)
before pushing, or the sweep rides into the PR under your name.

**The converse, so this memory does not make you timid.** A "BA FAIL — implement the missing
criteria or fix the defect" order is **not** scoped: it authorises the whole report. On
2026-09-13 that meant reformatting the entire crate (`cargo fmt`, 511 lines) — the exact gesture
HR-20260909-035 had forbidden — because the BA had listed the red `fmt --check` as a deviation
failing the repo's own CI. Same repository, same file set, opposite correct answer. **Read the
order, not this memory, to decide the boundary**; what travels between orders is the *method*
(one logical change per commit, explicit `git add` paths, style isolated from behaviour), never
the perimeter. When the perimeter is wide, still keep the style commit separate from the
behavioural ones so a reviewer can skip it.

**Remedy when it has:** if the sweep is **unpushed**, a plain `git reset origin/<branch>` (mixed)
un-commits it and leaves the file dirty — which is exactly the state these orders describe. It is
local-only, touches nothing already pushed, needs no force-push, and is reflog-reversible. Do not
"rescue" the content by keeping the commit just because its value is correct (the 5435 port really
is right): correct-but-unauthorised is still unauthorised, and [[h2-advisory-campaign]]'s PR #3 had
to stay at exactly 4 in-scope files.
