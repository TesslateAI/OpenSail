//! Immutable Release identity: guest-shaped `voie-pack` bytes committed to
//! Blob. Mutating Workspace files after commit cannot change the stored hash.

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use tokio::net::TcpListener;
use uuid::Uuid;
use voie_cloud::session_store::BlobStore;
use voie_cloud::{Config, Kernel};
use voie_pack::{pack, pack_and_stage};

const BLOB_ACCOUNT: &str = "p1-pack-account";
const BLOB_CONTAINER: &str = "p1-pack-container";
const BLOB_KEY_BASE64: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "voie-{label}-{}-{}",
            std::process::id(),
            Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
        TempDir(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn serve_blob(listener: TcpListener, objects: Arc<Mutex<HashMap<String, Vec<u8>>>>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            break;
        };
        let io = hyper_util::rt::TokioIo::new(stream);
        let objects = objects.clone();
        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |request: Request<Incoming>| {
                let objects = objects.clone();
                async move { Ok::<_, Infallible>(blob_handle(request, &objects).await) }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .await;
        });
    }
}

async fn blob_handle(
    request: Request<Incoming>,
    objects: &Mutex<HashMap<String, Vec<u8>>>,
) -> Response<Full<bytes::Bytes>> {
    let path = request.uri().path().to_owned();
    let Some(key) = path
        .strip_prefix(&format!("/{BLOB_CONTAINER}/"))
        .map(str::to_owned)
    else {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(bytes::Bytes::from_static(b"unknown container")))
            .expect("static response");
    };
    match request.method() {
        &Method::PUT => {
            let bytes = request
                .into_body()
                .collect()
                .await
                .expect("blob body")
                .to_bytes();
            let mut objects = objects.lock().expect("blob map lock");
            if let Some(existing) = objects.get(&key) {
                if existing.as_slice() != bytes.as_ref() {
                    return Response::builder()
                        .status(StatusCode::CONFLICT)
                        .body(Full::new(bytes::Bytes::from_static(b"immutable")))
                        .expect("static response");
                }
            }
            objects.insert(key, bytes.to_vec());
            Response::builder()
                .status(StatusCode::CREATED)
                .body(Full::new(bytes::Bytes::from_static(b"")))
                .expect("created")
        }
        &Method::GET => {
            let objects = objects.lock().expect("blob map lock");
            match objects.get(&key) {
                Some(bytes) => Response::builder()
                    .status(StatusCode::OK)
                    .body(Full::new(bytes::Bytes::from(bytes.clone())))
                    .expect("blob get"),
                None => Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Full::new(bytes::Bytes::from_static(b"missing")))
                    .expect("missing"),
            }
        }
        _ => Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .body(Full::new(bytes::Bytes::from_static(b"method")))
            .expect("method"),
    }
}

#[tokio::test]
async fn packed_release_hash_is_identity_after_workspace_mutation() {
    let fixture = TempDir::new("p1-pack");
    let app = fixture.0.join("app");
    std::fs::create_dir_all(app.join("dist")).unwrap();
    std::fs::write(
        app.join("voie.toml"),
        r#"
version = 1
[application]
runtime = "universal-v1"
[build]
command = ["sh", ".voie/build.sh"]
output = "dist"
[run]
command = ["node", "dist/server.js"]
port = 3000
health_path = "/healthz"
"#,
    )
    .unwrap();
    std::fs::write(app.join("dist/server.js"), b"console.log('preview')\n").unwrap();

    let first = pack_and_stage(&app, ".").expect("first pack");
    let staged = std::fs::read(app.join(".voie/tmp/release.tar.zst")).expect("staged artifact");
    assert_eq!(staged, first.artifact);

    let blob_listener = TcpListener::bind("127.0.0.1:0").await.expect("blob bind");
    let blob_port = blob_listener.local_addr().expect("addr").port();
    let objects = Arc::new(Mutex::new(HashMap::new()));
    tokio::spawn(serve_blob(blob_listener, objects.clone()));
    let blob = BlobStore::new(
        BLOB_ACCOUNT.to_owned(),
        BLOB_KEY_BASE64,
        BLOB_CONTAINER.to_owned(),
        format!("http://127.0.0.1:{blob_port}"),
    )
    .expect("blob client");

    let kernel = Kernel::connect(&Config::database_url(
        std::env::var("VOIE_TEST_DATABASE_URL").expect("VOIE_TEST_DATABASE_URL"),
    ))
    .await
    .expect("postgres");
    kernel.migrate().await.expect("migrate");

    let owner = Uuid::new_v4();
    let project = Uuid::new_v4();
    let fabric = Uuid::new_v4();
    let workspace = Uuid::new_v4();
    sqlx::query(
        "insert into users (id, issuer, subject, username, display_name, email, platform_role, status) \
         values ($1, $2, $3, $4, $5, $6, 'user', 'active')",
    )
    .bind(owner)
    .bind(format!("pack-issuer-{}", Uuid::new_v4()))
    .bind("pack-user")
    .bind(format!("pack-{owner}"))
    .bind("pack")
    .bind("pack@example.test")
    .execute(kernel.pool())
    .await
    .unwrap();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("pack-fabric-{fabric}"))
        .execute(kernel.pool())
        .await
        .unwrap();
    sqlx::query(
        "insert into projects (id, owner_user_id, name, kind) values ($1, $2, $3, 'personal')",
    )
    .bind(project)
    .bind(owner)
    .bind(format!("Pack-{project}"))
    .execute(kernel.pool())
    .await
    .unwrap();
    sqlx::query("insert into project_members (project_id, user_id, role) values ($1, $2, 'owner')")
        .bind(project)
        .bind(owner)
        .execute(kernel.pool())
        .await
        .unwrap();
    sqlx::query(
        "insert into workspaces (id, fabric_id, project_id, state, exec_generation, observed_state) \
         values ($1, $2, $3, 'creating', 1, 'ready')",
    )
    .bind(workspace)
    .bind(fabric)
    .bind(project)
    .execute(kernel.pool())
    .await
    .unwrap();

    let apps = voie_cloud::applications::ApplicationStore::new(
        kernel.pool().clone(),
        "console.test".into(),
    );
    let created = apps
        .create(owner, project, workspace, "Tracker", None)
        .await
        .expect("application");
    let manifest = std::fs::read_to_string(app.join("voie.toml")).unwrap();
    let releases = voie_cloud::releases::ReleaseStore::new(kernel.pool().clone());
    let intent = Uuid::new_v4();
    let (begin, _) = releases
        .begin(
            owner,
            created.application.id,
            intent,
            workspace,
            1,
            &manifest,
            None,
        )
        .await
        .expect("begin");
    assert!(matches!(
        begin,
        voie_cloud::releases::BeginRelease::ReadyToDispatch
    ));
    let committed = releases
        .commit_artifact(&blob, intent, &first.artifact, "packed")
        .await
        .expect("commit");
    assert_eq!(committed.state, "ready");
    assert_eq!(
        committed.artifact_hash.as_deref(),
        Some(first.artifact_hash.as_slice())
    );
    let key = committed.artifact_key.clone().expect("artifact key");
    let stored = blob.get_artifact(&key).await.expect("blob get");
    assert_eq!(stored, first.artifact);

    std::fs::write(app.join("dist/server.js"), b"console.log('mutated')\n").unwrap();
    let second = pack(&app, ".").expect("second pack after mutation");
    assert_ne!(
        second.artifact_hash, first.artifact_hash,
        "workspace mutation must change a new pack hash"
    );
    let again = blob
        .get_artifact(&key)
        .await
        .expect("blob still holds original");
    assert_eq!(again, first.artifact);
    let unchanged = releases
        .get(owner, committed.id)
        .await
        .expect("release metadata");
    assert_eq!(
        unchanged.artifact_hash.as_deref(),
        Some(first.artifact_hash.as_slice())
    );

    let retry = releases
        .commit_artifact(&blob, intent, &first.artifact, "packed")
        .await
        .expect("identical commit is identity");
    assert_eq!(retry.id, committed.id);
    assert_eq!(retry.artifact_hash, committed.artifact_hash);
}

#[tokio::test]
async fn database_backup_blob_is_identity_and_never_empty() {
    let blob_listener = TcpListener::bind("127.0.0.1:0").await.expect("blob bind");
    let blob_port = blob_listener.local_addr().expect("addr").port();
    let objects = Arc::new(Mutex::new(HashMap::new()));
    tokio::spawn(serve_blob(blob_listener, objects.clone()));
    let blob = BlobStore::new(
        BLOB_ACCOUNT.to_owned(),
        BLOB_KEY_BASE64,
        BLOB_CONTAINER.to_owned(),
        format!("http://127.0.0.1:{blob_port}"),
    )
    .expect("blob client");

    let kernel = Kernel::connect(&Config::database_url(
        std::env::var("VOIE_TEST_DATABASE_URL").expect("VOIE_TEST_DATABASE_URL"),
    ))
    .await
    .expect("postgres");
    kernel.migrate().await.expect("migrate");

    let owner = Uuid::new_v4();
    let project = Uuid::new_v4();
    let fabric = Uuid::new_v4();
    let workspace = Uuid::new_v4();
    sqlx::query(
        "insert into users (id, issuer, subject, username, display_name, email, platform_role, status) \
         values ($1, $2, $3, $4, $5, $6, 'user', 'active')",
    )
    .bind(owner)
    .bind(format!("bak-issuer-{}", Uuid::new_v4()))
    .bind("bak-user")
    .bind(format!("bak-{owner}"))
    .bind("bak")
    .bind("bak@example.test")
    .execute(kernel.pool())
    .await
    .unwrap();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("bak-fabric-{fabric}"))
        .execute(kernel.pool())
        .await
        .unwrap();
    sqlx::query(
        "insert into projects (id, owner_user_id, name, kind) values ($1, $2, $3, 'personal')",
    )
    .bind(project)
    .bind(owner)
    .bind(format!("Bak-{project}"))
    .execute(kernel.pool())
    .await
    .unwrap();
    sqlx::query("insert into project_members (project_id, user_id, role) values ($1, $2, 'owner')")
        .bind(project)
        .bind(owner)
        .execute(kernel.pool())
        .await
        .unwrap();
    sqlx::query(
        "insert into workspaces (id, fabric_id, project_id, state, exec_generation, observed_state) \
         values ($1, $2, $3, 'creating', 1, 'ready')",
    )
    .bind(workspace)
    .bind(fabric)
    .bind(project)
    .execute(kernel.pool())
    .await
    .unwrap();
    let apps = voie_cloud::applications::ApplicationStore::new(
        kernel.pool().clone(),
        "console.test".into(),
    );
    let created = apps
        .create(owner, project, workspace, "Bak", None)
        .await
        .expect("application");
    let env_id: Uuid = sqlx::query_scalar(
        "select id from application_environments where application_id = $1 and kind = 'dev'",
    )
    .bind(created.application.id)
    .fetch_one(kernel.pool())
    .await
    .unwrap();
    let databases = voie_cloud::databases::DatabaseStore::new(kernel.pool().clone());
    let operation = Uuid::new_v4();
    let hash = voie_cloud::applications::request_hash(&[b"create", env_id.as_bytes()]);
    let database = databases
        .create(owner, env_id, fabric, operation, &hash)
        .await
        .expect("database");

    let dump = b"PGDUMP-REAL-BYTES";
    let digest: [u8; 32] = {
        use sha2::{Digest, Sha256};
        Sha256::digest(dump).into()
    };
    let key = voie_cloud::databases::DatabaseStore::backup_key(database.id, operation);
    blob.put_artifact_if_absent(&key, dump)
        .await
        .expect("backup blob");
    let recorded = databases
        .record_backup(database.id, &key, &digest, dump.len() as i64, "manual")
        .await
        .expect("backup row");
    assert_eq!(recorded.byte_length, dump.len() as i64);
    assert!(!recorded.object_key.is_empty());
    let stored = blob.get_artifact(&key).await.expect("get backup");
    assert_eq!(stored, dump);

    let mutated = b"PGDUMP-MUTATED-BYTES";
    let mutated_digest: [u8; 32] = {
        use sha2::{Digest, Sha256};
        Sha256::digest(mutated).into()
    };
    assert_ne!(digest, mutated_digest);
    let again = blob.get_artifact(&key).await.expect("original remains");
    assert_eq!(again, dump);
    let listed = databases
        .list_backups(owner, database.id)
        .await
        .expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].content_hash, digest);
}
