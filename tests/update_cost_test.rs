//! Renaming a document rewrote the document.
//!
//! `PATCH /api/v1/documents/{id}` with `{"title": "…"}` changes twenty-six
//! bytes. Underneath, `document_repo::update_document` read the whole row —
//! body included — with `FOR UPDATE`, then wrote **every** column back:
//!
//! ```sql
//! SET title = $1, status = $2, metadata = $3, content = $4, word_count = $5,
//!     current_version = $6, updated_at = now()
//! ```
//!
//! `content = $4` was bound to the body that had just been read, so PostgreSQL
//! received a *new* value for the column and re-TOASTed it: new chunks, new WAL,
//! the old chunks dead until autovacuum. The cost of renaming a document was the
//! size of the document.
//!
//! Measured on the running binary (release build, PostgreSQL 17.11, an 8 000 000
//! byte incompressible body — a document at the ceiling this service published
//! until HR-20260914-001):
//!
//! ```text
//!                                              WAL written    latency
//!   before   PATCH {"title": "Renommage A"}     9 087 272 B     506 ms
//!   before   PATCH {"status": "published"}      9 086 976 B     286 ms
//!   after    PATCH {"title": …}                   299 144 B     120 ms
//!   after    PATCH {"status": …}                  299 160 B     123 ms
//!   after    PATCH {"metadata": …}                299 144 B     120 ms
//!   an idle window of the same length                   0 B          —
//!
//!   the legitimate write, unchanged by this batch:
//!   before   PATCH {"content": …}              17 670 984 B     427 ms
//!   after    PATCH {"content": …}              17 650 104 B  ~450-500 ms
//! ```
//!
//! Thirty times the write-ahead log, and as many bytes of dead TOAST, for a
//! change that touched no body. The 299 kB floor is the index work every update
//! owes — `updated_at` is itself indexed, so no update of this table is ever
//! HOT — and it is the same before and after; what disappears is the body.
//!
//! Three consequences, none of them visible in a response: write-ahead log
//! volume (billed, replicated, and read by the Datastream CDC of ADR-004 —
//! platform analytics would carry 8 MB per rename), table bloat until
//! autovacuum, and two useless copies of the body in the process on every
//! PATCH — including on a PATCH that *replaces* the body, which read the old one
//! first for nothing.
//!
//! And the hypothesis the measurement refused: at the deployment's own limits
//! this was **not** an instance killer. Twenty then forty concurrent renames of
//! that document, in a 512 MiB cgroup, all answered `200` on both sides —
//! peak 369 -> 260 MiB at N=20, 384 -> 238 MiB at N=40. The repair buys
//! headroom and a factor of thirty on the log; it does not rescue the instance
//! from a death it was not dying. See ADR-014.
//!
//! Fifth batch running in which a cost was attached to a quantity other than the
//! one its name promised: the payload ceiling applied to the encoding (ADR-009),
//! the character bound applied to bytes, the search index bounded by vocabulary
//! (ADR-006), the history list reading every body it does not return (ADR-007).
//! Here: "rename" priced in megabytes of body.
//!
//! **What the tests below measure.** Not latency — a timing assertion on a
//! shared instance is a flake waiting for a neighbour. PostgreSQL 17 exposes
//! `pg_column_toast_chunk_id(value)`, which names the TOAST value a row's
//! column points at. A body that was re-written points at a **new** chunk id; a
//! body that was left alone points at the same one. That is a per-row fact:
//! immune to whatever the other test binaries of this suite are writing in
//! parallel, unlike a WAL-position delta or a table-size delta, which are
//! cluster-wide and would make this file fail for someone else's megabytes.
//!
//! The witness that the instrument is not vacuous is in the file:
//! [`a_new_body_does_rewrite_the_body`] asserts the chunk id *does* move when
//! the body really changes. Without it, "the chunk id did not change" would also
//! be true of an instrument that never sees anything.

mod common;

use common::setup_test_pool;
use ods_doceditor::domain::document::{Document, DocumentUpdate};
use ods_doceditor::domain::metadata::Metadata;
use ods_doceditor::repository::document_repo::{self, DOC_COLUMNS, UPDATE_LOCK_COLUMNS};
use ods_doceditor::repository::tenant_context::begin_tenant_tx;
use sqlx::PgPool;
use uuid::Uuid;

/// Big enough to be stored out of line (the TOAST threshold is ~2 kB), small
/// enough that the suite stays quick. The fixture asserts the value really was
/// toasted rather than assuming it.
const BODY_BYTES: usize = 256 * 1024;

/// An incompressible body of `bytes` characters.
///
/// Incompressible on purpose: PostgreSQL compresses before it moves a value out
/// of line, so a body of repeated prose can stay small enough to live inside the
/// tuple — and a document stored inline has no chunk id at all, which would make
/// every assertion in this file vacuously true. A linear congruential generator
/// rather than a random crate: the fixture must be reproducible.
fn incompressible_body(bytes: usize) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut state: u64 = 0x0d0c_ed17_2026_0915;
    let mut out = String::with_capacity(bytes);
    for _ in 0..bytes {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        out.push(ALPHABET[(state >> 33) as usize % ALPHABET.len()] as char);
    }
    out
}

/// The TOAST value the row's `content` column points at, or `None` when the
/// body is small enough to live inside the tuple.
///
/// `pg_column_toast_chunk_id` is PostgreSQL 17's; the service targets 17 (see
/// `CLAUDE.md`). Rendered as text because the function returns an `oid`.
async fn body_chunk_id(pool: &PgPool, tenant_id: Uuid, document_id: Uuid) -> Option<String> {
    let mut tx = begin_tenant_tx(pool, tenant_id)
        .await
        .expect("tenant transaction opens");
    let id: Option<String> = sqlx::query_scalar(
        "SELECT pg_column_toast_chunk_id(content)::text
           FROM editor.documents WHERE id = $1 AND tenant_id = $2",
    )
    .bind(document_id)
    .bind(tenant_id)
    .fetch_one(&mut *tx)
    .await
    .expect("the document's body is measurable");
    tx.commit().await.expect("the measurement commits");
    id
}

/// A document whose body is genuinely stored out of line, with its chunk id.
async fn toasted_document(pool: &PgPool, tenant_id: Uuid, user_id: Uuid) -> (Document, String) {
    let doc = document_repo::create_document(
        pool,
        tenant_id,
        &common::title("Contrat de prestation"),
        user_id,
        &Metadata::empty(),
        &incompressible_body(BODY_BYTES),
    )
    .await
    .expect("the document is created");

    let chunk_id = body_chunk_id(pool, tenant_id, doc.id).await.expect(
        "fixture: the body must be stored out of line, or this file measures nothing — \
         a body that lives inside the tuple has no chunk id",
    );
    (doc, chunk_id)
}

async fn reread(pool: &PgPool, tenant_id: Uuid, document_id: Uuid) -> Document {
    document_repo::get_document(pool, tenant_id, document_id)
        .await
        .expect("the document is readable")
}

/// The claim: a rename does not touch the body.
#[actix_web::test]
async fn a_rename_does_not_rewrite_the_body() {
    let pool = setup_test_pool().await;
    let (tenant_id, user_id) = (Uuid::new_v4(), Uuid::new_v4());
    let (doc, before) = toasted_document(&pool, tenant_id, user_id).await;

    document_repo::update_document(
        &pool,
        tenant_id,
        doc.id,
        user_id,
        DocumentUpdate {
            title: Some(common::title("Contrat de prestation — avenant 1")),
            ..Default::default()
        },
    )
    .await
    .expect("the rename succeeds");

    let after = body_chunk_id(&pool, tenant_id, doc.id).await;
    assert_eq!(
        after.as_deref(),
        Some(before.as_str()),
        "renaming a document must not rewrite its body: the TOAST value moved from {before} to \
         {after:?}, which means PostgreSQL re-wrote every byte of the body (and its write-ahead \
         log, and a dead copy until autovacuum) for a change of title. Measured at 8 MB: \
         9 087 272 bytes of WAL instead of 299 120."
    );
}

/// The same claim for the other two fields a PATCH can carry alone.
#[actix_web::test]
async fn a_status_change_does_not_rewrite_the_body() {
    let pool = setup_test_pool().await;
    let (tenant_id, user_id) = (Uuid::new_v4(), Uuid::new_v4());
    let (doc, before) = toasted_document(&pool, tenant_id, user_id).await;

    document_repo::update_document(
        &pool,
        tenant_id,
        doc.id,
        user_id,
        DocumentUpdate {
            status: Some("published"),
            ..Default::default()
        },
    )
    .await
    .expect("the transition draft -> published succeeds");

    let after = body_chunk_id(&pool, tenant_id, doc.id).await;
    assert_eq!(
        after.as_deref(),
        Some(before.as_str()),
        "publishing a document must not rewrite its body: {before} -> {after:?}"
    );
}

#[actix_web::test]
async fn a_metadata_change_does_not_rewrite_the_body() {
    let pool = setup_test_pool().await;
    let (tenant_id, user_id) = (Uuid::new_v4(), Uuid::new_v4());
    let (doc, before) = toasted_document(&pool, tenant_id, user_id).await;

    let metadata = Metadata::parse(&serde_json::json!({"dossier": "2026-0915"}))
        .expect("the fixture metadata is within bounds");
    document_repo::update_document(
        &pool,
        tenant_id,
        doc.id,
        user_id,
        DocumentUpdate {
            metadata: Some(metadata),
            ..Default::default()
        },
    )
    .await
    .expect("the metadata update succeeds");

    let after = body_chunk_id(&pool, tenant_id, doc.id).await;
    assert_eq!(
        after.as_deref(),
        Some(before.as_str()),
        "tagging a document must not rewrite its body: {before} -> {after:?}"
    );
}

/// The witness: the instrument above can see a body being rewritten.
///
/// Without this test, every assertion in this file would also hold for a
/// measurement that is simply blind — which is how the first draft of
/// `tests/list_stability_test.rs` came to be green against the very defect it
/// was written for.
#[actix_web::test]
async fn a_new_body_does_rewrite_the_body() {
    let pool = setup_test_pool().await;
    let (tenant_id, user_id) = (Uuid::new_v4(), Uuid::new_v4());
    let (doc, before) = toasted_document(&pool, tenant_id, user_id).await;

    let replacement = incompressible_body(BODY_BYTES / 2);
    document_repo::update_document(
        &pool,
        tenant_id,
        doc.id,
        user_id,
        DocumentUpdate {
            content: Some(&replacement),
            ..Default::default()
        },
    )
    .await
    .expect("the content mutation succeeds");

    let after = body_chunk_id(&pool, tenant_id, doc.id).await;
    assert_ne!(
        after.as_deref(),
        Some(before.as_str()),
        "a new body must be written — if the chunk id does not move here, the instrument these \
         tests rely on sees nothing and their green means nothing"
    );
}

/// What an update reads under its lock, and what it must not read.
///
/// The write half above has a companion on the read side: the `FOR UPDATE`
/// statement that locks the row before deciding anything used to select
/// [`DOC_COLUMNS`] — the body with it — so every PATCH pulled the whole document
/// out of PostgreSQL, *including* a PATCH whose only purpose was to replace it.
/// The projection is named once, as `version_repo` names its two, so it cannot
/// quietly grow a body column again.
#[actix_web::test]
async fn the_locking_read_of_an_update_carries_no_body() {
    assert!(
        !UPDATE_LOCK_COLUMNS.contains("content"),
        "the locking read of an update must not select the body: {UPDATE_LOCK_COLUMNS}"
    );
    assert!(
        DOC_COLUMNS.contains("content"),
        "the full projection must still serve the body: {DOC_COLUMNS}"
    );

    let pool = setup_test_pool().await;
    let (tenant_id, user_id) = (Uuid::new_v4(), Uuid::new_v4());
    let (doc, _) = toasted_document(&pool, tenant_id, user_id).await;

    let lock_bytes = projection_bytes(&pool, tenant_id, doc.id, UPDATE_LOCK_COLUMNS).await;
    let full_bytes = projection_bytes(&pool, tenant_id, doc.id, DOC_COLUMNS).await;

    assert!(
        full_bytes > 200 * 1024,
        "fixture: the full projection must really carry the body, got {full_bytes} bytes"
    );
    assert!(
        lock_bytes < 1024,
        "locking a row to decide a transition must cost bytes, not megabytes: \
         {lock_bytes} bytes against {full_bytes} for the full row"
    );
}

/// How many bytes PostgreSQL must materialise and send for `projection`.
///
/// `row::text` renders the composite as the server would have to produce it,
/// detoasting every column — the same instrument `tests/history_read_test.rs`
/// uses one table over.
async fn projection_bytes(
    pool: &PgPool,
    tenant_id: Uuid,
    document_id: Uuid,
    projection: &str,
) -> i64 {
    let mut tx = begin_tenant_tx(pool, tenant_id)
        .await
        .expect("tenant transaction opens");
    let bytes: i64 = sqlx::query_scalar(&format!(
        "SELECT coalesce(sum(octet_length(row::text)), 0)::bigint
           FROM (SELECT {projection}
                   FROM editor.documents
                  WHERE id = $1 AND tenant_id = $2) AS row"
    ))
    .bind(document_id)
    .bind(tenant_id)
    .fetch_one(&mut *tx)
    .await
    .expect("the projection is measurable");
    tx.commit().await.expect("the measurement commits");
    bytes
}

/// Cheaper must still mean the same thing: an update changes what it names and
/// nothing else.
///
/// The repair replaces seven assignments read from a pre-read row with
/// `COALESCE($n, column)`, so this is the test that the substitution is
/// faithful — field by field, including the two that only move when the body
/// does (`word_count`, `current_version`) and the history row that must exist
/// for the new version.
#[actix_web::test]
async fn a_partial_update_changes_only_what_it_names() {
    let pool = setup_test_pool().await;
    let (tenant_id, user_id) = (Uuid::new_v4(), Uuid::new_v4());
    let body = incompressible_body(BODY_BYTES);

    let metadata = Metadata::parse(&serde_json::json!({"dossier": "2026-0915", "lot": 18}))
        .expect("the fixture metadata is within bounds");
    let doc = document_repo::create_document(
        &pool,
        tenant_id,
        &common::title("Contrat de prestation"),
        user_id,
        &metadata,
        &body,
    )
    .await
    .expect("the document is created");
    assert_eq!(doc.current_version, 1);
    let words = doc.word_count;

    // A rename touches the title, the clock, and nothing else.
    document_repo::update_document(
        &pool,
        tenant_id,
        doc.id,
        user_id,
        DocumentUpdate {
            title: Some(common::title("Avenant 1")),
            ..Default::default()
        },
    )
    .await
    .expect("the rename succeeds");

    let renamed = reread(&pool, tenant_id, doc.id).await;
    assert_eq!(renamed.title, "Avenant 1");
    assert_eq!(renamed.status, "draft");
    assert_eq!(renamed.metadata, *metadata.as_value());
    assert_eq!(
        renamed.content, body,
        "the body is unchanged, byte for byte"
    );
    assert_eq!(renamed.word_count, words);
    assert_eq!(renamed.current_version, 1, "a rename writes no version");
    assert!(renamed.updated_at >= doc.updated_at);

    // A body change moves the version, the word count and the body.
    let replacement = format!("{} et une phrase de plus", incompressible_body(4096));
    document_repo::update_document(
        &pool,
        tenant_id,
        doc.id,
        user_id,
        DocumentUpdate {
            content: Some(&replacement),
            ..Default::default()
        },
    )
    .await
    .expect("the content mutation succeeds");

    let edited = reread(&pool, tenant_id, doc.id).await;
    assert_eq!(edited.title, "Avenant 1", "a save does not rename");
    assert_eq!(edited.metadata, *metadata.as_value());
    assert_eq!(edited.content, replacement);
    assert_eq!(edited.word_count, 6, "the word count follows the new body");
    assert_eq!(edited.current_version, 2);

    let history = ods_doceditor::repository::version_repo::list_versions(&pool, tenant_id, doc.id)
        .await
        .expect("the history is readable");
    assert_eq!(
        history.len(),
        2,
        "creation writes version 1 and the save writes version 2"
    );
    let snapshot =
        ods_doceditor::repository::version_repo::get_version(&pool, tenant_id, doc.id, 2)
            .await
            .expect("version 2 is readable");
    assert_eq!(
        snapshot.content, replacement,
        "the version row holds the body that was saved"
    );
}
