//! Substrate observation adapters. They classify facts; they do not plan.

use crate::reconcile::database::{DatabasePod, DatabaseRoles};
use crate::reconcile::deployment::DeploymentPod;
use crate::reconcile::workspace::WorkspacePod;

/// Kubelet waiting reasons that will not become Ready without replacing the pod.
pub fn terminal_container_wait(reason: Option<&str>) -> bool {
    matches!(
        reason,
        Some("CreateContainerError")
            | Some("CreateContainerConfigError")
            | Some("CrashLoopBackOff")
            | Some("ImagePullBackOff")
            | Some("ErrImagePull")
            | Some("InvalidImageName")
    )
}

pub fn classify_pod(phase: &str, ready: bool, waiting_reason: Option<&str>) -> DatabasePod {
    if ready {
        if phase == "Failed" || phase == "Succeeded" || phase == "Unknown" || phase.is_empty() {
            return DatabasePod::Unknown { stale_ready: true };
        }
        return DatabasePod::Ready;
    }
    if terminal_container_wait(waiting_reason) {
        return DatabasePod::Failed;
    }
    match phase {
        "Pending" => DatabasePod::Pending,
        "Running" => DatabasePod::Running,
        "Failed" | "Succeeded" => DatabasePod::Failed,
        "Unknown" => DatabasePod::Unknown { stale_ready: false },
        _ => DatabasePod::Unknown { stale_ready: false },
    }
}

pub fn classify_workspace_pod(
    phase: &str,
    ready: bool,
    waiting_reason: Option<&str>,
) -> WorkspacePod {
    match classify_pod(phase, ready, waiting_reason) {
        DatabasePod::Absent => WorkspacePod::Absent,
        DatabasePod::Pending => WorkspacePod::Pending,
        DatabasePod::Running => WorkspacePod::Running,
        DatabasePod::Ready => WorkspacePod::Ready,
        DatabasePod::Unknown { stale_ready } => WorkspacePod::Unknown { stale_ready },
        DatabasePod::Failed => WorkspacePod::Failed,
    }
}

pub fn classify_deployment_pod(
    phase: &str,
    ready: bool,
    waiting_reason: Option<&str>,
) -> DeploymentPod {
    match classify_pod(phase, ready, waiting_reason) {
        DatabasePod::Absent => DeploymentPod::Absent,
        DatabasePod::Pending => DeploymentPod::Pending,
        DatabasePod::Running => DeploymentPod::Running,
        DatabasePod::Ready => DeploymentPod::Ready,
        DatabasePod::Unknown { stale_ready } => DeploymentPod::Unknown { stale_ready },
        DatabasePod::Failed => DeploymentPod::Failed,
    }
}

/// Authoritative role contract from live PostgreSQL, never a guest marker.
///
/// Profile 1: `app` is the initdb superuser (current guest).
/// Profile 2: `app` is NOSUPERUSER / NOCREATEDB / NOCREATEROLE /
/// NOREPLICATION / NOBYPASSRLS and `voie_platform` exists with NOLOGIN.
pub fn classify_roles(psql_stdout: &str) -> DatabaseRoles {
    let mut app_super = None;
    let mut app_login = None;
    let mut app_createdb = None;
    let mut app_createrole = None;
    let mut app_replication = None;
    let mut app_bypassrls = None;
    let mut platform_nologin = None;
    for line in psql_stdout.lines() {
        let cols: Vec<&str> = line.split('|').map(str::trim).collect();
        if cols.len() < 7 {
            continue;
        }
        let name = cols[0];
        let superuser = cols[1] == "t";
        let can_login = cols[2] == "t";
        let createdb = cols[3] == "t";
        let createrole = cols[4] == "t";
        let replication = cols[5] == "t";
        let bypassrls = cols[6] == "t";
        if name == "app" {
            app_super = Some(superuser);
            app_login = Some(can_login);
            app_createdb = Some(createdb);
            app_createrole = Some(createrole);
            app_replication = Some(replication);
            app_bypassrls = Some(bypassrls);
        }
        if name == "voie_platform" {
            platform_nologin = Some(!can_login);
        }
    }
    let Some(app_super) = app_super else {
        return DatabaseRoles::Unobserved;
    };
    let profile_2 = !app_super
        && app_login == Some(true)
        && app_createdb == Some(false)
        && app_createrole == Some(false)
        && app_replication == Some(false)
        && app_bypassrls == Some(false)
        && platform_nologin == Some(true);
    if profile_2 {
        DatabaseRoles::Matches {
            security_profile: 2,
        }
    } else if app_super {
        DatabaseRoles::Matches {
            security_profile: 1,
        }
    } else {
        DatabaseRoles::Mismatch {
            security_profile: 1,
        }
    }
}

pub const ROLE_QUERY: &str = "SELECT r.rolname, r.rolsuper, r.rolcanlogin, r.rolcreatedb, \
r.rolcreaterole, r.rolreplication, r.rolbypassrls \
FROM pg_roles r WHERE r.rolname IN ('app','voie_platform');";

pub const PROFILE_2_SQL: &str = "DO $$ BEGIN \
IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'voie_platform') THEN \
CREATE ROLE voie_platform SUPERUSER NOLOGIN; \
END IF; \
END $$; \
ALTER ROLE app WITH LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_running_is_ready() {
        assert_eq!(classify_pod("Running", true, None), DatabasePod::Ready);
    }

    #[test]
    fn stale_ready_on_unknown_phase() {
        assert_eq!(
            classify_pod("Unknown", true, None),
            DatabasePod::Unknown { stale_ready: true }
        );
    }

    #[test]
    fn missing_ready_running_is_running() {
        assert_eq!(classify_pod("Running", false, None), DatabasePod::Running);
    }

    #[test]
    fn profile_1_from_superuser_app() {
        let out = "app|t|t|t|t|t|t\n";
        assert_eq!(
            classify_roles(out),
            DatabaseRoles::Matches {
                security_profile: 1
            }
        );
    }

    #[test]
    fn profile_2_from_restricted_app() {
        let out = "app|f|t|f|f|f|f\nvoie_platform|t|f|t|t|t|t\n";
        assert_eq!(
            classify_roles(out),
            DatabaseRoles::Matches {
                security_profile: 2
            }
        );
    }

    #[test]
    fn workspace_and_deployment_pod_classes() {
        assert_eq!(
            classify_workspace_pod("Running", false, None),
            WorkspacePod::Running
        );
        assert_eq!(
            classify_deployment_pod("Unknown", true, None),
            DeploymentPod::Unknown { stale_ready: true }
        );
    }

    #[test]
    fn create_container_error_on_pending_is_failed() {
        assert_eq!(
            classify_pod("Pending", false, Some("CreateContainerError")),
            DatabasePod::Failed
        );
        assert_eq!(
            classify_workspace_pod("Pending", false, Some("CreateContainerError")),
            WorkspacePod::Failed
        );
        assert_eq!(
            classify_deployment_pod("Pending", false, Some("CreateContainerError")),
            DeploymentPod::Failed
        );
        assert_eq!(
            classify_pod("Pending", false, Some("ContainerCreating")),
            DatabasePod::Pending
        );
        assert_eq!(
            classify_pod("Running", false, Some("CrashLoopBackOff")),
            DatabasePod::Failed
        );
        assert!(!terminal_container_wait(Some("ContainerCreating")));
        assert!(terminal_container_wait(Some("ImagePullBackOff")));
    }
}
