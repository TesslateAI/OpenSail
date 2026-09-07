//! Actor Application quota and bounded Release retention.

use uuid::Uuid;
use voie_cloud::applications::{ApplicationError, ApplicationStore};
use voie_cloud::deployments::DeploymentStore;
use voie_cloud::releases::{
    BeginRelease, MAX_CONCURRENT_RELEASES_PER_PROJECT, MAX_CONCURRENT_RELEASES_PER_USER,
    MAX_FAILED_RELEASE_TOMBSTONES_PER_APPLICATION, MAX_PACKED_ARTIFACT_BYTES,
    MAX_RELEASE_BYTES_PER_APPLICATION, MAX_RELEASE_BYTES_PER_PROJECT, MAX_RELEASES_PER_APPLICATION,
    MAX_RELEASES_PER_PROJECT, ReleaseStore,
};
use voie_cloud::{Config, Kernel, MAX_APPLICATIONS_PER_USER};

const MANIFEST: &str = r#"
version = 1
[application]
runtime = "universal-v1"
[build]
command = ["true"]
output = "."
[run]
command = ["true"]
port = 3000
"#;

async fn kernel() -> Kernel {
    let kernel = Kernel::connect(&Config::database_url(
        std::env::var("VOIE_TEST_DATABASE_URL").expect("VOIE_TEST_DATABASE_URL"),
    ))
    .await
    .expect("postgres");
    kernel.migrate().await.expect("migrate");
    kernel
}

async fn insert_user(kernel: &Kernel, label: &str) -> Uuid {
    let user_id = Uuid::new_v4();
    sqlx::query("insert into users (id, issuer, subject) values ($1, $2, $3)")
        .bind(user_id)
        .bind(format!("{label}-{}", Uuid::new_v4()))
        .bind(label)
        .execute(kernel.pool())
        .await
        .expect("user");
    user_id
}

async fn add_member(kernel: &Kernel, project_id: Uuid, user_id: Uuid) {
    sqlx::query(
        "insert into project_members (project_id, user_id, role) values ($1, $2, 'member')",
    )
    .bind(project_id)
    .bind(user_id)
    .execute(kernel.pool())
    .await
    .expect("member");
}

async fn insert_workspace(kernel: &Kernel, project_id: Uuid, fabric: Uuid, creator: Uuid) -> Uuid {
    let workspace = Uuid::new_v4();
    sqlx::query(
        "insert into workspaces (id, project_id, fabric_id, state, created_by_user_id, exec_generation, observed_state) \
         values ($1, $2, $3, 'creating', $4, 1, 'ready')",
    )
    .bind(workspace)
    .bind(project_id)
    .bind(fabric)
    .bind(creator)
    .execute(kernel.pool())
    .await
    .expect("workspace");
    workspace
}

async fn insert_release_row(
    kernel: &Kernel,
    application_id: Uuid,
    workspace_id: Uuid,
    actor: Uuid,
    state: &str,
    created_offset_secs: i64,
    intent: Uuid,
) -> Uuid {
    let release_id = Uuid::new_v4();
    sqlx::query(
        "insert into application_releases (
            id, application_id, build_intent_id, request_hash, source_workspace_id,
            source_exec_generation, runtime_profile, manifest, manifest_hash,
            state, created_by_user_id, created_at
         ) values (
            $1, $2, $3, $4, $5, 1, 'universal-v1', $6::jsonb, $7, $8, $9,
            now() - make_interval(secs => $10)
         )",
    )
    .bind(release_id)
    .bind(application_id)
    .bind(intent)
    .bind(Uuid::new_v4().as_bytes().as_slice())
    .bind(workspace_id)
    .bind(r#"{"runtime":"universal-v1"}"#)
    .bind(&[3u8; 32].as_slice())
    .bind(state)
    .bind(actor)
    .bind(created_offset_secs)
    .execute(kernel.pool())
    .await
    .expect("release row");
    release_id
}

#[tokio::test]
async fn application_user_quota_charges_the_creating_actor() {
    let kernel = kernel().await;
    let owner = insert_user(&kernel, "quota-owner").await;
    let member = insert_user(&kernel, "quota-member").await;
    let project_a = kernel
        .create_project(Uuid::new_v4(), owner, "quota-app-a", "team")
        .await
        .expect("project a");
    let project_b = kernel
        .create_project(Uuid::new_v4(), owner, "quota-app-b", "team")
        .await
        .expect("project b");
    let project_c = kernel
        .create_project(Uuid::new_v4(), owner, "quota-app-c", "team")
        .await
        .expect("project c");
    add_member(&kernel, project_a.id, member).await;
    add_member(&kernel, project_b.id, member).await;
    add_member(&kernel, project_c.id, member).await;
    let fabric = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("quota-fabric-{fabric}"))
        .execute(kernel.pool())
        .await
        .expect("fabric");

    let apps = ApplicationStore::new(kernel.pool().clone(), "console.test".into());
    for project in [project_a.id, project_b.id] {
        for _ in 0..8 {
            let workspace = insert_workspace(&kernel, project, fabric, owner).await;
            apps.create(member, project, workspace, "Member App", None)
                .await
                .expect("member can fill each Project cap");
        }
    }
    let member_owned: i64 = sqlx::query_scalar(
        "select count(*) from applications where created_by_user_id = $1 and state <> 'deleting'",
    )
    .bind(member)
    .fetch_one(kernel.pool())
    .await
    .expect("member count");
    assert_eq!(member_owned, MAX_APPLICATIONS_PER_USER);

    let owner_workspace = insert_workspace(&kernel, project_c.id, fabric, owner).await;
    apps.create(owner, project_c.id, owner_workspace, "Owner App", None)
        .await
        .expect("Project owner is not charged for a member's Applications");

    let member_workspace = insert_workspace(&kernel, project_c.id, fabric, owner).await;
    let overflow = apps
        .create(member, project_c.id, member_workspace, "Overflow", None)
        .await;
    assert!(
        matches!(
            overflow,
            Err(ApplicationError::Kernel(voie_cloud::KernelError::Quota))
        ),
        "the creating actor is the user-wide Application principal: {overflow:?}"
    );
}

struct AppFixture {
    kernel: Kernel,
    owner: Uuid,
    workspace: Uuid,
    application_id: Uuid,
    dev_id: Uuid,
}

async fn app_fixture(label: &str) -> AppFixture {
    let kernel = kernel().await;
    let owner = insert_user(&kernel, &format!("{label}-owner")).await;
    let project = kernel
        .create_project(
            Uuid::new_v4(),
            owner,
            &format!("{label}-{owner}"),
            "personal",
        )
        .await
        .expect("project");
    let fabric = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("{label}-fabric-{fabric}"))
        .execute(kernel.pool())
        .await
        .expect("fabric");
    let workspace = insert_workspace(&kernel, project.id, fabric, owner).await;
    let created = ApplicationStore::new(kernel.pool().clone(), "console.test".into())
        .create(owner, project.id, workspace, "Retention", None)
        .await
        .expect("application");
    let dev_id = created
        .environments
        .iter()
        .find(|environment| environment.kind == "dev")
        .expect("dev")
        .id;
    AppFixture {
        kernel,
        owner,
        workspace,
        application_id: created.application.id,
        dev_id,
    }
}

#[tokio::test]
async fn release_begin_refuses_at_ready_cap_until_object_is_dropped() {
    let fixture = app_fixture("ready-cap").await;
    let releases = ReleaseStore::new(fixture.kernel.pool().clone());
    let mut ready_ids = Vec::new();
    for index in 0..MAX_RELEASES_PER_APPLICATION {
        let id = insert_release_row(
            &fixture.kernel,
            fixture.application_id,
            fixture.workspace,
            fixture.owner,
            "ready",
            MAX_RELEASES_PER_APPLICATION - index,
            Uuid::new_v4(),
        )
        .await;
        ready_ids.push(id);
    }
    sqlx::query(
        "insert into application_deployments (
            id, environment_id, release_id, deployment_intent_id, request_hash,
            desired_state, desired_revision, created_by_user_id
         ) values ($1, $2, $3, $4, $5, 'absent', 1, $6)",
    )
    .bind(Uuid::new_v4())
    .bind(fixture.dev_id)
    .bind(ready_ids[0])
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4().as_bytes().as_slice())
    .bind(fixture.owner)
    .execute(fixture.kernel.pool())
    .await
    .expect("reference oldest ready");

    let refused = releases
        .begin(
            fixture.owner,
            fixture.application_id,
            Uuid::new_v4(),
            fixture.workspace,
            1,
            MANIFEST,
            None,
        )
        .await;
    assert!(
        matches!(
            refused,
            Err(ApplicationError::Kernel(voie_cloud::KernelError::Quota))
        ),
        "ready cap must refuse instead of deleting Blob-backed rows: {refused:?}"
    );
    let referenced = releases
        .drop_unreferenced(fixture.owner, ready_ids[0], None)
        .await;
    assert!(
        matches!(referenced, Err(ApplicationError::ReleaseInUse)),
        "a referenced Release cannot be dropped: {referenced:?}"
    );
    releases
        .drop_unreferenced(fixture.owner, ready_ids[1], None)
        .await
        .expect("unreferenced ready without Blob can be dropped");
    let (begin, _) = releases
        .begin(
            fixture.owner,
            fixture.application_id,
            Uuid::new_v4(),
            fixture.workspace,
            1,
            MANIFEST,
            None,
        )
        .await
        .expect("begin after explicit drop");
    assert!(
        matches!(begin, BeginRelease::ReadyToDispatch),
        "dropping the unreferenced object frees the retain budget: {begin:?}"
    );
}

#[tokio::test]
async fn release_intent_ledger_keeps_no_replay_after_object_drop() {
    let fixture = app_fixture("intent-ledger").await;
    let releases = ReleaseStore::new(fixture.kernel.pool().clone());

    let unknown_intent = Uuid::new_v4();
    let (first, _) = releases
        .begin(
            fixture.owner,
            fixture.application_id,
            unknown_intent,
            fixture.workspace,
            1,
            MANIFEST,
            None,
        )
        .await
        .expect("reserve unknown intent");
    assert!(matches!(first, BeginRelease::ReadyToDispatch));
    let unknown_id: Uuid =
        sqlx::query_scalar("select id from application_releases where build_intent_id = $1")
            .bind(unknown_intent)
            .fetch_one(fixture.kernel.pool())
            .await
            .expect("unknown row");
    releases
        .unknown(unknown_intent)
        .await
        .expect("mark unknown");
    releases
        .drop_unreferenced(fixture.owner, unknown_id, None)
        .await
        .expect("unknown bulky object can be dropped");
    let bulky_gone: bool =
        sqlx::query_scalar("select exists(select 1 from application_releases where id = $1)")
            .bind(unknown_id)
            .fetch_one(fixture.kernel.pool())
            .await
            .expect("unknown bulky lookup");
    assert!(!bulky_gone, "unknown bulky object was dropped");
    let (replay_unknown, _) = releases
        .begin(
            fixture.owner,
            fixture.application_id,
            unknown_intent,
            fixture.workspace,
            1,
            MANIFEST,
            None,
        )
        .await
        .expect("unknown intent still no-replays");
    assert!(
        matches!(replay_unknown, BeginRelease::OutcomeUnknown),
        "deleting the bulky unknown row must not re-dispatch that intent: {replay_unknown:?}"
    );

    let failed_intent = Uuid::new_v4();
    let failed_manifest = r#"
version = 1
[application]
runtime = "universal-v1"
[build]
command = ["true", "failed"]
output = "."
[run]
command = ["true"]
port = 3000
"#;
    let (failed_begin, _) = releases
        .begin(
            fixture.owner,
            fixture.application_id,
            failed_intent,
            fixture.workspace,
            1,
            failed_manifest,
            None,
        )
        .await
        .expect("reserve failed intent");
    assert!(matches!(failed_begin, BeginRelease::ReadyToDispatch));
    releases
        .fail(failed_intent, "pack failed")
        .await
        .expect("mark failed");
    let failed_id: Uuid =
        sqlx::query_scalar("select id from application_releases where build_intent_id = $1")
            .bind(failed_intent)
            .fetch_one(fixture.kernel.pool())
            .await
            .expect("failed row");
    for index in 0..=MAX_FAILED_RELEASE_TOMBSTONES_PER_APPLICATION {
        insert_release_row(
            &fixture.kernel,
            fixture.application_id,
            fixture.workspace,
            fixture.owner,
            "failed",
            80 + index,
            Uuid::new_v4(),
        )
        .await;
    }
    let (trim, _) = releases
        .begin(
            fixture.owner,
            fixture.application_id,
            Uuid::new_v4(),
            fixture.workspace,
            1,
            r#"
version = 1
[application]
runtime = "universal-v1"
[build]
command = ["true", "trim"]
output = "."
[run]
command = ["true"]
port = 3000
"#,
            None,
        )
        .await
        .expect("new intent after failed-object trim");
    assert!(matches!(trim, BeginRelease::ReadyToDispatch));
    releases
        .drop_unreferenced(fixture.owner, failed_id, None)
        .await
        .expect("failed bulky object can be dropped");
    let (replay_failed, existing) = releases
        .begin(
            fixture.owner,
            fixture.application_id,
            failed_intent,
            fixture.workspace,
            1,
            failed_manifest,
            None,
        )
        .await
        .expect("failed intent still no-replays");
    assert!(
        matches!(replay_failed, BeginRelease::Failed { .. }),
        "failed intent stays terminal after object drop: {replay_failed:?}"
    );
    assert!(
        existing.is_none()
            || existing
                .as_ref()
                .is_some_and(|row| row.build_intent_id == failed_intent),
        "no-replay does not mint a new failed object"
    );
}

const ALT_MANIFEST: &str = r#"
version = 1
[application]
runtime = "universal-v1"
[build]
command = ["true", "alt"]
output = "."
[run]
command = ["true"]
port = 3000
"#;

async fn pack_ready(
    releases: &ReleaseStore,
    fixture: &AppFixture,
    blob: &voie_cloud::session_store::BlobStore,
    manifest: &str,
    artifact: &[u8],
) -> (Uuid, voie_cloud::releases::Release) {
    let intent = Uuid::new_v4();
    let (begin, _) = releases
        .begin(
            fixture.owner,
            fixture.application_id,
            intent,
            fixture.workspace,
            1,
            manifest,
            None,
        )
        .await
        .expect("begin");
    assert!(matches!(begin, BeginRelease::ReadyToDispatch));
    let committed = releases
        .commit_artifact(blob, intent, artifact, "packed")
        .await
        .expect("commit");
    (intent, committed)
}

struct FakeBlob {
    store: voie_cloud::session_store::BlobStore,
    objects: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>>,
}

async fn fake_blob() -> FakeBlob {
    use std::collections::HashMap;
    use std::convert::Infallible;
    use std::sync::{Arc, Mutex};

    use http_body_util::{BodyExt, Full};
    use hyper::body::Incoming;
    use hyper::{Method, Request, Response, StatusCode};
    use tokio::net::TcpListener;
    use voie_cloud::session_store::BlobStore;

    const ACCOUNT: &str = "p1-pack-account";
    const CONTAINER: &str = "p1-pack-container";
    const KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    async fn blob_handle(
        request: Request<Incoming>,
        objects: &Mutex<HashMap<String, Vec<u8>>>,
    ) -> Response<Full<bytes::Bytes>> {
        let path = request.uri().path().to_owned();
        let Some(key) = path
            .strip_prefix(&format!("/{CONTAINER}/"))
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
                    .expect("body")
                    .to_bytes()
                    .to_vec();
                objects.lock().expect("lock").insert(key, bytes);
                Response::builder()
                    .status(StatusCode::CREATED)
                    .body(Full::new(bytes::Bytes::new()))
                    .expect("created")
            }
            &Method::GET => match objects.lock().expect("lock").get(&key) {
                Some(bytes) => Response::builder()
                    .status(StatusCode::OK)
                    .body(Full::new(bytes::Bytes::from(bytes.clone())))
                    .expect("blob get"),
                None => Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Full::new(bytes::Bytes::from_static(b"missing")))
                    .expect("missing"),
            },
            &Method::DELETE => {
                objects.lock().expect("lock").remove(&key);
                Response::builder()
                    .status(StatusCode::ACCEPTED)
                    .body(Full::new(bytes::Bytes::new()))
                    .expect("deleted")
            }
            _ => Response::builder()
                .status(StatusCode::METHOD_NOT_ALLOWED)
                .body(Full::new(bytes::Bytes::from_static(b"method")))
                .expect("method"),
        }
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("blob bind");
    let blob_port = listener.local_addr().expect("addr").port();
    let objects = Arc::new(Mutex::new(HashMap::<String, Vec<u8>>::new()));
    let serve_objects = objects.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let io = hyper_util::rt::TokioIo::new(stream);
            let objects = serve_objects.clone();
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
    });
    FakeBlob {
        store: BlobStore::new(
            ACCOUNT.to_owned(),
            KEY,
            CONTAINER.to_owned(),
            format!("http://127.0.0.1:{blob_port}"),
        )
        .expect("blob client"),
        objects,
    }
}

#[tokio::test]
async fn dropping_a_ready_release_deletes_blob_before_freeing_quota() {
    let fixture = app_fixture("blob-gc").await;
    let fake = fake_blob().await;
    let releases = ReleaseStore::new(fixture.kernel.pool().clone());
    let intent = Uuid::new_v4();
    let (begin, _) = releases
        .begin(
            fixture.owner,
            fixture.application_id,
            intent,
            fixture.workspace,
            1,
            MANIFEST,
            None,
        )
        .await
        .expect("begin");
    assert!(matches!(begin, BeginRelease::ReadyToDispatch));
    let artifact = vec![7u8; 32];
    let committed = releases
        .commit_artifact(&fake.store, intent, &artifact, "packed")
        .await
        .expect("commit");
    let key = committed.artifact_key.clone().expect("artifact key");
    assert!(
        fake.objects.lock().expect("lock").contains_key(&key),
        "Blob holds the artifact before drop"
    );
    let refused = releases
        .drop_unreferenced(fixture.owner, committed.id, None)
        .await;
    assert!(
        refused.is_err(),
        "dropping a Blob-backed Release without deleting Blob must fail closed: {refused:?}"
    );
    let still: bool =
        sqlx::query_scalar("select exists(select 1 from application_releases where id = $1)")
            .bind(committed.id)
            .fetch_one(fixture.kernel.pool())
            .await
            .expect("row lookup");
    assert!(still, "SQL row remains while Blob delete is skipped");
    assert!(
        fake.objects.lock().expect("lock").contains_key(&key),
        "Blob remains when drop is refused"
    );
    releases
        .drop_unreferenced(fixture.owner, committed.id, Some(&fake.store))
        .await
        .expect("drop after Blob delete");
    assert!(
        !fake.objects.lock().expect("lock").contains_key(&key),
        "Blob object is gone before quota is freed"
    );
    let gone: bool =
        sqlx::query_scalar("select exists(select 1 from application_releases where id = $1)")
            .bind(committed.id)
            .fetch_one(fixture.kernel.pool())
            .await
            .expect("dropped row");
    assert!(!gone, "bulky Release row is removed after Blob delete");
    let class: String = sqlx::query_scalar(
        "select class from application_release_intents where build_intent_id = $1",
    )
    .bind(intent)
    .fetch_one(fixture.kernel.pool())
    .await
    .expect("ledger remains");
    assert_eq!(class, "ready", "intent tombstone survives object drop");
    let (replay, _) = releases
        .begin(
            fixture.owner,
            fixture.application_id,
            intent,
            fixture.workspace,
            1,
            MANIFEST,
            None,
        )
        .await
        .expect("dropped ready intent still no-replays");
    assert!(
        matches!(replay, BeginRelease::Ready { .. }),
        "the same intent must not pack again: {replay:?}"
    );
}

#[tokio::test]
async fn dropping_one_release_keeps_a_shared_content_addressed_blob() {
    let fixture = app_fixture("shared-blob").await;
    let fake = fake_blob().await;
    let releases = ReleaseStore::new(fixture.kernel.pool().clone());
    let artifact = vec![9u8; 32];
    let (intent_a, first) = pack_ready(&releases, &fixture, &fake.store, MANIFEST, &artifact).await;
    let (intent_b, second) =
        pack_ready(&releases, &fixture, &fake.store, ALT_MANIFEST, &artifact).await;
    assert_ne!(first.id, second.id);
    assert_ne!(intent_a, intent_b);
    let key = first.artifact_key.clone().expect("shared key");
    assert_eq!(second.artifact_key.as_deref(), Some(key.as_str()));
    assert!(
        fake.objects.lock().expect("lock").contains_key(&key),
        "identical artifact bytes share one Blob object"
    );

    releases
        .drop_unreferenced(fixture.owner, first.id, Some(&fake.store))
        .await
        .expect("drop first sharer");
    let first_gone: bool =
        sqlx::query_scalar("select exists(select 1 from application_releases where id = $1)")
            .bind(first.id)
            .fetch_one(fixture.kernel.pool())
            .await
            .expect("first lookup");
    assert!(!first_gone, "first bulky row is removed");
    let second_row = releases
        .get(fixture.owner, second.id)
        .await
        .expect("second Release stays ready");
    assert_eq!(second_row.state, "ready");
    assert_eq!(second_row.artifact_key.as_deref(), Some(key.as_str()));
    assert!(
        fake.objects.lock().expect("lock").contains_key(&key),
        "shared Blob must survive while another Release still names it"
    );

    releases
        .drop_unreferenced(fixture.owner, second.id, Some(&fake.store))
        .await
        .expect("drop last sharer");
    assert!(
        !fake.objects.lock().expect("lock").contains_key(&key),
        "Blob is deleted only after the last sharer is dropped"
    );
}

#[tokio::test]
async fn drop_holds_the_release_row_until_blob_delete_so_deploy_cannot_orphan_it() {
    let fixture = app_fixture("drop-lock").await;
    let fake = fake_blob().await;
    let releases = ReleaseStore::new(fixture.kernel.pool().clone());
    let (_, committed) = pack_ready(&releases, &fixture, &fake.store, MANIFEST, &[11u8; 32]).await;
    let key = committed.artifact_key.clone().expect("artifact key");

    let mut held = fixture.kernel.pool().begin().await.expect("hold tx");
    sqlx::query("select id from application_releases where id = $1 for update")
        .bind(committed.id)
        .fetch_one(&mut *held)
        .await
        .expect("lock Release row");

    let drop_owner = fixture.owner;
    let drop_id = committed.id;
    let drop_store = fake.store.clone();
    let drop_releases = ReleaseStore::new(fixture.kernel.pool().clone());
    let drop_task = tokio::spawn(async move {
        drop_releases
            .drop_unreferenced(drop_owner, drop_id, Some(&drop_store))
            .await
    });

    sqlx::query(
        "insert into application_deployments (
            id, environment_id, release_id, deployment_intent_id, request_hash,
            desired_state, desired_revision, created_by_user_id
         ) values ($1, $2, $3, $4, $5, 'absent', 1, $6)",
    )
    .bind(Uuid::new_v4())
    .bind(fixture.dev_id)
    .bind(committed.id)
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4().as_bytes().as_slice())
    .bind(fixture.owner)
    .execute(&mut *held)
    .await
    .expect("deploy insert while Release is locked");
    held.commit().await.expect("commit deploy winner");

    let dropped = tokio::time::timeout(std::time::Duration::from_secs(5), drop_task)
        .await
        .expect("drop must not deadlock on the Release row lock")
        .expect("join drop");
    assert!(
        matches!(dropped, Err(ApplicationError::ReleaseInUse)),
        "drop must see the Deployment that won the row lock: {dropped:?}"
    );
    assert!(
        fake.objects.lock().expect("lock").contains_key(&key),
        "Blob must remain when drop loses to a concurrent Deployment"
    );
    let still: bool =
        sqlx::query_scalar("select exists(select 1 from application_releases where id = $1)")
            .bind(committed.id)
            .fetch_one(fixture.kernel.pool())
            .await
            .expect("row lookup");
    assert!(still, "Release row remains referenced");
}

#[tokio::test]
async fn application_delete_reclaims_every_release_blob() {
    let fixture = app_fixture("app-reclaim").await;
    let fake = fake_blob().await;
    let releases = ReleaseStore::new(fixture.kernel.pool().clone());
    let (intent_a, first) =
        pack_ready(&releases, &fixture, &fake.store, MANIFEST, &[13u8; 32]).await;
    let (intent_b, second) =
        pack_ready(&releases, &fixture, &fake.store, ALT_MANIFEST, &[17u8; 32]).await;
    let key_a = first.artifact_key.clone().expect("first key");
    let key_b = second.artifact_key.clone().expect("second key");
    assert_ne!(key_a, key_b);

    let refused = releases
        .reclaim_application_blobs(fixture.application_id, None)
        .await;
    assert!(
        matches!(
            refused,
            Err(ApplicationError::Kernel(voie_cloud::KernelError::Database))
        ),
        "reclaim without a Blob client must fail closed while keys remain: {refused:?}"
    );
    assert!(
        fake.objects.lock().expect("lock").contains_key(&key_a)
            && fake.objects.lock().expect("lock").contains_key(&key_b),
        "Blobs stay when reclaim is refused"
    );

    releases
        .reclaim_application_blobs(fixture.application_id, Some(&fake.store))
        .await
        .expect("reclaim Application blobs");
    assert!(
        !fake.objects.lock().expect("lock").contains_key(&key_a)
            && !fake.objects.lock().expect("lock").contains_key(&key_b),
        "every Application Blob is deleted before delete settles"
    );
    let leftover: i64 = sqlx::query_scalar(
        "select count(*) from application_releases \
         where application_id = $1 and artifact_key is not null",
    )
    .bind(fixture.application_id)
    .fetch_one(fixture.kernel.pool())
    .await
    .expect("cleared keys");
    assert_eq!(leftover, 0, "artifact_key is cleared after Blob delete");
    let bytes: i64 = sqlx::query_scalar(
        "select coalesce(sum(artifact_bytes), 0)::bigint from application_releases \
         where application_id = $1",
    )
    .bind(fixture.application_id)
    .fetch_one(fixture.kernel.pool())
    .await
    .expect("byte quota");
    assert_eq!(bytes, 0, "artifact_bytes no longer consume storage quota");
    let bulky: i64 =
        sqlx::query_scalar("select count(*) from application_releases where application_id = $1")
            .bind(fixture.application_id)
            .fetch_one(fixture.kernel.pool())
            .await
            .expect("bulky rows");
    assert_eq!(bulky, 2, "bulky rows stay for Deployment FKs");
    for intent in [intent_a, intent_b] {
        let class: String = sqlx::query_scalar(
            "select class from application_release_intents where build_intent_id = $1",
        )
        .bind(intent)
        .fetch_one(fixture.kernel.pool())
        .await
        .expect("ledger remains");
        assert_eq!(class, "ready", "intent ledger survives Application reclaim");
    }

    ApplicationStore::new(fixture.kernel.pool().clone(), "console.test".into())
        .commit_delete(fixture.application_id)
        .await
        .expect("commit Application delete after Blob reclaim");
    let state: String = sqlx::query_scalar("select state from applications where id = $1")
        .bind(fixture.application_id)
        .fetch_one(fixture.kernel.pool())
        .await
        .expect("application state");
    assert_eq!(state, "deleting");
    let workspace_desired: String =
        sqlx::query_scalar("select desired_state from workspaces where id = $1")
            .bind(fixture.workspace)
            .fetch_one(fixture.kernel.pool())
            .await
            .expect("workspace desired");
    assert_eq!(
        workspace_desired, "deleted",
        "Application delete must free the Workspace quota row"
    );
    let intents: i64 = sqlx::query_scalar(
        "select count(*) from application_release_intents where application_id = $1",
    )
    .bind(fixture.application_id)
    .fetch_one(fixture.kernel.pool())
    .await
    .expect("ledger after delete");
    assert_eq!(intents, 2, "durable intent ledger remains after delete");
}

async fn fence_delete(apps: &ApplicationStore, owner: Uuid, application_id: Uuid) {
    let refused = apps.plan_delete(owner, application_id, None).await;
    let approval_id = match refused {
        Err(ApplicationError::ApprovalRequired(id)) => id,
        other => panic!("delete requires typed approval: {other:?}"),
    };
    apps.accept_pending_approval(owner, approval_id)
        .await
        .expect("accept delete_application");
    apps.plan_delete(owner, application_id, Some(approval_id))
        .await
        .expect("fence Application deleting");
}

#[tokio::test]
async fn application_delete_cleanup_includes_the_workspace() {
    let fixture = app_fixture("delete-ws").await;
    let apps = ApplicationStore::new(fixture.kernel.pool().clone(), "console.test".into());
    let refused = apps
        .plan_delete(fixture.owner, fixture.application_id, None)
        .await;
    let approval_id = match refused {
        Err(ApplicationError::ApprovalRequired(id)) => id,
        other => panic!("delete requires typed approval: {other:?}"),
    };
    apps.accept_pending_approval(fixture.owner, approval_id)
        .await
        .expect("accept delete_application");
    let cleanup = apps
        .plan_delete(fixture.owner, fixture.application_id, Some(approval_id))
        .await
        .expect("delete cleanup");
    assert_eq!(
        cleanup.workspace_id,
        Some(fixture.workspace),
        "deleting an Application must reclaim its Workspace"
    );
}

#[tokio::test]
async fn deleting_application_is_fenced_before_cleanup() {
    let fixture = app_fixture("delete-fence").await;
    let apps = ApplicationStore::new(fixture.kernel.pool().clone(), "console.test".into());
    let releases = ReleaseStore::new(fixture.kernel.pool().clone());
    let inflight = Uuid::new_v4();
    let (begin, _) = releases
        .begin(
            fixture.owner,
            fixture.application_id,
            inflight,
            fixture.workspace,
            1,
            MANIFEST,
            None,
        )
        .await
        .expect("begin in-flight Release");
    assert!(matches!(begin, BeginRelease::ReadyToDispatch));
    let ready_id = insert_release_row(
        &fixture.kernel,
        fixture.application_id,
        fixture.workspace,
        fixture.owner,
        "ready",
        1,
        Uuid::new_v4(),
    )
    .await;

    fence_delete(&apps, fixture.owner, fixture.application_id).await;
    let state: String = sqlx::query_scalar("select state from applications where id = $1")
        .bind(fixture.application_id)
        .fetch_one(fixture.kernel.pool())
        .await
        .expect("fenced state");
    assert_eq!(state, "deleting", "approved delete records deleting first");
    let charged: i64 = sqlx::query_scalar(
        "select count(*) from applications where created_by_user_id = $1 and state <> 'deleting'",
    )
    .bind(fixture.owner)
    .fetch_one(fixture.kernel.pool())
    .await
    .expect("quota count");
    assert_eq!(charged, 0, "a deleting Application leaves the actor quota");

    let refused_begin = releases
        .begin(
            fixture.owner,
            fixture.application_id,
            Uuid::new_v4(),
            fixture.workspace,
            1,
            ALT_MANIFEST,
            None,
        )
        .await;
    assert!(
        matches!(refused_begin, Err(ApplicationError::NotFound)),
        "new Releases cannot be reserved on a deleting Application: {refused_begin:?}"
    );
    let refused_complete = releases
        .complete(inflight, "k", &[5u8; 32], 5, "packed")
        .await;
    assert!(
        matches!(refused_complete, Err(ApplicationError::NotFound)),
        "in-flight publication cannot complete after the delete fence: {refused_complete:?}"
    );
    let refused_deploy = DeploymentStore::new(fixture.kernel.pool().clone())
        .deploy(
            fixture.owner,
            fixture.dev_id,
            ready_id,
            Uuid::new_v4(),
            None,
        )
        .await;
    assert!(
        matches!(refused_deploy, Err(ApplicationError::NotFound)),
        "new Deployments cannot be created on a deleting Application: {refused_deploy:?}"
    );

    let project_id: Uuid = sqlx::query_scalar("select project_id from applications where id = $1")
        .bind(fixture.application_id)
        .fetch_one(fixture.kernel.pool())
        .await
        .expect("project");
    let fabric: Uuid = sqlx::query_scalar("select fabric_id from workspaces where id = $1")
        .bind(fixture.workspace)
        .fetch_one(fixture.kernel.pool())
        .await
        .expect("fabric");
    let replacement_workspace =
        insert_workspace(&fixture.kernel, project_id, fabric, fixture.owner).await;
    let created = apps
        .create(
            fixture.owner,
            project_id,
            replacement_workspace,
            "Next",
            None,
        )
        .await
        .expect("the freed Application slot can be used again");
    assert_ne!(
        created.application.id, fixture.application_id,
        "the replacement is a distinct live Application"
    );
}

#[tokio::test]
async fn list_keeps_deleting_application_until_workspace_is_reclaimed() {
    let fixture = app_fixture("list-deleting").await;
    let apps = ApplicationStore::new(fixture.kernel.pool().clone(), "console.test".into());
    let project_id: Uuid = sqlx::query_scalar("select project_id from applications where id = $1")
        .bind(fixture.application_id)
        .fetch_one(fixture.kernel.pool())
        .await
        .expect("project");
    fence_delete(&apps, fixture.owner, fixture.application_id).await;

    let listed = apps
        .list(fixture.owner, project_id)
        .await
        .expect("occupancy-visible list");
    assert!(
        listed
            .iter()
            .any(|item| { item.id == fixture.application_id && item.state == "deleting" }),
        "deleting Application must stay listed while its Workspace is still charged: {listed:?}"
    );
    let reuse = apps
        .create(fixture.owner, project_id, fixture.workspace, "Reuse", None)
        .await;
    assert!(
        matches!(reuse, Err(ApplicationError::WorkspaceBusy)),
        "create must not share a Workspace still occupied by a deleting Application: {reuse:?}"
    );

    apps.commit_delete(fixture.application_id)
        .await
        .expect("commit delete");
    let listed = apps
        .list(fixture.owner, project_id)
        .await
        .expect("reclaimed list");
    assert!(
        listed.iter().all(|item| item.id != fixture.application_id),
        "completed delete drops the Application once the Workspace is deleted: {listed:?}"
    );
}

#[tokio::test]
async fn deleting_sibling_does_not_keep_the_workspace_charged() {
    let fixture = app_fixture("delete-sibling").await;
    let apps = ApplicationStore::new(fixture.kernel.pool().clone(), "console.test".into());
    let sibling = Uuid::new_v4();
    sqlx::query(
        "insert into applications \
         (id, project_id, workspace_id, name, slug, root_path, runtime_profile, state, created_by_user_id) \
         select $1, project_id, workspace_id, 'Sibling deleting', $2, '.', runtime_profile, 'deleting', created_by_user_id \
         from applications where id = $3",
    )
    .bind(sibling)
    .bind(format!("sib-{}", Uuid::new_v4().simple()))
    .bind(fixture.application_id)
    .execute(fixture.kernel.pool())
    .await
    .expect("sibling deleting row");

    let refused = apps
        .plan_delete(fixture.owner, fixture.application_id, None)
        .await;
    let approval_id = match refused {
        Err(ApplicationError::ApprovalRequired(id)) => id,
        other => panic!("delete requires typed approval: {other:?}"),
    };
    apps.accept_pending_approval(fixture.owner, approval_id)
        .await
        .expect("accept delete_application");
    let cleanup = apps
        .plan_delete(fixture.owner, fixture.application_id, Some(approval_id))
        .await
        .expect("fence Application deleting");
    assert_eq!(
        cleanup.workspace_id,
        Some(fixture.workspace),
        "a deleting sibling must not pin the Workspace reservation"
    );
    apps.commit_delete(fixture.application_id)
        .await
        .expect("commit delete");
    let workspace_desired: String =
        sqlx::query_scalar("select desired_state from workspaces where id = $1")
            .bind(fixture.workspace)
            .fetch_one(fixture.kernel.pool())
            .await
            .expect("workspace desired");
    assert_eq!(
        workspace_desired, "deleted",
        "Application delete must free the Workspace despite leftover deleting rows"
    );
}

async fn sibling_application(
    fixture: &AppFixture,
    name: &str,
    _slug_prefix: &str,
) -> (Uuid, Uuid, Uuid) {
    let apps = ApplicationStore::new(fixture.kernel.pool().clone(), "console.test".into());
    let project_id: Uuid = sqlx::query_scalar("select project_id from applications where id = $1")
        .bind(fixture.application_id)
        .fetch_one(fixture.kernel.pool())
        .await
        .expect("project");
    let fabric: Uuid = sqlx::query_scalar("select fabric_id from workspaces where id = $1")
        .bind(fixture.workspace)
        .fetch_one(fixture.kernel.pool())
        .await
        .expect("fabric");
    let workspace = insert_workspace(&fixture.kernel, project_id, fabric, fixture.owner).await;
    let created = apps
        .create(fixture.owner, project_id, workspace, name, None)
        .await
        .expect("sibling application");
    (project_id, workspace, created.application.id)
}

async fn begin_inflight(
    releases: &ReleaseStore,
    owner: Uuid,
    application_id: Uuid,
    workspace: Uuid,
    manifest: &str,
) -> Uuid {
    let intent = Uuid::new_v4();
    let (begin, _) = releases
        .begin(owner, application_id, intent, workspace, 1, manifest, None)
        .await
        .expect("begin inflight");
    assert!(matches!(begin, BeginRelease::ReadyToDispatch));
    intent
}

#[tokio::test]
async fn release_bytes_reserve_the_pack_ceiling_for_inflight_builds() {
    let fixture = app_fixture("byte-reserve").await;
    let releases = ReleaseStore::new(fixture.kernel.pool().clone());
    let ready = insert_release_row(
        &fixture.kernel,
        fixture.application_id,
        fixture.workspace,
        fixture.owner,
        "ready",
        1,
        Uuid::new_v4(),
    )
    .await;
    sqlx::query("update application_releases set artifact_bytes = $2 where id = $1")
        .bind(ready)
        .bind(MAX_RELEASE_BYTES_PER_APPLICATION - MAX_PACKED_ARTIFACT_BYTES + 1)
        .execute(fixture.kernel.pool())
        .await
        .expect("near-full Application budget");
    let refused = releases
        .begin(
            fixture.owner,
            fixture.application_id,
            Uuid::new_v4(),
            fixture.workspace,
            1,
            MANIFEST,
            None,
        )
        .await;
    assert!(
        matches!(
            refused,
            Err(ApplicationError::Kernel(voie_cloud::KernelError::Quota))
        ),
        "an in-flight pack must reserve the maximum artifact size atomically: {refused:?}"
    );
}

#[tokio::test]
async fn project_release_count_and_bytes_are_capped() {
    let fixture = app_fixture("project-retain").await;
    let releases = ReleaseStore::new(fixture.kernel.pool().clone());
    let (_, sibling_workspace, sibling_id) =
        sibling_application(&fixture, "Retain Sib", "rsib").await;
    let packed = insert_release_row(
        &fixture.kernel,
        sibling_id,
        sibling_workspace,
        fixture.owner,
        "ready",
        1,
        Uuid::new_v4(),
    )
    .await;
    sqlx::query("update application_releases set artifact_bytes = $2 where id = $1")
        .bind(packed)
        .bind(MAX_RELEASE_BYTES_PER_PROJECT - MAX_PACKED_ARTIFACT_BYTES + 1)
        .execute(fixture.kernel.pool())
        .await
        .expect("near-full Project byte budget");
    let refused_bytes = releases
        .begin(
            fixture.owner,
            fixture.application_id,
            Uuid::new_v4(),
            fixture.workspace,
            1,
            MANIFEST,
            None,
        )
        .await;
    assert!(
        matches!(
            refused_bytes,
            Err(ApplicationError::Kernel(voie_cloud::KernelError::Quota))
        ),
        "Project Release-byte cap applies across Applications: {refused_bytes:?}"
    );

    sqlx::query("update application_releases set artifact_bytes = 0 where id = $1")
        .bind(packed)
        .execute(fixture.kernel.pool())
        .await
        .expect("clear Project byte budget");
    for offset in 1..MAX_RELEASES_PER_PROJECT {
        insert_release_row(
            &fixture.kernel,
            sibling_id,
            sibling_workspace,
            fixture.owner,
            "ready",
            offset,
            Uuid::new_v4(),
        )
        .await;
    }
    let refused_count = releases
        .begin(
            fixture.owner,
            fixture.application_id,
            Uuid::new_v4(),
            fixture.workspace,
            1,
            MANIFEST,
            None,
        )
        .await;
    assert!(
        matches!(
            refused_count,
            Err(ApplicationError::Kernel(voie_cloud::KernelError::Quota))
        ),
        "Project Release-count cap applies across Applications: {refused_count:?}"
    );
}

#[tokio::test]
async fn concurrent_builds_are_capped_per_project_and_per_actor() {
    let fixture = app_fixture("build-project").await;
    let apps = ApplicationStore::new(fixture.kernel.pool().clone(), "console.test".into());
    let releases = ReleaseStore::new(fixture.kernel.pool().clone());
    let project_id: Uuid = sqlx::query_scalar("select project_id from applications where id = $1")
        .bind(fixture.application_id)
        .fetch_one(fixture.kernel.pool())
        .await
        .expect("project");
    let fabric: Uuid = sqlx::query_scalar("select fabric_id from workspaces where id = $1")
        .bind(fixture.workspace)
        .fetch_one(fixture.kernel.pool())
        .await
        .expect("fabric");
    let sibling_workspace =
        insert_workspace(&fixture.kernel, project_id, fabric, fixture.owner).await;
    let sibling = apps
        .create(
            fixture.owner,
            project_id,
            sibling_workspace,
            "Sibling",
            None,
        )
        .await
        .expect("sibling application");
    assert_eq!(MAX_CONCURRENT_RELEASES_PER_PROJECT, 2);
    begin_inflight(
        &releases,
        fixture.owner,
        fixture.application_id,
        fixture.workspace,
        MANIFEST,
    )
    .await;
    begin_inflight(
        &releases,
        fixture.owner,
        fixture.application_id,
        fixture.workspace,
        ALT_MANIFEST,
    )
    .await;
    let refused_project = releases
        .begin(
            fixture.owner,
            sibling.application.id,
            Uuid::new_v4(),
            sibling_workspace,
            1,
            MANIFEST,
            None,
        )
        .await;
    assert!(
        matches!(
            refused_project,
            Err(ApplicationError::Kernel(voie_cloud::KernelError::Quota))
        ),
        "Project concurrent-build cap applies across Applications: {refused_project:?}"
    );

    let other_project = fixture
        .kernel
        .create_project(
            Uuid::new_v4(),
            fixture.owner,
            &format!("build-user-{}", fixture.owner),
            "personal",
        )
        .await
        .expect("second project");
    let other_workspace =
        insert_workspace(&fixture.kernel, other_project.id, fabric, fixture.owner).await;
    let other = apps
        .create(
            fixture.owner,
            other_project.id,
            other_workspace,
            "Other",
            None,
        )
        .await
        .expect("application on second Project");
    assert_eq!(MAX_CONCURRENT_RELEASES_PER_USER, 2);
    let refused_user = releases
        .begin(
            fixture.owner,
            other.application.id,
            Uuid::new_v4(),
            other_workspace,
            1,
            MANIFEST,
            None,
        )
        .await;
    assert!(
        matches!(
            refused_user,
            Err(ApplicationError::Kernel(voie_cloud::KernelError::Quota))
        ),
        "actor concurrent-build cap applies across Projects: {refused_user:?}"
    );
}

#[tokio::test]
async fn concurrent_deploys_are_capped_per_project_and_per_actor() {
    use voie_cloud::deployments::{
        DeploymentStore, MAX_CONCURRENT_DEPLOYMENTS_PER_PROJECT,
        MAX_CONCURRENT_DEPLOYMENTS_PER_USER,
    };

    let fixture = app_fixture("deploy-project").await;
    let apps = ApplicationStore::new(fixture.kernel.pool().clone(), "console.test".into());
    let deployments = DeploymentStore::new(fixture.kernel.pool().clone());
    let project_id: Uuid = sqlx::query_scalar("select project_id from applications where id = $1")
        .bind(fixture.application_id)
        .fetch_one(fixture.kernel.pool())
        .await
        .expect("project");
    let fabric: Uuid = sqlx::query_scalar("select fabric_id from workspaces where id = $1")
        .bind(fixture.workspace)
        .fetch_one(fixture.kernel.pool())
        .await
        .expect("fabric");
    let sibling_workspace =
        insert_workspace(&fixture.kernel, project_id, fabric, fixture.owner).await;
    let sibling = apps
        .create(
            fixture.owner,
            project_id,
            sibling_workspace,
            "Deploy Sib",
            None,
        )
        .await
        .expect("sibling application");
    let sibling_dev = sibling
        .environments
        .iter()
        .find(|environment| environment.kind == "dev")
        .expect("sibling dev")
        .id;
    let first_release = insert_release_row(
        &fixture.kernel,
        fixture.application_id,
        fixture.workspace,
        fixture.owner,
        "ready",
        1,
        Uuid::new_v4(),
    )
    .await;
    let second_release = insert_release_row(
        &fixture.kernel,
        sibling.application.id,
        sibling_workspace,
        fixture.owner,
        "ready",
        1,
        Uuid::new_v4(),
    )
    .await;
    deployments
        .deploy(
            fixture.owner,
            fixture.dev_id,
            first_release,
            Uuid::new_v4(),
            None,
        )
        .await
        .expect("first in-flight deploy");
    deployments
        .deploy(
            fixture.owner,
            sibling_dev,
            second_release,
            Uuid::new_v4(),
            None,
        )
        .await
        .expect("second in-flight deploy");
    assert_eq!(MAX_CONCURRENT_DEPLOYMENTS_PER_PROJECT, 2);
    let third_workspace =
        insert_workspace(&fixture.kernel, project_id, fabric, fixture.owner).await;
    let third = apps
        .create(
            fixture.owner,
            project_id,
            third_workspace,
            "Deploy Third",
            None,
        )
        .await
        .expect("third application");
    let third_dev = third
        .environments
        .iter()
        .find(|environment| environment.kind == "dev")
        .expect("third dev")
        .id;
    let third_release = insert_release_row(
        &fixture.kernel,
        third.application.id,
        third_workspace,
        fixture.owner,
        "ready",
        1,
        Uuid::new_v4(),
    )
    .await;
    let refused_project = deployments
        .deploy(
            fixture.owner,
            third_dev,
            third_release,
            Uuid::new_v4(),
            None,
        )
        .await;
    assert!(
        matches!(refused_project, Err(ApplicationError::InFlightQuota)),
        "Project concurrent-deploy cap applies across Applications: {refused_project:?}"
    );

    let other_project = fixture
        .kernel
        .create_project(
            Uuid::new_v4(),
            fixture.owner,
            &format!("deploy-user-{}", fixture.owner),
            "personal",
        )
        .await
        .expect("second project");
    let other_workspace =
        insert_workspace(&fixture.kernel, other_project.id, fabric, fixture.owner).await;
    let other = apps
        .create(
            fixture.owner,
            other_project.id,
            other_workspace,
            "Deploy Other",
            None,
        )
        .await
        .expect("application on second Project");
    let other_dev = other
        .environments
        .iter()
        .find(|environment| environment.kind == "dev")
        .expect("other dev")
        .id;
    let other_release = insert_release_row(
        &fixture.kernel,
        other.application.id,
        other_workspace,
        fixture.owner,
        "ready",
        1,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(MAX_CONCURRENT_DEPLOYMENTS_PER_USER, 2);
    let refused_user = deployments
        .deploy(
            fixture.owner,
            other_dev,
            other_release,
            Uuid::new_v4(),
            None,
        )
        .await;
    assert!(
        matches!(refused_user, Err(ApplicationError::InFlightQuota)),
        "actor concurrent-deploy cap applies across Projects: {refused_user:?}"
    );
}

#[tokio::test]
async fn failed_release_streams_do_not_occupy_inflight_deploy_capacity() {
    let fixture = app_fixture("stream-fail-quota").await;
    let apps = ApplicationStore::new(fixture.kernel.pool().clone(), "console.test".into());
    let deployments = DeploymentStore::new(fixture.kernel.pool().clone());
    let project_id: Uuid = sqlx::query_scalar("select project_id from applications where id = $1")
        .bind(fixture.application_id)
        .fetch_one(fixture.kernel.pool())
        .await
        .expect("project");
    let fabric: Uuid = sqlx::query_scalar("select fabric_id from workspaces where id = $1")
        .bind(fixture.workspace)
        .fetch_one(fixture.kernel.pool())
        .await
        .expect("fabric");
    let first = insert_release_row(
        &fixture.kernel,
        fixture.application_id,
        fixture.workspace,
        fixture.owner,
        "ready",
        2,
        Uuid::new_v4(),
    )
    .await;
    let second = insert_release_row(
        &fixture.kernel,
        fixture.application_id,
        fixture.workspace,
        fixture.owner,
        "ready",
        1,
        Uuid::new_v4(),
    )
    .await;
    for release_id in [first, second] {
        sqlx::query(
            "insert into application_deployments (
                id, environment_id, release_id, deployment_intent_id, request_hash,
                desired_state, desired_revision, created_by_user_id,
                last_error_code, proven, observed_state
             ) values ($1, $2, $3, $4, $5, 'running', 1, $6, 'release_stream_failed', false, 'needs_release_stream')",
        )
        .bind(Uuid::new_v4())
        .bind(fixture.dev_id)
        .bind(release_id)
        .bind(Uuid::new_v4())
        .bind(Uuid::new_v4().as_bytes().as_slice())
        .bind(fixture.owner)
        .execute(fixture.kernel.pool())
        .await
        .expect("failed stream candidate");
    }
    let sibling_workspace =
        insert_workspace(&fixture.kernel, project_id, fabric, fixture.owner).await;
    let sibling = apps
        .create(
            fixture.owner,
            project_id,
            sibling_workspace,
            "Stream Sibling",
            None,
        )
        .await
        .expect("sibling application");
    let sibling_dev = sibling
        .environments
        .iter()
        .find(|environment| environment.kind == "dev")
        .expect("sibling dev")
        .id;
    let next = insert_release_row(
        &fixture.kernel,
        sibling.application.id,
        sibling_workspace,
        fixture.owner,
        "ready",
        0,
        Uuid::new_v4(),
    )
    .await;
    deployments
        .deploy(fixture.owner, sibling_dev, next, Uuid::new_v4(), None)
        .await
        .expect("definite stream failures must not block a new deploy");
}
