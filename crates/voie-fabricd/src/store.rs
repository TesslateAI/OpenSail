//! Local realization facts. Six tables, no general persistence layer.

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::FabricError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRow {
    pub id: String,
    pub state: String,
    pub device: String,
    pub node: String,
    pub pv_name: String,
    pub pvc_name: String,
    pub lv_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReservationRow {
    pub workspace_id: String,
    pub device: String,
    pub node: String,
    pub pv_name: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationRow {
    pub workspace_id: String,
    pub generation: i64,
    pub pod_name: String,
    pub pod_uid: Option<String>,
    pub sandbox_id: Option<String>,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecRow {
    pub workspace_id: String,
    pub call_id: String,
    pub request_hash: Vec<u8>,
    pub state: String,
    pub exit_code: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupRow {
    pub workspace_id: String,
    pub pod_absent: bool,
    pub reservation_released: bool,
    pub jail_absent: bool,
    pub vmm_absent: bool,
    pub children_absent: bool,
}

/// Durable desired/observed state of the one guest-egress NetworkPolicy the
/// daemon owns. This is a single concrete object, not a policy framework:
/// the desired YAML and spec digest are stored before realization, and the
/// observed state is recorded after every positive or failed confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyRow {
    pub name: String,
    pub namespace: String,
    pub desired_yaml: String,
    pub desired_spec_sha: String,
    pub observed_state: String,
    pub observed_spec_sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeginDispatch {
    ReadyToDispatch,
    Terminal {
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    OutcomeUnknown,
    Conflict,
}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, FabricError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    FabricError::Store(format!("cannot create sqlite directory: {error}"))
                })?;
            }
        }
        let conn = Connection::open(path)
            .map_err(|error| FabricError::Store(format!("open sqlite: {error}")))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|error| FabricError::Store(error.to_string()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             PRAGMA synchronous=NORMAL;",
        )
        .map_err(|error| FabricError::Store(error.to_string()))?;
        let store = Store {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<(), FabricError> {
        let conn = self.lock()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS workspaces (
                 id TEXT PRIMARY KEY,
                 state TEXT NOT NULL,
                 device TEXT NOT NULL,
                 node TEXT NOT NULL,
                 pv_name TEXT NOT NULL,
                 pvc_name TEXT NOT NULL,
                 lv_name TEXT,
                 created_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS volume_reservations (
                 workspace_id TEXT PRIMARY KEY,
                 device TEXT NOT NULL,
                 node TEXT NOT NULL,
                 pv_name TEXT NOT NULL,
                 state TEXT NOT NULL,
                 reason TEXT,
                 reserved_at INTEGER NOT NULL,
                 released_at INTEGER
             );
             CREATE UNIQUE INDEX IF NOT EXISTS volume_reservations_active_device
                 ON volume_reservations(device) WHERE state = 'reserved';
             CREATE TABLE IF NOT EXISTS execution_generations (
                 workspace_id TEXT NOT NULL,
                 generation INTEGER NOT NULL,
                 pod_name TEXT NOT NULL,
                 pod_uid TEXT,
                 sandbox_id TEXT,
                 state TEXT NOT NULL,
                 started_at INTEGER NOT NULL,
                 stopped_at INTEGER,
                 PRIMARY KEY (workspace_id, generation)
             );
             CREATE TABLE IF NOT EXISTS exec_calls (
                 workspace_id TEXT NOT NULL,
                 call_id TEXT NOT NULL,
                 request_hash BLOB NOT NULL,
                 state TEXT NOT NULL,
                 exit_code INTEGER,
                 stdout TEXT,
                 stderr TEXT,
                 dispatched_at INTEGER NOT NULL,
                 finished_at INTEGER,
                 PRIMARY KEY (workspace_id, call_id)
             );
             CREATE TABLE IF NOT EXISTS cleanup_state (
                 workspace_id TEXT PRIMARY KEY,
                 pod_absent INTEGER NOT NULL DEFAULT 0,
                 reservation_released INTEGER NOT NULL DEFAULT 0,
                 jail_absent INTEGER NOT NULL DEFAULT 0,
                 vmm_absent INTEGER NOT NULL DEFAULT 0,
                 children_absent INTEGER NOT NULL DEFAULT 0,
                 updated_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS network_policies (
                 name TEXT PRIMARY KEY,
                 namespace TEXT NOT NULL,
                 desired_yaml TEXT NOT NULL,
                 desired_spec_sha TEXT NOT NULL,
                 observed_state TEXT NOT NULL DEFAULT 'pending',
                 observed_spec_sha TEXT,
                 updated_at INTEGER NOT NULL
             );",
        )
        .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, FabricError> {
        self.conn
            .lock()
            .map_err(|_| FabricError::Store("sqlite mutex poisoned".into()))
    }

    pub fn request_hash(command: &str) -> [u8; 32] {
        Sha256::digest(command.as_bytes()).into()
    }

    pub fn get_workspace(&self, id: &str) -> Result<Option<WorkspaceRow>, FabricError> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, state, device, node, pv_name, pvc_name, lv_name
             FROM workspaces WHERE id = ?1",
            params![id],
            workspace_from_row,
        )
        .optional()
        .map_err(|error| FabricError::Store(error.to_string()))
    }

    /// Every known workspace, oldest first; the collection view for
    /// `GET /v1/workspaces`.
    pub fn list_workspaces(&self) -> Result<Vec<WorkspaceRow>, FabricError> {
        let conn = self.lock()?;
        let mut statement = conn
            .prepare(
                "SELECT id, state, device, node, pv_name, pvc_name, lv_name
                 FROM workspaces ORDER BY created_at, id",
            )
            .map_err(|error| FabricError::Store(error.to_string()))?;
        let rows = statement
            .query_map([], workspace_from_row)
            .map_err(|error| FabricError::Store(error.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(rows)
    }

    pub fn upsert_workspace(&self, row: &WorkspaceRow) -> Result<(), FabricError> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO workspaces (id, state, device, node, pv_name, pvc_name, lv_name, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                 state=excluded.state,
                 device=excluded.device,
                 node=excluded.node,
                 pv_name=excluded.pv_name,
                 pvc_name=excluded.pvc_name,
                 lv_name=excluded.lv_name",
            params![
                row.id,
                row.state,
                row.device,
                row.node,
                row.pv_name,
                row.pvc_name,
                row.lv_name,
                now_secs()
            ],
        )
        .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(())
    }

    pub fn set_workspace_state(&self, id: &str, state: &str) -> Result<(), FabricError> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE workspaces SET state = ?2 WHERE id = ?1",
            params![id, state],
        )
        .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(())
    }

    /// Reserve `device` for `workspace_id` before any Pod is created.
    ///
    /// A different workspace that already holds the device is foreign. An
    /// existing reservation for this workspace on the same device is reused.
    /// Released rows may be taken again by the same workspace. Unknown
    /// outcomes must not call [`Store::release_reservation`].
    pub fn reserve_volume(
        &self,
        workspace_id: &str,
        device: &str,
        node: &str,
        pv_name: &str,
    ) -> Result<ReservationRow, FabricError> {
        let conn = self.lock()?;
        let existing: Option<(String, String, String)> = conn
            .query_row(
                "SELECT workspace_id, state, pv_name FROM volume_reservations
                 WHERE device = ?1 AND state = 'reserved'",
                params![device],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|error| FabricError::Store(error.to_string()))?;
        if let Some((owner, _, _)) = existing {
            if owner != workspace_id {
                return Err(FabricError::Foreign(format!(
                    "block device already reserved by workspace {owner}"
                )));
            }
        }

        let ours: Option<ReservationRow> = conn
            .query_row(
                "SELECT workspace_id, device, node, pv_name, state
                 FROM volume_reservations WHERE workspace_id = ?1",
                params![workspace_id],
                reservation_from_row,
            )
            .optional()
            .map_err(|error| FabricError::Store(error.to_string()))?;
        if let Some(row) = ours {
            if row.state == "reserved" {
                if row.device != device {
                    return Err(FabricError::Foreign(format!(
                        "workspace {workspace_id} already reserved {}",
                        row.device
                    )));
                }
                return Ok(row);
            }
            conn.execute(
                "UPDATE volume_reservations
                 SET device=?2, node=?3, pv_name=?4, state='reserved',
                     reason=NULL, reserved_at=?5, released_at=NULL
                 WHERE workspace_id=?1",
                params![workspace_id, device, node, pv_name, now_secs()],
            )
            .map_err(|error| FabricError::Store(error.to_string()))?;
        } else {
            conn.execute(
                "INSERT INTO volume_reservations
                 (workspace_id, device, node, pv_name, state, reserved_at)
                 VALUES (?1, ?2, ?3, ?4, 'reserved', ?5)",
                params![workspace_id, device, node, pv_name, now_secs()],
            )
            .map_err(|error| match error {
                rusqlite::Error::SqliteFailure(code, _)
                    if code.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    FabricError::Foreign(
                        "block device reservation collided with a foreign row".into(),
                    )
                }
                other => FabricError::Store(other.to_string()),
            })?;
        }
        Ok(ReservationRow {
            workspace_id: workspace_id.to_owned(),
            device: device.to_owned(),
            node: node.to_owned(),
            pv_name: pv_name.to_owned(),
            state: "reserved".into(),
        })
    }

    pub fn get_reservation(
        &self,
        workspace_id: &str,
    ) -> Result<Option<ReservationRow>, FabricError> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT workspace_id, device, node, pv_name, state
             FROM volume_reservations WHERE workspace_id = ?1",
            params![workspace_id],
            reservation_from_row,
        )
        .optional()
        .map_err(|error| FabricError::Store(error.to_string()))
    }

    /// Every currently held reservation, ordered by workspace id; the scan
    /// input for startup reconciliation.
    pub fn list_reserved_reservations(&self) -> Result<Vec<ReservationRow>, FabricError> {
        let conn = self.lock()?;
        let mut statement = conn
            .prepare(
                "SELECT workspace_id, device, node, pv_name, state
                 FROM volume_reservations WHERE state = 'reserved' ORDER BY workspace_id",
            )
            .map_err(|error| FabricError::Store(error.to_string()))?;
        let rows = statement
            .query_map([], reservation_from_row)
            .map_err(|error| FabricError::Store(error.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(rows)
    }

    /// Release only after the caller has positively observed absence of the
    /// realized objects. Releasing an already released row is an idempotent
    /// success so a crash after the state transition can safely resume
    /// cleanup; every other missing or non-reserved state remains an error.
    pub fn release_reservation(&self, workspace_id: &str, reason: &str) -> Result<(), FabricError> {
        let conn = self.lock()?;
        let changed = conn
            .execute(
                "UPDATE volume_reservations
                 SET state='released', reason=?2, released_at=?3
                 WHERE workspace_id=?1 AND state='reserved'",
                params![workspace_id, reason, now_secs()],
            )
            .map_err(|error| FabricError::Store(error.to_string()))?;
        if changed == 0 {
            let state: Option<String> = conn
                .query_row(
                    "SELECT state FROM volume_reservations WHERE workspace_id=?1",
                    params![workspace_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| FabricError::Store(error.to_string()))?;
            if state.as_deref() != Some("released") {
                return Err(FabricError::Store(format!(
                    "no reserved volume for workspace {workspace_id}"
                )));
            }
        }
        Ok(())
    }

    pub fn insert_generation(&self, row: &GenerationRow) -> Result<(), FabricError> {
        let conn = self.lock()?;
        conn.execute(
            "
            INSERT INTO execution_generations
                (workspace_id, generation, pod_name, pod_uid, sandbox_id, state, started_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(workspace_id, generation) DO UPDATE SET
                pod_name=excluded.pod_name,
                pod_uid=excluded.pod_uid,
                sandbox_id=excluded.sandbox_id,
                state=excluded.state,
                started_at=excluded.started_at",
            params![
                row.workspace_id,
                row.generation,
                row.pod_name,
                row.pod_uid,
                row.sandbox_id,
                row.state,
                now_secs()
            ],
        )
        .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(())
    }

    pub fn latest_generation(
        &self,
        workspace_id: &str,
    ) -> Result<Option<GenerationRow>, FabricError> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT workspace_id, generation, pod_name, pod_uid, sandbox_id, state
             FROM execution_generations
             WHERE workspace_id = ?1
             ORDER BY generation DESC LIMIT 1",
            params![workspace_id],
            generation_from_row,
        )
        .optional()
        .map_err(|error| FabricError::Store(error.to_string()))
    }

    pub fn stop_generation(
        &self,
        workspace_id: &str,
        generation: i64,
        state: &str,
    ) -> Result<(), FabricError> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE execution_generations
             SET state=?3, stopped_at=?4
             WHERE workspace_id=?1 AND generation=?2",
            params![workspace_id, generation, state, now_secs()],
        )
        .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(())
    }

    pub fn update_generation_runtime(
        &self,
        workspace_id: &str,
        generation: i64,
        pod_uid: &str,
        sandbox_id: Option<&str>,
        state: &str,
    ) -> Result<(), FabricError> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE execution_generations
             SET pod_uid=?3, sandbox_id=?4, state=?5
             WHERE workspace_id=?1 AND generation=?2",
            params![workspace_id, generation, pod_uid, sandbox_id, state],
        )
        .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(())
    }

    pub fn begin_dispatch(
        &self,
        workspace_id: &str,
        call_id: &str,
        request_hash: &[u8; 32],
    ) -> Result<BeginDispatch, FabricError> {
        let conn = self.lock()?;
        let inserted = conn
            .execute(
                "INSERT INTO exec_calls
                 (workspace_id, call_id, request_hash, state, dispatched_at)
                 VALUES (?1, ?2, ?3, 'dispatched', ?4)
                 ON CONFLICT(workspace_id, call_id) DO NOTHING",
                params![workspace_id, call_id, request_hash.as_slice(), now_secs()],
            )
            .map_err(|error| FabricError::Store(error.to_string()))?;
        if inserted == 1 {
            return Ok(BeginDispatch::ReadyToDispatch);
        }
        let row = conn
            .query_row(
                "SELECT request_hash, state, exit_code, stdout, stderr
                 FROM exec_calls WHERE workspace_id=?1 AND call_id=?2",
                params![workspace_id, call_id],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<i32>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .map_err(|error| FabricError::Store(error.to_string()))?;
        if row.0.as_slice() != request_hash.as_slice() {
            return Ok(BeginDispatch::Conflict);
        }
        match row.1.as_str() {
            "terminal" => Ok(BeginDispatch::Terminal {
                exit_code: row.2,
                stdout: row.3.unwrap_or_default(),
                stderr: row.4.unwrap_or_default(),
            }),
            _ => Ok(BeginDispatch::OutcomeUnknown),
        }
    }

    pub fn complete_exec(
        &self,
        workspace_id: &str,
        call_id: &str,
        exit_code: i32,
        stdout: &str,
        stderr: &str,
    ) -> Result<(), FabricError> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE exec_calls
             SET state='terminal', exit_code=?3, stdout=?4, stderr=?5, finished_at=?6
             WHERE workspace_id=?1 AND call_id=?2 AND state='dispatched'",
            params![workspace_id, call_id, exit_code, stdout, stderr, now_secs()],
        )
        .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(())
    }

    pub fn mark_unknown(
        &self,
        workspace_id: &str,
        call_id: &str,
        exit_code: Option<i32>,
        stdout: &str,
        stderr: &str,
    ) -> Result<(), FabricError> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE exec_calls
             SET state='unknown', exit_code=?3, stdout=?4, stderr=?5, finished_at=?6
             WHERE workspace_id=?1 AND call_id=?2 AND state='dispatched'",
            params![workspace_id, call_id, exit_code, stdout, stderr, now_secs()],
        )
        .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(())
    }

    pub fn get_exec(
        &self,
        workspace_id: &str,
        call_id: &str,
    ) -> Result<Option<ExecRow>, FabricError> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT workspace_id, call_id, request_hash, state, exit_code, stdout, stderr
             FROM exec_calls WHERE workspace_id=?1 AND call_id=?2",
            params![workspace_id, call_id],
            exec_from_row,
        )
        .optional()
        .map_err(|error| FabricError::Store(error.to_string()))
    }

    pub fn put_cleanup(&self, row: &CleanupRow) -> Result<(), FabricError> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO cleanup_state
             (workspace_id, pod_absent, reservation_released, jail_absent, vmm_absent,
              children_absent, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(workspace_id) DO UPDATE SET
                 pod_absent=excluded.pod_absent,
                 reservation_released=excluded.reservation_released,
                 jail_absent=excluded.jail_absent,
                 vmm_absent=excluded.vmm_absent,
                 children_absent=excluded.children_absent,
                 updated_at=excluded.updated_at",
            params![
                row.workspace_id,
                row.pod_absent as i64,
                row.reservation_released as i64,
                row.jail_absent as i64,
                row.vmm_absent as i64,
                row.children_absent as i64,
                now_secs()
            ],
        )
        .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(())
    }

    pub fn get_cleanup(&self, workspace_id: &str) -> Result<Option<CleanupRow>, FabricError> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT workspace_id, pod_absent, reservation_released, jail_absent,
                    vmm_absent, children_absent
             FROM cleanup_state WHERE workspace_id=?1",
            params![workspace_id],
            cleanup_from_row,
        )
        .optional()
        .map_err(|error| FabricError::Store(error.to_string()))
    }

    /// Records what this Fabric wants the guest-egress NetworkPolicy to be.
    /// The observed columns are intentionally untouched on conflict so the
    /// last observation survives desire updates.
    pub fn put_policy_desired(
        &self,
        name: &str,
        namespace: &str,
        desired_yaml: &str,
        desired_spec_sha: &str,
    ) -> Result<(), FabricError> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO network_policies
             (name, namespace, desired_yaml, desired_spec_sha, observed_state, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'pending', ?5)
             ON CONFLICT(name) DO UPDATE SET
                 namespace=excluded.namespace,
                 desired_yaml=excluded.desired_yaml,
                 desired_spec_sha=excluded.desired_spec_sha,
                 updated_at=excluded.updated_at",
            params![name, namespace, desired_yaml, desired_spec_sha, now_secs()],
        )
        .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(())
    }

    pub fn set_policy_observed(
        &self,
        name: &str,
        observed_state: &str,
        observed_spec_sha: Option<&str>,
    ) -> Result<(), FabricError> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE network_policies
             SET observed_state=?2, observed_spec_sha=?3, updated_at=?4
             WHERE name=?1",
            params![name, observed_state, observed_spec_sha, now_secs()],
        )
        .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(())
    }

    pub fn get_policy(&self, name: &str) -> Result<Option<PolicyRow>, FabricError> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT name, namespace, desired_yaml, desired_spec_sha,
                    observed_state, observed_spec_sha
             FROM network_policies WHERE name=?1",
            params![name],
            policy_from_row,
        )
        .optional()
        .map_err(|error| FabricError::Store(error.to_string()))
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn workspace_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceRow> {
    Ok(WorkspaceRow {
        id: row.get(0)?,
        state: row.get(1)?,
        device: row.get(2)?,
        node: row.get(3)?,
        pv_name: row.get(4)?,
        pvc_name: row.get(5)?,
        lv_name: row.get(6)?,
    })
}

fn reservation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReservationRow> {
    Ok(ReservationRow {
        workspace_id: row.get(0)?,
        device: row.get(1)?,
        node: row.get(2)?,
        pv_name: row.get(3)?,
        state: row.get(4)?,
    })
}

fn generation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GenerationRow> {
    Ok(GenerationRow {
        workspace_id: row.get(0)?,
        generation: row.get(1)?,
        pod_name: row.get(2)?,
        pod_uid: row.get(3)?,
        sandbox_id: row.get(4)?,
        state: row.get(5)?,
    })
}

fn exec_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExecRow> {
    Ok(ExecRow {
        workspace_id: row.get(0)?,
        call_id: row.get(1)?,
        request_hash: row.get(2)?,
        state: row.get(3)?,
        exit_code: row.get(4)?,
        stdout: row.get(5)?,
        stderr: row.get(6)?,
    })
}

fn cleanup_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CleanupRow> {
    Ok(CleanupRow {
        workspace_id: row.get(0)?,
        pod_absent: row.get::<_, i64>(1)? != 0,
        reservation_released: row.get::<_, i64>(2)? != 0,
        jail_absent: row.get::<_, i64>(3)? != 0,
        vmm_absent: row.get::<_, i64>(4)? != 0,
        children_absent: row.get::<_, i64>(5)? != 0,
    })
}

fn policy_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PolicyRow> {
    Ok(PolicyRow {
        name: row.get(0)?,
        namespace: row.get(1)?,
        desired_yaml: row.get(2)?,
        desired_spec_sha: row.get(3)?,
        observed_state: row.get(4)?,
        observed_spec_sha: row.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_store(tag: &str) -> (Store, PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "voie-fabricd-store-{}-{tag}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).expect("open sqlite");
        (store, path)
    }

    #[test]
    fn dispatch_is_at_most_once_and_retains_terminal() {
        let (store, path) = temp_store("journal");
        let hash_a = Store::request_hash("printf marker > /workspace/marker");
        let hash_b = Store::request_hash("cat /workspace/marker");

        assert_eq!(
            store.begin_dispatch("ws", "c1", &hash_a).unwrap(),
            BeginDispatch::ReadyToDispatch
        );
        assert_eq!(
            store.begin_dispatch("ws", "c1", &hash_a).unwrap(),
            BeginDispatch::OutcomeUnknown
        );
        store.complete_exec("ws", "c1", 0, "", "").unwrap();
        match store.begin_dispatch("ws", "c1", &hash_a).unwrap() {
            BeginDispatch::Terminal { exit_code, .. } => assert_eq!(exit_code, Some(0)),
            other => panic!("expected terminal, got {other:?}"),
        }
        assert_eq!(
            store.begin_dispatch("ws", "c1", &hash_b).unwrap(),
            BeginDispatch::Conflict
        );

        assert_eq!(
            store.begin_dispatch("ws", "c2", &hash_a).unwrap(),
            BeginDispatch::ReadyToDispatch
        );
        store
            .mark_unknown("ws", "c2", Some(124), "partial", "deadline")
            .unwrap();
        assert_eq!(
            store.begin_dispatch("ws", "c2", &hash_a).unwrap(),
            BeginDispatch::OutcomeUnknown
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn reservation_refuses_foreign_device_and_retains_until_release() {
        let (store, path) = temp_store("reserve");
        store
            .reserve_volume("ws-a", "/dev/loop0", "baremetal-1", "pv-a")
            .unwrap();
        let err = store
            .reserve_volume("ws-b", "/dev/loop0", "baremetal-1", "pv-b")
            .unwrap_err();
        assert!(matches!(err, FabricError::Foreign(_)), "{err}");

        store
            .release_reservation("ws-a", "positive-absence")
            .unwrap();
        // A restart can replay the release step after the durable state
        // transition. Releasing the same row again is a no-op.
        store
            .release_reservation("ws-a", "positive-absence")
            .unwrap();
        store
            .reserve_volume("ws-b", "/dev/loop0", "baremetal-1", "pv-b")
            .unwrap();
        let row = store.get_reservation("ws-b").unwrap().unwrap();
        assert_eq!(row.state, "reserved");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn policy_desire_is_durable_and_observation_is_separate() {
        let (store, path) = temp_store("policy");
        store
            .put_policy_desired("voie-guest-egress", "voie-workspace", "kind: x", "sha-a")
            .unwrap();
        let row = store.get_policy("voie-guest-egress").unwrap().unwrap();
        assert_eq!(row.observed_state, "pending");

        // Updating desire keeps the previous observation until reconfirmed.
        store
            .set_policy_observed("voie-guest-egress", "present", Some("sha-a"))
            .unwrap();
        store
            .put_policy_desired("voie-guest-egress", "voie-workspace", "kind: y", "sha-b")
            .unwrap();
        let row = store.get_policy("voie-guest-egress").unwrap().unwrap();
        assert_eq!(row.desired_spec_sha, "sha-b");
        assert_eq!(row.observed_state, "present");
        assert_eq!(row.observed_spec_sha.as_deref(), Some("sha-a"));

        store
            .set_policy_observed("voie-guest-egress", "foreign", None)
            .unwrap();
        let row = store.get_policy("voie-guest-egress").unwrap().unwrap();
        assert_eq!(row.observed_state, "foreign");
        assert_eq!(row.observed_spec_sha, None);
        let _ = std::fs::remove_file(path);
    }
}
