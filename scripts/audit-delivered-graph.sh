#!/usr/bin/env bash
#
# Advisories, judged on the graph this service DELIVERS.
#
# `cargo audit` reads `Cargo.lock`, and a lockfile is wider than a binary: it
# holds the optional dependencies of our dependencies, whether or not their
# feature is turned on. Measured here on 2026-09-14, the four crates it flags
# are reached by nothing this service compiles:
#
#   rsa 0.9.10       RUSTSEC-2023-0071   only through sqlx-mysql, and `sqlx` is
#                                        taken with `postgres` alone
#   anyhow 1.0.102   RUSTSEC-2026-0190   removed as a dependency in 735647d
#   spin 0.9.8       yanked              only under sqlx-sqlite / num-bigint-dig
#   h2 0.3           RUSTSEC-2026-0258   gone from the lockfile since b4b8103
#
# So "cargo audit is red" and "this service is exposed" are different
# statements, and it is the second one that has to fail a build. That is the
# whole job of this script: it keeps the same advisory database and intersects
# it with `cargo tree -e normal`, the graph `tests/framework.rs` already treats
# as the reference. See docs/adr/012, which also records that the estate's own
# citation for this rule (BR-0010) resolves to nothing today.
#
# Why it exists at all. On 2026-09-14 RUSTSEC-2026-0285 was published against
# `rustls` 0.23.40 — a crate this service has shipped since it was written,
# through `sqlx`'s `tls-rustls`, and now through `reqwest` as well (ADR-011).
# Nothing in this repository went red: the only dependency guard names `h2`
# 0.3, and a guard shaped for one crate says nothing about the next one. The
# finding came from a reviewer running `cargo audit` by hand, which is the part
# that does not repeat.
#
# Exit codes
#   0  no advisory against a delivered crate (warnings may still be printed)
#   1  at least one delivered crate carries a vulnerability
#   2  the measurement itself could not be taken — never silence
#
# `yanked` and `unsound` findings are reported and do not fail the run, which
# is the line `cargo audit` itself draws. They are printed with the delivered
# flag so the judgement stays available: a delivered yanked crate is worth a
# lockfile bump (735647d took `chacha20` 0.10.0 -> 0.10.2 for exactly that), it
# is simply not worth a red build on an upstream decision no diff caused.
#
# The two _FILE variables exist so tests/advisories.rs can exercise this
# classification without a network or an advisory database. CI sets neither.

set -uo pipefail

CARGO="${CARGO:-cargo}"
delivered_file="${DELIVERED_GRAPH_FILE:-}"
report_file="${AUDIT_REPORT_FILE:-}"

die_unmeasured() {
    echo "cannot judge the delivered graph: $1" >&2
    echo "Nothing was checked. This is not a pass." >&2
    exit 2
}

command -v jq >/dev/null 2>&1 || die_unmeasured "jq is not installed"

# --- the graph this service delivers -------------------------------------
if [ -n "$delivered_file" ]; then
    [ -r "$delivered_file" ] || die_unmeasured "DELIVERED_GRAPH_FILE=$delivered_file is unreadable"
    tree_out="$(cat "$delivered_file")"
else
    tree_out="$("$CARGO" tree -e normal --prefix none --format '{p}' 2>/dev/null)" \
        || die_unmeasured "\`$CARGO tree\` failed"
fi

# `name v1.2.3` per line, the spelling `cargo audit` can be compared against.
delivered="$(printf '%s\n' "$tree_out" | awk 'NF >= 2 { print $1, $2 }' | sort -u)"

# Non-vacuity: an empty set is a subset of everything, so a tree command that
# printed nothing would clear every advisory at once.
[ -n "$delivered" ] || die_unmeasured "the delivered graph came back empty"

# --- the advisories ------------------------------------------------------
if [ -n "$report_file" ]; then
    [ -r "$report_file" ] || die_unmeasured "AUDIT_REPORT_FILE=$report_file is unreadable"
    report="$(cat "$report_file")"
else
    # A non-zero exit is the normal outcome here: it means the lockfile tally
    # found something, which is the question this script re-asks properly.
    report="$("$CARGO" audit --json 2>/dev/null)"
fi

printf '%s' "$report" | jq -e 'type == "object"' >/dev/null 2>&1 \
    || die_unmeasured "the advisory report is not JSON (is cargo-audit installed?)"

# id \t crate \t version \t kind \t fixed-in
findings="$(printf '%s' "$report" | jq -r '
    [ (.vulnerabilities.list // [])[]
      | { id: .advisory.id, name: .package.name, version: .package.version,
          kind: "vulnerability",
          fixed: ((.versions.patched // []) | join(", ")) } ]
  + [ (.warnings // {}) | to_entries[] as $k | $k.value[]
      | { id: (.advisory.id // "-"), name: .package.name, version: .package.version,
          kind: $k.key,
          fixed: ((.versions.patched // []) | join(", ")) } ]
  | .[] | [.id, .name, .version, .kind, (if .fixed == "" then "-" else .fixed end)]
  | @tsv')" || die_unmeasured "could not read the advisory report"

echo "Advisories against the delivered graph ($(printf '%s\n' "$delivered" | wc -l) crates compiled into this service)"
echo

if [ -z "$findings" ]; then
    echo "  cargo audit reports nothing at all."
    exit 0
fi

printf '  %-22s %-24s %-13s %-12s %s\n' ADVISORY CRATE KIND DELIVERED 'FIXED IN'
status=0
while IFS=$'\t' read -r id name version kind fixed; do
    [ -n "$name" ] || continue
    if printf '%s\n' "$delivered" | grep -qxF "$name v$version"; then
        flag=yes
        [ "$kind" = vulnerability ] && status=1
    else
        flag=no
    fi
    printf '  %-22s %-24s %-13s %-12s %s\n' "$id" "$name $version" "$kind" "$flag" "$fixed"
done <<< "$findings"

echo
if [ "$status" -ne 0 ]; then
    cat >&2 <<'MSG'
A crate this service compiles carries a vulnerability (DELIVERED = yes above).

Take the fix rather than the ignore, in this order:
  1. a semver-compatible release exists -> `cargo update -p <crate>`, lockfile only;
  2. the crate is pulled by a feature nothing uses -> turn the feature off;
  3. the crate is pulled by a dependency nothing imports -> remove it
     (tests/dependencies.rs exists because that has happened here);
  4. none of the above -> escalate. Do not add a blanket ignore: an advisory
     against something we ship is exposure, and this repository keeps that
     judgement in the open (HR-20260909-001, ADR-012).
MSG
fi
exit "$status"
