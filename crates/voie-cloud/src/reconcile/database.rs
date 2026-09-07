//! Database desired-state wake. Fabric owns realization.

use std::time::Duration;

use serde_json::json;
use uuid::Uuid;

use crate::databases::Database;
use crate::fabric_client::FabricError;
use crate::http::Platform;
use crate::reconcile::{
    OBSERVE_AFTER_SECS, OBSERVE_RETRY_SECS, fabric_reported_revision, fabric_revision_caught_up,
    observed_satisfies_desired, should_heal_missing_database_spec,
};
use crate::secrets::SecretValue;

pub async fn reconcile_due(platform: &Platform) {
    let _ = platform
        .databases
        .persist_absent_desired_for_removing_applications()
        .await;
    let Ok(due) = platform.databases.list_due().await else {
        return;
    };
    for database in due {
        reconcile_one(platform, database).await;
    }
}

/// Mutation-path wake after PostgreSQL records desired state.
pub async fn put_due_database(platform: &Platform, id: Uuid) {
    let Ok(database) = platform.databases.get_internal(id).await else {
        return;
    };
    reconcile_one(platform, database).await;
}

pub fn spawn_loop(platform: Platform) {
    tokio::spawn(async move {
        loop {
            reconcile_due(&platform).await;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

async fn reconcile_one(platform: &Platform, database: Database) {
    if application_is_removing(platform, database.application_id).await
        && database.desired_state != "absent"
    {
        return;
    }
    let database = if database.security_profile == 1 && database.desired_state != "absent" {
        match platform
            .databases
            .advance_release0_security_profile(database.id)
            .await
        {
            Ok(updated) => updated,
            Err(_) => database,
        }
    } else {
        database
    };
    if database.desired_revision > database.observed_revision {
        if database.observed_state == "lost" {
            return;
        }
        put_database_spec(platform, &database).await;
        return;
    }
    observe_database_status(platform, &database).await;
}

async fn put_database_spec(platform: &Platform, database: &Database) {
    let Some(runtime) = platform.runtime.as_ref() else {
        return;
    };
    let Ok(Some(environment)) = crate::applications::load_environment(
        platform.applications.pool(),
        database.environment_id,
    )
    .await
    else {
        return;
    };
    let Ok(application) = crate::applications::ApplicationStore::new(
        platform.applications.pool().clone(),
        String::new(),
    )
    .get_internal(environment.application_id)
    .await
    else {
        return;
    };
    let mut body = json!({
        "revision": database.desired_revision,
        "desired": database.desired_state,
        "runtimeProfile": database.engine_profile,
        "securityProfile": database.security_profile,
        "storageTier": database.storage_tier,
        "slug": application.slug,
        "kind": environment.kind,
    });
    let mut secret_id = database.credential_secret_id;
    if database.desired_state == "present" {
        match load_or_mint_password(&platform.databases, runtime.secrets.as_ref(), database).await {
            Password::Ready { id, password } => {
                secret_id = Some(id);
                body["postgresPassword"] = json!(password);
            }
            Password::Retry => return,
            Password::FailClosed => return,
        }
    }
    match runtime.fabric.put_database_spec(database.id, &body).await {
        Ok(outcome) if outcome.state == "lost" => {
            let error = outcome
                .last_error_code
                .as_deref()
                .unwrap_or("durable_volume_missing");
            let _ = platform
                .databases
                .record_lost(database.id, error, outcome.observed_revision)
                .await;
        }
        Ok(outcome) if outcome.state == "ready" => {
            if let Some(secret_id) = secret_id {
                let _ = platform.databases.mark_ready(database.id, secret_id).await;
            }
            apply_database_put_outcome(platform, database, &outcome).await;
        }
        Ok(outcome) => apply_database_put_outcome(platform, database, &outcome).await,
        Err(_) => {
            let _ = platform
                .databases
                .record_reconcile_error(database.id, "fabric_put_failed", 5)
                .await;
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Password {
    Ready { id: Uuid, password: String },
    Retry,
    FailClosed,
}

async fn load_existing_password(
    databases: &crate::databases::DatabaseStore,
    secrets: &crate::secrets::MaterialBackend,
    database_id: Uuid,
    secret_id: Uuid,
) -> Password {
    match secrets.get_platform_material(secret_id).await {
        Ok(material) => match std::str::from_utf8(material.as_bytes()) {
            Ok(text) if !text.is_empty() => Password::Ready {
                id: secret_id,
                password: text.to_owned(),
            },
            _ => {
                let _ = databases.fail_closed_creating(database_id).await;
                Password::FailClosed
            }
        },
        Err(_) => Password::Retry,
    }
}

async fn load_or_mint_password(
    databases: &crate::databases::DatabaseStore,
    secrets: &crate::secrets::MaterialBackend,
    database: &Database,
) -> Password {
    if let Some(existing) = database.credential_secret_id {
        return load_existing_password(databases, secrets, database.id, existing).await;
    }
    let Ok(password) = crate::databases::generate_postgres_password() else {
        return Password::Retry;
    };
    let candidate_id = Uuid::new_v4();
    let Ok(value) = SecretValue::from_text(password.clone()) else {
        return Password::Retry;
    };
    if secrets
        .put_platform_material(candidate_id, value)
        .await
        .is_err()
    {
        eprintln!(
            "voie-cloud: database {} credential store failed",
            database.id
        );
        return Password::Retry;
    }
    let Ok(winner_id) = databases.attach_credential(database.id, candidate_id).await else {
        return Password::Retry;
    };
    if winner_id == candidate_id {
        return Password::Ready {
            id: candidate_id,
            password,
        };
    }
    let _ = secrets.delete_platform_material(candidate_id).await;
    load_existing_password(databases, secrets, database.id, winner_id).await
}

async fn apply_database_put_outcome(
    platform: &Platform,
    database: &Database,
    outcome: &crate::fabric_client::ProductOutcome,
) {
    if !fabric_revision_caught_up(outcome.observed_revision, database.desired_revision) {
        persist_database_observation(platform, database.id, outcome).await;
        return;
    }
    let observed = outcome.state.as_str();
    if !observed_satisfies_desired(&database.desired_state, observed) {
        persist_database_observation(platform, database.id, outcome).await;
        return;
    }
    persist_database_observation(platform, database.id, outcome).await;
}

async fn persist_database_observation(
    platform: &Platform,
    database_id: Uuid,
    outcome: &crate::fabric_client::ProductOutcome,
) {
    let fabric_rev = fabric_reported_revision(outcome.observed_revision);
    let desired: String =
        sqlx::query_scalar("select desired_state from application_databases where id = $1")
            .bind(database_id)
            .fetch_one(platform.applications.pool())
            .await
            .unwrap_or_default();
    let desired_revision: i64 =
        sqlx::query_scalar("select desired_revision from application_databases where id = $1")
            .bind(database_id)
            .fetch_one(platform.applications.pool())
            .await
            .unwrap_or(0);
    let caught_up = fabric_revision_caught_up(outcome.observed_revision, desired_revision)
        && observed_satisfies_desired(&desired, &outcome.state);
    let done_absent = caught_up && desired == "absent";
    let error = if caught_up {
        None
    } else if !fabric_revision_caught_up(outcome.observed_revision, desired_revision)
        && observed_satisfies_desired(&desired, &outcome.state)
    {
        Some("fabric_revision_unproven")
    } else {
        Some(
            outcome
                .last_error_code
                .as_deref()
                .unwrap_or("observed_not_desired"),
        )
    };
    let after = if done_absent {
        None
    } else if caught_up {
        Some(OBSERVE_AFTER_SECS)
    } else {
        Some(OBSERVE_RETRY_SECS)
    };
    let _ = sqlx::query(
        "update application_databases \
         set observed_state = $2, last_error_code = $3, \
             observed_revision = coalesce($4, observed_revision), \
             reconcile_after = case \
                 when $5 then null \
                 else now() + ($6 * interval '1 second') end \
         where id = $1",
    )
    .bind(database_id)
    .bind(&outcome.state)
    .bind(error)
    .bind(fabric_rev)
    .bind(done_absent)
    .bind(after.unwrap_or(OBSERVE_AFTER_SECS))
    .execute(platform.applications.pool())
    .await;
}

async fn observe_database_status(platform: &Platform, database: &Database) {
    let Some(runtime) = platform.runtime.as_ref() else {
        return;
    };
    match runtime
        .fabric
        .product_get(&format!("/v1/databases/{}", database.id))
        .await
    {
        Ok(outcome) if outcome.state == "lost" => {
            let error = outcome
                .last_error_code
                .as_deref()
                .unwrap_or("durable_volume_missing");
            let _ = platform
                .databases
                .record_lost(database.id, error, outcome.observed_revision)
                .await;
        }
        Ok(outcome)
            if observed_satisfies_desired(&database.desired_state, &outcome.state)
                && fabric_revision_caught_up(
                    outcome.observed_revision,
                    database.desired_revision,
                ) =>
        {
            persist_database_observation(platform, database.id, &outcome).await;
            if outcome.state == "ready" {
                if let Some(secret_id) = database.credential_secret_id {
                    let _ = platform.databases.mark_ready(database.id, secret_id).await;
                }
            }
        }
        Ok(outcome) if observed_satisfies_desired(&database.desired_state, &outcome.state) => {
            persist_database_observation(platform, database.id, &outcome).await;
            put_database_spec(platform, database).await;
        }
        Ok(outcome) if outcome.state == "needs_release_stream" => {
            let _ = platform
                .databases
                .record_reconcile_error(
                    database.id,
                    outcome
                        .last_error_code
                        .as_deref()
                        .unwrap_or("needs_release_stream"),
                    OBSERVE_RETRY_SECS,
                )
                .await;
        }
        Ok(outcome) => {
            persist_database_observation(platform, database.id, &outcome).await;
        }
        Err(FabricError::Transport) => {
            let _ = sqlx::query(
                "update application_databases set last_error_code = 'fabric_unreachable', \
                 reconcile_after = now() + ($2 * interval '1 second') \
                 where id = $1 and observed_state <> 'lost'",
            )
            .bind(database.id)
            .bind(OBSERVE_RETRY_SECS)
            .execute(platform.applications.pool())
            .await;
            let _ = sqlx::query(
                "update application_databases \
                 set reconcile_after = now() + ($2 * interval '1 second') \
                 where id = $1 and observed_state = 'lost'",
            )
            .bind(database.id)
            .bind(OBSERVE_RETRY_SECS)
            .execute(platform.applications.pool())
            .await;
        }
        Err(_) => {
            let application_state = application_state(platform, database.application_id).await;
            if should_heal_missing_database_spec(
                &database.desired_state,
                &database.observed_state,
                &application_state,
            ) || database.desired_state == "absent"
            {
                put_database_spec(platform, database).await;
            } else if database.observed_state == "lost" {
                let _ = platform
                    .databases
                    .record_reconcile_error(
                        database.id,
                        "fabric_observe_failed",
                        OBSERVE_RETRY_SECS,
                    )
                    .await;
            }
        }
    }
}

async fn application_state(platform: &Platform, application_id: Uuid) -> String {
    sqlx::query_scalar("select state from applications where id = $1")
        .bind(application_id)
        .fetch_optional(platform.applications.pool())
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

async fn application_is_removing(platform: &Platform, application_id: Uuid) -> bool {
    matches!(
        application_state(platform, application_id).await.as_str(),
        "deleting" | "deleted" | "archiving" | "archived"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::applications::ApplicationStore;
    use crate::secrets::{InMemorySecretBackend, MaterialBackend};
    use crate::{Config, Kernel};
    use tokio::sync::Mutex;

    static MIGRATE_LOCK: Mutex<()> = Mutex::const_new(());

    async fn kernel() -> Kernel {
        let _lock = MIGRATE_LOCK.lock().await;
        let kernel = Kernel::connect(&Config::database_url(
            std::env::var("VOIE_TEST_DATABASE_URL").expect("VOIE_TEST_DATABASE_URL"),
        ))
        .await
        .expect("postgres");
        kernel.migrate().await.expect("desired-state migrations");
        kernel
    }

    async fn present_database(
        kernel: &Kernel,
        label: &str,
    ) -> (crate::databases::DatabaseStore, Database) {
        let owner = Uuid::new_v4();
        sqlx::query("insert into users (id, issuer, subject) values ($1, $2, $3)")
            .bind(owner)
            .bind(format!("{label}-{}", Uuid::new_v4()))
            .bind(label)
            .execute(kernel.pool())
            .await
            .expect("user");
        let project = kernel
            .create_project(Uuid::new_v4(), owner, &format!("{label}-proj"), "team")
            .await
            .expect("project");
        let fabric = Uuid::new_v4();
        sqlx::query("insert into fabrics (id, name) values ($1, $2)")
            .bind(fabric)
            .bind(format!("{label}-fabric-{fabric}"))
            .execute(kernel.pool())
            .await
            .expect("fabric");
        let workspace = Uuid::new_v4();
        sqlx::query(
            "insert into workspaces (id, project_id, fabric_id, state, created_by_user_id, exec_generation, observed_state) \
             values ($1, $2, $3, 'creating', $4, 1, 'ready')",
        )
        .bind(workspace)
        .bind(project.id)
        .bind(fabric)
        .bind(owner)
        .execute(kernel.pool())
        .await
        .expect("workspace");
        let created = ApplicationStore::new(kernel.pool().clone(), "console.test".into())
            .create(owner, project.id, workspace, "Credential Claim App", None)
            .await
            .expect("application");
        let environment_id: Uuid = sqlx::query_scalar(
            "select id from application_environments where application_id = $1 and kind = 'dev'",
        )
        .bind(created.application.id)
        .fetch_one(kernel.pool())
        .await
        .expect("dev environment");
        let databases = crate::databases::DatabaseStore::new(kernel.pool().clone());
        let database = databases
            .create(owner, environment_id, fabric, Uuid::new_v4(), &[7u8; 32])
            .await
            .expect("database row");
        (databases, database)
    }

    #[tokio::test]
    async fn concurrent_initial_password_acquisitions_share_one_credential() {
        let kernel = kernel().await;
        let (databases, database) = present_database(&kernel, "db-cred").await;
        assert!(database.credential_secret_id.is_none());
        let secrets = MaterialBackend::Memory(InMemorySecretBackend::new());
        let (left, right) = tokio::join!(
            load_or_mint_password(&databases, &secrets, &database),
            load_or_mint_password(&databases, &secrets, &database),
        );
        let Password::Ready {
            id: left_id,
            password: left_password,
        } = left
        else {
            panic!("left mint {left:?}");
        };
        let Password::Ready {
            id: right_id,
            password: right_password,
        } = right
        else {
            panic!("right mint {right:?}");
        };
        assert_eq!(left_id, right_id);
        assert_eq!(left_password, right_password);
        assert!(!left_password.is_empty());
        let stored = databases
            .get_internal(database.id)
            .await
            .expect("stored database");
        assert_eq!(stored.credential_secret_id, Some(left_id));
        let MaterialBackend::Memory(memory) = &secrets else {
            unreachable!("memory backend");
        };
        assert_eq!(memory.stored_count(), 1);
    }
}
