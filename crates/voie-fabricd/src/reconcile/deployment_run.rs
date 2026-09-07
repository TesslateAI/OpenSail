//! Observe, plan, and execute one Deployment until WaitPod, Converged, or bound.

use crate::observe::classify_deployment_pod;
use crate::product_realize::{
    app_env_secret_name, app_intent_from_spec, app_pod_name, app_service_name,
    application_postgres_policy_yaml, deployment_volume_for_lv, deployment_volume_name,
};
use crate::reconcile::deployment::{
    DeploymentAction, DeploymentLocal, DeploymentObserved, DeploymentPod, plan_deployment,
};
use crate::specs::deployment::DeploymentSpec;
use crate::{Fabric, FabricError, VolumeKind};

const MAX_STEPS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentStatus {
    pub desired_revision: i64,
    pub observed_revision: i64,
    pub desired_state: String,
    pub observed_state: String,
    pub last_error: Option<String>,
    pub observed_pod_generation: i64,
}

pub fn persist_deployment_spec_for(
    fabric: &Fabric,
    deployment_id: &str,
    spec: &DeploymentSpec,
) -> Result<crate::specs::accept::DesiredSpecAcceptance, FabricError> {
    let typed = serde_json::to_string(spec)
        .map_err(|_| FabricError::Store("cannot encode deployment spec".into()))?;
    crate::specs::accept::require_spec_write(fabric.store.accept_resource_spec(
        "deployment",
        deployment_id,
        spec.revision,
        &spec.hash_bytes(),
        &typed,
    )?)
}

pub async fn reconcile_deployment(
    fabric: &Fabric,
    deployment_id: &str,
) -> Result<DeploymentStatus, FabricError> {
    let _lock = fabric
        .lifecycle_guard(&format!("deployment:{deployment_id}"))
        .await;
    reconcile_deployment_held(fabric, deployment_id).await
}

pub(crate) async fn reconcile_deployment_held(
    fabric: &Fabric,
    deployment_id: &str,
) -> Result<DeploymentStatus, FabricError> {
    let Some(row) = fabric
        .store
        .get_resource_spec("deployment", deployment_id)?
    else {
        return Err(FabricError::NotFound);
    };
    let spec: DeploymentSpec = serde_json::from_str(&row.typed_spec)
        .map_err(|_| FabricError::Store("deployment spec is unusable".into()))?;
    for _ in 0..MAX_STEPS {
        let observed = observe_deployment(fabric, deployment_id, &spec).await?;
        let local = DeploymentLocal {
            allocation: fabric
                .get_allocation(VolumeKind::Deployment, deployment_id)?
                .is_some_and(|row| row.state == "allocated"),
        };
        let action = plan_deployment(spec.planner_desired(), local, observed, spec.pod_generation);
        match action {
            DeploymentAction::Converged => {
                if spec.desired == crate::specs::deployment::DeploymentDesiredName::Absent {
                    fabric.purge_product_resource("deployment", deployment_id)?;
                    fabric
                        .store
                        .delete_resource_spec("deployment", deployment_id)?;
                    return Ok(DeploymentStatus {
                        desired_revision: spec.revision,
                        observed_revision: spec.revision,
                        desired_state: spec.desired.as_str().into(),
                        observed_state: "absent".into(),
                        last_error: None,
                        observed_pod_generation: spec.pod_generation,
                    });
                }
                let observed_state = spec.desired.as_str();
                fabric.store.set_resource_spec_observed(
                    "deployment",
                    deployment_id,
                    spec.revision,
                    observed_state,
                    None,
                )?;
                return Ok(DeploymentStatus {
                    desired_revision: spec.revision,
                    observed_revision: spec.revision,
                    desired_state: spec.desired.as_str().into(),
                    observed_state: observed_state.into(),
                    last_error: None,
                    observed_pod_generation: spec.pod_generation,
                });
            }
            DeploymentAction::WaitPod => {
                // Pod exists but is not Ready. Cached sqlite `running` from a
                // prior Converged pass hid Lost: GET kept reporting running
                // while the guest was only Pending/Running after a remint.
                fabric.store.set_resource_spec_observed(
                    "deployment",
                    deployment_id,
                    row.observed_revision,
                    "starting",
                    None,
                )?;
                return Ok(DeploymentStatus {
                    desired_revision: spec.revision,
                    observed_revision: row.observed_revision,
                    desired_state: spec.desired.as_str().into(),
                    observed_state: "starting".into(),
                    last_error: None,
                    observed_pod_generation: observed.pod_generation,
                });
            }
            DeploymentAction::NeedsReleaseStream => {
                fabric.store.set_resource_spec_observed(
                    "deployment",
                    deployment_id,
                    row.observed_revision,
                    "needs_release_stream",
                    Some("needs_release_stream"),
                )?;
                return Ok(DeploymentStatus {
                    desired_revision: spec.revision,
                    observed_revision: row.observed_revision,
                    desired_state: spec.desired.as_str().into(),
                    observed_state: "needs_release_stream".into(),
                    last_error: Some("needs_release_stream".into()),
                    observed_pod_generation: observed.pod_generation,
                });
            }
            other => {
                if let Err(error) = execute_deployment(fabric, deployment_id, &spec, other).await {
                    fabric.store.set_resource_spec_observed(
                        "deployment",
                        deployment_id,
                        row.observed_revision,
                        "failed",
                        Some(&error.to_string()),
                    )?;
                    return Err(error);
                }
            }
        }
    }
    Err(FabricError::Realize(
        "deployment reconcile did not settle".into(),
    ))
}

pub async fn reconcile_accepted_deployments(fabric: &Fabric) -> Result<(), FabricError> {
    for row in fabric.store.list_resource_specs("deployment")? {
        if let Err(error) = reconcile_deployment(fabric, &row.resource_id).await {
            eprintln!(
                "voie-fabricd: deployment {} reconcile: {error}",
                row.resource_id
            );
        }
    }
    Ok(())
}

async fn observe_deployment(
    fabric: &Fabric,
    deployment_id: &str,
    spec: &DeploymentSpec,
) -> Result<DeploymentObserved, FabricError> {
    let allocation = fabric.get_allocation(VolumeKind::Deployment, deployment_id)?;
    let lv_name = allocation
        .as_ref()
        .map(|row| row.lv_name.clone())
        .unwrap_or_else(|| crate::lv_name_for_deployment(deployment_id));
    let lv_path = format!("/dev/{}/{}", fabric.live().vg_name(), lv_name);
    // A leftover /dev node is not a volume. `lvs` is the Lost signal;
    // Path::exists is only the fallback when LVM itself is unreadable.
    let lv = match fabric.live().list_lv_names().await {
        Ok(names) => names.iter().any(|name| name == &lv_name),
        Err(_) => std::path::Path::new(&lv_path).exists(),
    };
    let pv_names = crate::product_realize::deployment_volume_aliases(
        allocation.as_ref().map(|row| row.lv_name.as_str()),
        deployment_id,
    );
    let mut pv = false;
    for name in &pv_names {
        if fabric.live().get_pv(name).await?.is_some()
            || fabric.live().get_namespaced("pvc", name).await?.is_some()
        {
            pv = true;
            break;
        }
    }
    let pod_generation = match fabric
        .live()
        .get_namespaced("pod", &app_pod_name(deployment_id))
        .await?
    {
        Some(value) => value
            .pointer("/metadata/labels/io.voie~1pod-generation")
            .and_then(|item| item.as_str())
            .and_then(|item| item.parse().ok())
            .unwrap_or(0),
        None => 0,
    };
    let pod = match fabric.live().get_pod(&app_pod_name(deployment_id)).await? {
        Some(info) => {
            classify_deployment_pod(&info.phase, info.ready, info.waiting_reason.as_deref())
        }
        None => DeploymentPod::Absent,
    };
    let (service_present, service_owned) = if spec.slug.is_empty() {
        (false, false)
    } else {
        match fabric
            .live()
            .get_namespaced("svc", &app_service_name(&spec.slug, &spec.kind))
            .await?
        {
            None => (false, false),
            Some(value) => {
                let owned = value
                    .pointer("/spec/selector")
                    .and_then(|selector| selector.get("io.voie/deployment"))
                    .and_then(|item| item.as_str())
                    == Some(deployment_id);
                (true, owned)
            }
        }
    };
    Ok(DeploymentObserved {
        lv,
        pv,
        pod,
        service_present,
        service_owned,
        pod_generation,
    })
}

async fn execute_deployment(
    fabric: &Fabric,
    deployment_id: &str,
    spec: &DeploymentSpec,
    action: DeploymentAction,
) -> Result<(), FabricError> {
    match action {
        DeploymentAction::AllocateLv => Err(FabricError::Realize(
            "deployment volume is streamed from the Release, not allocated empty".into(),
        )),
        DeploymentAction::CreatePv => {
            let Some(row) = fabric.get_allocation(VolumeKind::Deployment, deployment_id)? else {
                return Err(FabricError::Realize(
                    "deployment LV is not allocated".into(),
                ));
            };
            let device = format!("/dev/{}/{}", fabric.live().vg_name(), row.lv_name);
            crate::realize::require_stable_block_path(&device)?;
            let pv = crate::product_realize::deployment_pv_yaml(
                fabric.live(),
                deployment_id,
                &device,
                some_slug(&spec.slug),
            );
            let pvc = crate::product_realize::deployment_pvc_yaml(
                fabric.live(),
                deployment_id,
                some_slug(&spec.slug),
            );
            crate::product::refuse_user_infrastructure(&pv)?;
            crate::product::refuse_user_infrastructure(&pvc)?;
            crate::product::apply_or_unknown(fabric, &format!("{pv}\n---\n{pvc}")).await
        }
        DeploymentAction::CreatePod | DeploymentAction::ReplacePod => {
            fabric.ensure_runtime_class().await?;
            if spec.run_argv.is_empty() || spec.slug.is_empty() {
                return Err(FabricError::Realize(
                    "deployment pod create needs run argv and slug on the typed spec".into(),
                ));
            }
            if action == DeploymentAction::ReplacePod {
                crate::product::delete_named_retryable(
                    fabric,
                    "pod",
                    &app_pod_name(deployment_id),
                    true,
                    60,
                )
                .await?;
            }
            let env_secret = app_env_secret_name(deployment_id);
            let env_from = match fabric.live().get_namespaced("secret", &env_secret).await {
                Ok(Some(_)) => Some(env_secret.as_str()),
                _ => None,
            };
            let intent = app_intent_from_spec(deployment_id, spec);
            let pvc = fabric
                .get_allocation(VolumeKind::Deployment, deployment_id)?
                .map(|row| deployment_volume_for_lv(&row.lv_name, deployment_id))
                .unwrap_or_else(|| deployment_volume_name(deployment_id));
            let yaml =
                crate::product_realize::app_pod_yaml(fabric.live(), &intent, &pvc, env_from)?;
            crate::product::refuse_user_infrastructure(&yaml)?;
            if yaml.contains("postgres://") || yaml.contains("POSTGRES_PASSWORD") {
                return Err(FabricError::Realize(
                    "application pod must not embed credentials".into(),
                ));
            }
            if yaml.contains(&format!(
                "io.voie/kind: \"{}\"",
                crate::product_realize::KIND_WORKSPACE
            )) {
                return Err(FabricError::Realize(
                    "application pod must not use the Workspace identity".into(),
                ));
            }
            crate::product::ensure_egress_present(fabric).await?;
            crate::product::ensure_application_policy_present(fabric).await?;
            let postgres = application_postgres_policy_yaml(fabric.live(), &intent)?;
            crate::product::refuse_user_infrastructure(&postgres)?;
            if postgres.contains("ipBlock") || postgres.contains("fromEntities") {
                return Err(FabricError::Realize(
                    "application postgres policy must not carry CIDR or host entities".into(),
                ));
            }
            crate::product::apply_or_unknown(fabric, &format!("{yaml}\n---\n{postgres}")).await?;
            fabric.upsert_product_resource(
                "deployment",
                deployment_id,
                Some(&app_pod_name(deployment_id)),
                None,
                None,
                "starting",
            )?;
            Ok(())
        }
        DeploymentAction::RemovePod => {
            crate::product::delete_named_retryable(
                fabric,
                "pod",
                &app_pod_name(deployment_id),
                true,
                60,
            )
            .await
        }
        DeploymentAction::RemovePv => {
            crate::product::delete_deployment_volumes(fabric, deployment_id).await
        }
        DeploymentAction::RemoveLv => {
            crate::product::delete_named_retryable(
                fabric,
                "secret",
                &app_env_secret_name(deployment_id),
                true,
                30,
            )
            .await?;
            crate::product::delete_named_retryable(
                fabric,
                "networkpolicy",
                &crate::product_realize::application_postgres_policy_name(deployment_id),
                true,
                30,
            )
            .await?;
            fabric
                .free_volume(VolumeKind::Deployment, deployment_id)
                .await?;
            fabric.purge_product_resource("deployment", deployment_id)?;
            Ok(())
        }
        DeploymentAction::WaitPod
        | DeploymentAction::Converged
        | DeploymentAction::NeedsReleaseStream => Ok(()),
    }
}

fn some_slug(slug: &str) -> Option<&str> {
    if slug.is_empty() { None } else { Some(slug) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::specs::database::{DatabaseDesiredName, DatabaseSpec};
    use crate::specs::deployment::{DeploymentDesiredName, DeploymentSpec};
    use crate::{Config, Fabric, FabricError, Live, StoragePolicy};
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    fn write_executable(dir: &Path, name: &str, body: &str) -> PathBuf {
        let program = dir.join(name);
        std::fs::write(&program, body).expect("write fake program");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        program
    }

    fn absent_fabric(tag: &str) -> (Fabric, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "voie-fabricd-absent-journal-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let kubectl = write_executable(
            &dir,
            "kubectl",
            "#!/bin/sh\necho \"Error from server (NotFound)\" >&2\nexit 1\n",
        );
        let config = Config {
            bind: "127.0.0.1:0".into(),
            sqlite: dir.join("state.sqlite"),
            node_name: "node-under-test".into(),
            namespace: "voie-workspace".into(),
            storage_class: "voie-workspace-block".into(),
            runtime_class: "voie-firecracker".into(),
            runtime_handler: "kata-fc-rs-voie".into(),
            runner_image: "voie-runner:c1".into(),
            jailer_root: dir.join("jails"),
            vg: "voie-ws".into(),
            storage: StoragePolicy::test(),
            residue_wait_secs: 1,
            runtime_class_wait_secs: 0,
            kubectl_program: kubectl.to_string_lossy().into_owned(),
            kubectl_prefix: vec![],
            kubeconfig: None,
            crictl_program: "crictl".into(),
            crictl_prefix: vec![],
            tls_cert: PathBuf::from("/dev/null"),
            tls_key: PathBuf::from("/dev/null"),
            tls_ca: PathBuf::from("/dev/null"),
            approved_egress: None,
            client_sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .into(),
        };
        let live = Live::from_config(&config).expect("live");
        let fabric = Fabric::open(config, live).expect("fabric");
        (fabric, dir)
    }

    #[tokio::test]
    async fn absent_converge_removes_journal_rows() {
        let (fabric, dir) = absent_fabric("deploy-db");
        let deployment_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let database_id = "11111111-2222-3333-4444-555555555555";
        persist_deployment_spec_for(
            &fabric,
            deployment_id,
            &DeploymentSpec {
                revision: 3,
                desired: DeploymentDesiredName::Absent,
                release_id: uuid::Uuid::nil(),
                release_hash: String::new(),
                runtime_profile: "universal-v1".into(),
                slug: String::new(),
                kind: String::new(),
                port: 3000,
                run_argv: vec![],
                health_path: String::new(),
                cpu_millis: 0,
                memory_mb: 0,
                previous_deployment_id: None,
                pod_generation: 0,
            },
        )
        .unwrap();
        fabric
            .upsert_product_resource(
                "deployment",
                deployment_id,
                Some("pod"),
                None,
                None,
                "running",
            )
            .unwrap();
        let db = DatabaseSpec {
            revision: 4,
            desired: DatabaseDesiredName::Absent,
            runtime_profile: "voie-postgres:v1".into(),
            security_profile: 1,
            storage_tier: String::new(),
            volume_bytes: 1,
            credential_version: 0,
            slug: String::new(),
            kind: String::new(),
        };
        let typed = serde_json::to_string(&db).unwrap();
        fabric
            .store
            .upsert_resource_spec(
                "database",
                database_id,
                db.revision,
                &db.hash_bytes(),
                &typed,
            )
            .unwrap();
        fabric
            .upsert_product_resource("database", database_id, Some("pod"), None, None, "ready")
            .unwrap();

        let dep = reconcile_deployment(&fabric, deployment_id)
            .await
            .expect("delete mutate must succeed");
        assert_eq!(dep.observed_state, "absent");
        assert!(
            fabric
                .store
                .get_resource_spec("deployment", deployment_id)
                .unwrap()
                .is_none(),
            "absent Deployment spec is the journal row P1-C5 requires gone"
        );
        assert!(
            fabric
                .get_product_resource("deployment", deployment_id)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            reconcile_deployment(&fabric, deployment_id).await,
            Err(FabricError::NotFound)
        ));

        let db_status =
            crate::reconcile::database_run::reconcile_database(&fabric, database_id, None)
                .await
                .expect("database delete mutate must succeed");
        assert_eq!(db_status.observed_state, "absent");
        assert!(
            fabric
                .store
                .get_resource_spec("database", database_id)
                .unwrap()
                .is_none(),
            "absent Database spec is the journal row P1-C5 requires gone"
        );
        assert!(
            fabric
                .get_product_resource("database", database_id)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            crate::reconcile::database_run::reconcile_database(&fabric, database_id, None).await,
            Err(FabricError::NotFound)
        ));
        let _ = std::fs::remove_dir_all(dir);
    }
}
