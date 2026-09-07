//! Observe, plan, and execute one Database until WaitPod, Converged, or bound.

use std::path::Path;

use crate::observe::{self, classify_pod, classify_roles};
use crate::product_realize::{self, database_intent_from_spec, postgres_service_name};
use crate::realize::{legacy_lv_name_for_postgres, lv_name_for_postgres};
use crate::reconcile::database::{
    DatabaseAction, DatabaseLocal, DatabaseObserved, DatabasePod, DatabaseRoles, RestoreCandidate,
    plan_database,
};
use crate::specs::database::{DatabaseDesiredName, DatabaseSpec};
use crate::{Fabric, FabricError, VolumeKind};

const MAX_STEPS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseStatus {
    pub desired_revision: i64,
    pub observed_revision: i64,
    pub desired_state: String,
    pub observed_state: String,
    pub last_error: Option<String>,
}

pub async fn reconcile_database(
    fabric: &Fabric,
    database_id: &str,
    postgres_password: Option<&str>,
) -> Result<DatabaseStatus, FabricError> {
    let _lock = fabric
        .lifecycle_guard(&format!("database:{database_id}"))
        .await;
    let Some(row) = fabric.store.get_resource_spec("database", database_id)? else {
        return Err(FabricError::NotFound);
    };
    let spec: DatabaseSpec = serde_json::from_str(&row.typed_spec)
        .map_err(|_| FabricError::Store("database spec is unusable".into()))?;
    for _ in 0..MAX_STEPS {
        let observed = observe_database(fabric, database_id, &spec).await?;
        let local = DatabaseLocal {
            materialized: fabric
                .get_allocation(VolumeKind::Database, database_id)?
                .is_some_and(|row| row.state == "allocated"),
        };
        let action = plan_database(spec.planner_desired(), local, observed);
        match action {
            DatabaseAction::Converged => {
                if spec.desired == DatabaseDesiredName::Absent {
                    fabric.purge_product_resource("database", database_id)?;
                    fabric.store.delete_resource_spec("database", database_id)?;
                    return Ok(DatabaseStatus {
                        desired_revision: spec.revision,
                        observed_revision: spec.revision,
                        desired_state: spec.desired.as_str().into(),
                        observed_state: "absent".into(),
                        last_error: None,
                    });
                }
                let observed_state = match spec.desired {
                    DatabaseDesiredName::Present => "ready",
                    DatabaseDesiredName::Suspended => "suspended",
                    DatabaseDesiredName::Absent => "absent",
                };
                fabric.store.set_resource_spec_observed(
                    "database",
                    database_id,
                    spec.revision,
                    observed_state,
                    None,
                )?;
                fabric.upsert_product_resource(
                    "database",
                    database_id,
                    Some(&crate::product::live_postgres_pod(fabric, database_id)),
                    Some(&postgres_service_name(database_id)),
                    Some(database_id),
                    observed_state,
                )?;
                return Ok(DatabaseStatus {
                    desired_revision: spec.revision,
                    observed_revision: spec.revision,
                    desired_state: spec.desired.as_str().into(),
                    observed_state: observed_state.into(),
                    last_error: None,
                });
            }
            DatabaseAction::WaitPod => {
                return Ok(DatabaseStatus {
                    desired_revision: spec.revision,
                    observed_revision: row.observed_revision,
                    desired_state: spec.desired.as_str().into(),
                    observed_state: row.state.clone(),
                    last_error: None,
                });
            }
            DatabaseAction::Lost => {
                fabric.store.set_resource_spec_observed(
                    "database",
                    database_id,
                    row.observed_revision,
                    "lost",
                    Some("durable_volume_missing"),
                )?;
                return Ok(DatabaseStatus {
                    desired_revision: spec.revision,
                    observed_revision: row.observed_revision,
                    desired_state: spec.desired.as_str().into(),
                    observed_state: "lost".into(),
                    last_error: Some("durable_volume_missing".into()),
                });
            }
            DatabaseAction::ObserveRoles => {
                if observe_roles(fabric, database_id).await
                    == crate::reconcile::database::DatabaseRoles::Unobserved
                {
                    return Ok(DatabaseStatus {
                        desired_revision: spec.revision,
                        observed_revision: row.observed_revision,
                        desired_state: spec.desired.as_str().into(),
                        observed_state: row.state.clone(),
                        last_error: None,
                    });
                }
            }
            other => {
                if let Err(error) =
                    execute_database(fabric, database_id, &spec, postgres_password, other).await
                {
                    fabric.store.set_resource_spec_observed(
                        "database",
                        database_id,
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
        "database reconcile did not settle".into(),
    ))
}

pub async fn reconcile_accepted_databases(fabric: &Fabric) -> Result<(), FabricError> {
    for row in fabric.store.list_resource_specs("database")? {
        if let Err(error) = reconcile_database(fabric, &row.resource_id, None).await {
            eprintln!(
                "voie-fabricd: database {} reconcile: {error}",
                row.resource_id
            );
        }
    }
    Ok(())
}

async fn observe_database(
    fabric: &Fabric,
    database_id: &str,
    spec: &DatabaseSpec,
) -> Result<DatabaseObserved, FabricError> {
    let allocation = fabric.get_allocation(VolumeKind::Database, database_id)?;
    let lv_name = allocation
        .as_ref()
        .map(|row| row.lv_name.clone())
        .unwrap_or_else(|| lv_name_for_postgres(database_id));
    let vg = fabric.live().vg_name();
    let lv = Path::new(&format!("/dev/{vg}/{lv_name}")).exists()
        || Path::new(&format!(
            "/dev/{vg}/{}",
            legacy_lv_name_for_postgres(database_id)
        ))
        .exists();
    // Linear Database LVs are not wrapped today. Mapper is satisfied when
    // the LV exists so EnsureMapper is not a reboot dead-end.
    let mapper = lv;
    let pv_name = crate::product::live_postgres_volume(fabric, database_id);
    let pv = fabric.live().get_pv(&pv_name).await?.is_some();
    let pod = match fabric
        .live()
        .get_pod(&crate::product::live_postgres_pod(fabric, database_id))
        .await?
    {
        Some(info) => classify_pod(&info.phase, info.ready, info.waiting_reason.as_deref()),
        None => DatabasePod::Absent,
    };
    let roles = if pod == DatabasePod::Ready {
        observe_roles(fabric, database_id).await
    } else {
        DatabaseRoles::Unobserved
    };
    let candidate = fabric
        .get_allocation(VolumeKind::DatabaseRestore, database_id)?
        .map(|row| RestoreCandidate {
            present: true,
            verified: false,
            ambiguous: row.state != "allocated",
        })
        .unwrap_or_default();
    let _ = spec;
    Ok(DatabaseObserved {
        lv,
        mapper,
        pv,
        pod,
        roles,
        candidate,
        unformatted: if lv {
            fabric
                .live()
                .device_is_unformatted(&format!("/dev/{vg}/{lv_name}"))
                .await
                .unwrap_or(false)
        } else {
            false
        },
    })
}

async fn observe_roles(fabric: &Fabric, database_id: &str) -> DatabaseRoles {
    let pod = crate::product::live_postgres_pod(fabric, database_id);
    let cmd = crate::product::postgres_client_command(&format!(
        "psql -U app -d postgres -At -F '|' -c \"{}\"",
        observe::ROLE_QUERY.replace('"', "\\\"")
    ));
    let argv: Vec<&str> = cmd.iter().map(String::as_str).collect();
    match fabric
        .live()
        .exec_guest(&pod, "postgres", &argv, 15_000)
        .await
    {
        Ok(output) if !output.ambiguous && output.exit_code == 0 => classify_roles(&output.stdout),
        _ => DatabaseRoles::Unobserved,
    }
}

async fn execute_database(
    fabric: &Fabric,
    database_id: &str,
    spec: &DatabaseSpec,
    postgres_password: Option<&str>,
    action: DatabaseAction,
) -> Result<(), FabricError> {
    let intent = database_intent_from_spec(database_id, spec);
    match action {
        DatabaseAction::AllocateLv => {
            fabric.ensure_runtime_class().await?;
            let bytes = spec.volume_bytes_for(fabric.live().storage());
            let slot = fabric
                .allocate_volume(VolumeKind::Database, database_id, bytes, None)
                .await?;
            fabric.live().mkfs_ext4_if_needed(&slot.device).await?;
            Ok(())
        }
        DatabaseAction::EnsureMapper => Ok(()),
        DatabaseAction::EnsureFilesystem => {
            if fabric
                .live()
                .get_pod(&crate::product::live_postgres_pod(fabric, database_id))
                .await?
                .is_some()
            {
                crate::product::delete_named_retryable(
                    fabric,
                    "pod",
                    &crate::product::live_postgres_pod(fabric, database_id),
                    true,
                    60,
                )
                .await?;
            }
            let Some(row) = fabric.get_allocation(VolumeKind::Database, database_id)? else {
                return Err(FabricError::Realize("database LV is not allocated".into()));
            };
            let device = format!("/dev/{}/{}", fabric.live().vg_name(), row.lv_name);
            crate::realize::require_stable_block_path(&device)?;
            fabric.live().mkfs_ext4_if_needed(&device).await
        }
        DatabaseAction::CreatePv => {
            let Some(row) = fabric.get_allocation(VolumeKind::Database, database_id)? else {
                return Err(FabricError::Realize("database LV is not allocated".into()));
            };
            let device = format!("/dev/{}/{}", fabric.live().vg_name(), row.lv_name);
            crate::realize::require_stable_block_path(&device)?;
            fabric.live().mkfs_ext4_if_needed(&device).await?;
            let bytes = spec.volume_bytes_for(fabric.live().storage());
            let pv = product_realize::postgres_pv_yaml(
                fabric.live(),
                database_id,
                &device,
                some_slug(&spec.slug),
                bytes,
            );
            let pvc = product_realize::postgres_pvc_yaml(
                fabric.live(),
                database_id,
                some_slug(&spec.slug),
                bytes,
            );
            crate::product::refuse_user_infrastructure(&pv)?;
            crate::product::refuse_user_infrastructure(&pvc)?;
            crate::product::apply_or_unknown(fabric, &format!("{pv}\n---\n{pvc}")).await
        }
        DatabaseAction::CreatePod | DatabaseAction::ReplacePod => {
            fabric.ensure_runtime_class().await?;
            if action == DatabaseAction::ReplacePod {
                crate::product::delete_named_retryable(
                    fabric,
                    "pod",
                    &crate::product::live_postgres_pod(fabric, database_id),
                    true,
                    60,
                )
                .await?;
            }
            if let Some(password) = postgres_password {
                let mut pg_labels: Vec<(&str, &str)> = vec![
                    ("io.voie/kind", "postgres"),
                    ("io.voie/database", database_id),
                ];
                if !spec.slug.is_empty() {
                    pg_labels.push(("io.voie/slug", spec.slug.as_str()));
                }
                fabric
                    .live()
                    .apply_opaque_secret(
                        &product_realize::postgres_secret_name(database_id),
                        "postgres-password",
                        password.as_bytes(),
                        &pg_labels,
                    )
                    .await
                    .map_err(|error| FabricError::Unknown(error.to_string()))?;
            }
            let Some(row) = fabric.get_allocation(VolumeKind::Database, database_id)? else {
                return Err(FabricError::Realize("database LV is not allocated".into()));
            };
            let yaml = product_realize::postgres_runtime_pod_yaml(
                fabric.live(),
                database_id,
                &row.lv_name,
                row.operation_id.as_deref(),
                &intent.slug,
                &intent.kind,
                intent.security_profile,
                intent.revision,
            );
            let generation = product_realize::postgres_generation_for_lv(
                database_id,
                &row.lv_name,
                row.operation_id.as_deref(),
            );
            let service =
                product_realize::postgres_service_yaml(fabric.live(), &intent, &generation);
            let policy = product_realize::postgres_network_policy_yaml(fabric.live(), &intent)?;
            crate::product::refuse_user_infrastructure(&yaml)?;
            crate::product::refuse_user_infrastructure(&service)?;
            crate::product::refuse_user_infrastructure(&policy)?;
            if yaml.contains("POSTGRES_PASSWORD") || service.contains("POSTGRES_PASSWORD") {
                return Err(FabricError::Realize(
                    "postgres manifest must not embed credentials".into(),
                ));
            }
            if policy.contains("ipBlock") || policy.contains("fromEntities") {
                return Err(FabricError::Realize(
                    "postgres network policy must not carry CIDR or host entities".into(),
                ));
            }
            crate::product::apply_or_unknown(
                fabric,
                &format!("{yaml}\n---\n{service}\n---\n{policy}"),
            )
            .await?;
            fabric.upsert_product_resource(
                "database",
                database_id,
                Some(&product_realize::postgres_pod_for_lv(
                    &row.lv_name,
                    database_id,
                )),
                Some(&postgres_service_name(database_id)),
                Some(database_id),
                "creating",
            )?;
            Ok(())
        }
        DatabaseAction::ObserveRoles => {
            if observe_roles(fabric, database_id).await == DatabaseRoles::Unobserved {
                return Ok(());
            }
            Ok(())
        }
        DatabaseAction::RestartWithCurrentPostgresProfile => {
            let pod = crate::product::live_postgres_pod(fabric, database_id);
            let cmd = crate::product::postgres_client_command(&format!(
                "psql -U app -d postgres -v ON_ERROR_STOP=1 -c \"{}\"",
                observe::PROFILE_2_SQL.replace('"', "\\\"")
            ));
            let argv: Vec<&str> = cmd.iter().map(String::as_str).collect();
            let output = fabric
                .live()
                .exec_guest(&pod, "postgres", &argv, 30_000)
                .await?;
            if output.ambiguous {
                return Err(FabricError::Unknown(
                    "database security profile apply did not settle".into(),
                ));
            }
            if output.exit_code != 0 {
                return Err(FabricError::Realize(format!(
                    "database security profile apply exited {}",
                    output.exit_code
                )));
            }
            Ok(())
        }
        DatabaseAction::RemovePod => {
            crate::product::delete_named_retryable(
                fabric,
                "pod",
                &crate::product::live_postgres_pod(fabric, database_id),
                true,
                60,
            )
            .await?;
            crate::product::delete_named_retryable(
                fabric,
                "svc",
                &postgres_service_name(database_id),
                true,
                30,
            )
            .await?;
            crate::product::delete_named_retryable(
                fabric,
                "secret",
                &product_realize::postgres_secret_name(database_id),
                true,
                30,
            )
            .await?;
            crate::product::delete_named_retryable(
                fabric,
                "networkpolicy",
                &product_realize::postgres_network_policy_name(database_id),
                true,
                30,
            )
            .await
        }
        DatabaseAction::RemovePv => {
            crate::product::delete_local_volume(
                fabric,
                &crate::product::live_postgres_volume(fabric, database_id),
            )
            .await
        }
        DatabaseAction::RemoveMapper => Ok(()),
        DatabaseAction::RemoveLv => {
            let _ = std::fs::remove_dir_all(fabric.postgres_root().join(database_id));
            fabric
                .free_volume(VolumeKind::Database, database_id)
                .await?;
            fabric.purge_product_resource("database", database_id)?;
            Ok(())
        }
        DatabaseAction::DiscardCandidate => {
            if let Some(row) = fabric.get_allocation(VolumeKind::DatabaseRestore, database_id)? {
                let pod = product_realize::postgres_pod_for_lv(&row.lv_name, database_id);
                let pvc = product_realize::postgres_pvc_for_lv(&row.lv_name, database_id);
                let _ = crate::product::delete_named_retryable(fabric, "pod", &pod, true, 60).await;
                let _ = crate::product::delete_local_volume(fabric, &pvc).await;
                let _ = fabric
                    .free_volume(VolumeKind::DatabaseRestore, database_id)
                    .await;
            }
            Ok(())
        }
        DatabaseAction::MaterializeCandidate | DatabaseAction::PromoteCandidate => {
            Err(FabricError::Realize(
                "database restore candidate is still an at-most-once operation".into(),
            ))
        }
        DatabaseAction::WaitPod | DatabaseAction::Converged => Ok(()),
        DatabaseAction::Lost => Err(FabricError::Realize(
            "database volume is lost; recovery is an explicit restore".into(),
        )),
    }
}

fn some_slug(slug: &str) -> Option<&str> {
    if slug.is_empty() { None } else { Some(slug) }
}

pub fn persist_database_spec_for(
    fabric: &Fabric,
    database_id: &str,
    spec: &DatabaseSpec,
) -> Result<(), FabricError> {
    let bytes = spec.volume_bytes_for(fabric.live().storage());
    if spec.desired == crate::specs::database::DatabaseDesiredName::Present {
        let prod = spec.kind == "prod";
        if bytes == 0
            || !fabric
                .live()
                .storage()
                .matches_tier(VolumeKind::Database, bytes, prod)
        {
            return Err(FabricError::Conflict(
                "database size is not a platform storage tier".into(),
            ));
        }
    }
    let typed = serde_json::to_string(spec)
        .map_err(|_| FabricError::Store("cannot encode database spec".into()))?;
    crate::specs::accept::require_spec_write(fabric.store.accept_resource_spec(
        "database",
        database_id,
        spec.revision,
        &spec.hash_bytes(),
        &typed,
    )?)
    .map(|_| ())
}
