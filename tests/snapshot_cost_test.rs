//! What an explicit snapshot costs, asked of the wire rather than of the code.
//!
//! `POST /api/v1/documents/{id}/versions` is a request of a few dozen bytes: a
//! document id in the path and, at most, a 500-character comment. Until
//! 2026-09-15 the work it provoked was set by something else entirely — the
//! **stored** body. `version_repo::create_version` selected `content` out of
//! PostgreSQL into this process and bound it straight back as an INSERT
//! parameter, so a snapshot moved the document twice across the connection and
//! held it in memory in between.
//!
//! Nothing in the request bounds that, and `MAX_DOCUMENT_SIZE_MB` does not
//! either: it is checked on bodies a caller **sends** (ADR-009 keeps documents
//! stored above it readable, renamable and snapshottable on purpose), and the
//! dev instance holds six of them today, the largest at 10 485 760 bytes.
//! Measured on the running binary in a cgroup at the deployment's own 512 MiB:
//! twenty concurrent snapshots of a 10 MB document — 20 × 30 bytes of request —
//! **OOM-killed the instance**, twice, with three to four callers getting no
//! answer at all and `/health` gone with them. See ADR-015.
//!
//! # The instrument
//!
//! The obvious measurement — peak RSS of the test process — is useless here:
//! this suite runs thirty-one binaries in parallel against one instance, so a
//! process-wide number says as much about the neighbours as about the code. The
//! narrow-scoped fact is **how many bytes crossed this pool's own socket**, and
//! that is measurable exactly: the pool is pointed at a counting TCP proxy in
//! this process, which forwards to the real PostgreSQL and tallies each
//! direction. Nothing another test does can move that counter.
//!
//! The non-vacuity half is written first and matters as much: the same meter,
//! on the same pool, must **see** a body when one genuinely crosses. A read of
//! the document does exactly that.

mod common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::Executor;
use uuid::Uuid;

use ods_doceditor::domain::metadata::Metadata;
use ods_doceditor::domain::text::Title;
use ods_doceditor::repository::tenant_context::{runtime_role_is_adoptable, session_setup};
use ods_doceditor::repository::{document_repo, version_repo};

/// Big enough that a body crossing the wire cannot be confused with protocol
/// overhead, small enough that the shared dev instance does not care.
const BODY_BYTES: usize = 4_000_000;

/// Bytes counted on one PostgreSQL connection, per direction.
#[derive(Default)]
struct WireMeter {
    to_server: AtomicU64,
    from_server: AtomicU64,
}

impl WireMeter {
    fn reset(&self) {
        self.to_server.store(0, Ordering::SeqCst);
        self.from_server.store(0, Ordering::SeqCst);
    }

    /// `(sent to PostgreSQL, received from PostgreSQL)`.
    fn read(&self) -> (u64, u64) {
        (
            self.to_server.load(Ordering::SeqCst),
            self.from_server.load(Ordering::SeqCst),
        )
    }
}

/// Forward one direction of a connection, tallying what goes through.
async fn pump<R, W>(mut reader: R, mut writer: W, counter: &AtomicU64)
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => {
                let _ = writer.shutdown().await;
                return;
            }
            Ok(n) => {
                counter.fetch_add(n as u64, Ordering::SeqCst);
                if writer.write_all(&buf[..n]).await.is_err() {
                    return;
                }
            }
        }
    }
}

/// A transparent TCP proxy in front of PostgreSQL, returning the port to dial.
async fn start_meter_proxy(host: String, port: u16, meter: Arc<WireMeter>) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the meter proxy binds a local port");
    let local = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        loop {
            let Ok((client, _)) = listener.accept().await else {
                return;
            };
            let meter = meter.clone();
            let host = host.clone();
            tokio::spawn(async move {
                let Ok(server) = tokio::net::TcpStream::connect((host.as_str(), port)).await else {
                    return;
                };
                let _ = client.set_nodelay(true);
                let _ = server.set_nodelay(true);
                let (client_read, client_write) = client.into_split();
                let (server_read, server_write) = server.into_split();
                let up = {
                    let meter = meter.clone();
                    tokio::spawn(
                        async move { pump(client_read, server_write, &meter.to_server).await },
                    )
                };
                let down = tokio::spawn(async move {
                    pump(server_read, client_write, &meter.from_server).await
                });
                let _ = tokio::join!(up, down);
            });
        }
    });

    local
}

/// A serving-shaped pool (search_path, runtime role) whose socket is counted.
async fn metered_pool(meter: Arc<WireMeter>) -> sqlx::PgPool {
    // Migrations, and the answer to "can this database adopt the runtime role",
    // both belong to the administrative pool — and neither must be counted.
    let admin = common::setup_admin_pool().await;
    let adopt = runtime_role_is_adoptable(&admin)
        .await
        .expect("the runtime role is checkable");
    admin.close().await;

    let direct: PgConnectOptions = common::database_url()
        .parse()
        .expect("DATABASE_URL parses as PostgreSQL connect options");
    let proxy_port = start_meter_proxy(
        direct.get_host().to_string(),
        direct.get_port(),
        meter.clone(),
    )
    .await;

    // `sslmode=disable` so the meter counts the protocol and not a TLS record
    // layer; the connection is to 127.0.0.1 either way.
    let through_meter = direct
        .host("127.0.0.1")
        .port(proxy_port)
        .ssl_mode(PgSslMode::Disable);

    let setup = session_setup(adopt);
    PgPoolOptions::new()
        // One connection, so a handshake cannot land in the middle of a
        // measurement: the pool is warmed below, before anything is counted.
        .max_connections(1)
        .after_connect(move |conn, _meta| {
            let setup = setup.clone();
            Box::pin(async move { conn.execute(setup.as_str()).await.map(|_| ()) })
        })
        .connect_with(through_meter)
        .await
        .expect("the metered pool connects through the proxy")
}

#[tokio::test]
async fn an_explicit_snapshot_does_not_pull_the_body_through_this_process() {
    let meter = Arc::new(WireMeter::default());
    let pool = metered_pool(meter.clone()).await;
    // Warm the single connection: the startup packet and the session setup are
    // not part of what a snapshot costs.
    sqlx::query("SELECT 1")
        .execute(&pool)
        .await
        .expect("the metered pool answers");

    let tenant = Uuid::new_v4();
    let author = Uuid::new_v4();
    let body = "ipsum dolor ".repeat(BODY_BYTES.div_ceil(12));
    assert!(body.len() >= BODY_BYTES);

    let doc = document_repo::create_document(
        &pool,
        tenant,
        &Title::parse("Un document volumineux").unwrap(),
        author,
        &Metadata::default(),
        &body,
    )
    .await
    .expect("the fixture document is created");

    // ── The instrument can see a body, on this pool, through this proxy ─────
    // Written first and asserted first: a meter that reads zero for everything
    // would make the real assertion below pass while measuring nothing.
    meter.reset();
    let read_back = document_repo::get_document(&pool, tenant, doc.id)
        .await
        .expect("the document reads back");
    let (_, body_read_from_server) = meter.read();
    assert_eq!(read_back.content.len(), body.len());
    assert!(
        body_read_from_server >= body.len() as u64,
        "the meter is vacuous: reading a {} byte body counted only {} bytes from PostgreSQL",
        body.len(),
        body_read_from_server
    );

    // ── What a snapshot costs ──────────────────────────────────────────────
    meter.reset();
    let snapshot = version_repo::create_version(&pool, tenant, doc.id, author, None, false)
        .await
        .expect("the snapshot is taken");
    let (sent, received) = meter.read();

    let budget = (body.len() / 10) as u64;
    assert!(
        received < budget,
        "taking a snapshot read {received} bytes back from PostgreSQL for a {} byte document — \
         the body is being pulled through this process",
        body.len()
    );
    assert!(
        sent < budget,
        "taking a snapshot sent {sent} bytes to PostgreSQL for a {} byte document — \
         the body is being written back from this process",
        body.len()
    );

    // ── And it is a real snapshot, not a cheap one ─────────────────────────
    assert_eq!(snapshot.version, 2);
    assert_eq!(snapshot.snapshot_size_bytes as usize, body.len());
    let stored = version_repo::get_version(&pool, tenant, doc.id, snapshot.version)
        .await
        .expect("the version reads back");
    assert_eq!(
        stored.content, body,
        "the snapshot must hold the document's body verbatim"
    );
    assert!(!stored.is_auto);

    pool.close().await;
}
