//! Local realization facts. Six tables, no general persistence layer.

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
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
             PRAGMA synchronous=FULL;",
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
             );
             CREATE TABLE IF NOT EXISTS product_operations (
                 kind TEXT NOT NULL,
                 resource_id TEXT NOT NULL,
                 operation_id TEXT NOT NULL,
                 request_hash TEXT NOT NULL,
                 state TEXT NOT NULL,
                 created_at INTEGER NOT NULL,
                 PRIMARY KEY (kind, resource_id, operation_id)
             );
             CREATE TABLE IF NOT EXISTS gateway_routes (
                 slug TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 service TEXT NOT NULL,
                 console_host TEXT NOT NULL,
                 PRIMARY KEY (slug, kind)
             );
             CREATE TABLE IF NOT EXISTS product_resources (
                 kind TEXT NOT NULL,
                 resource_id TEXT NOT NULL,
                 pod_name TEXT,
                 service_name TEXT,
                 artifact_hash TEXT,
                 state TEXT NOT NULL,
                 desired_yaml TEXT,
                 PRIMARY KEY (kind, resource_id)
             );
             CREATE TABLE IF NOT EXISTS volume_allocations (
                 kind TEXT NOT NULL,
                 resource_id TEXT NOT NULL,
                 lv_name TEXT NOT NULL UNIQUE,
                 allocated_bytes INTEGER NOT NULL,
                 state TEXT NOT NULL,
                 operation_id TEXT,
                 created_at INTEGER NOT NULL,
                 PRIMARY KEY (kind, resource_id)
             );",
        )
        .map_err(|error| FabricError::Store(error.to_string()))?;
        // Existing estates created the table before desired_yaml existed.
        match conn.execute(
            "ALTER TABLE product_resources ADD COLUMN desired_yaml TEXT",
            [],
        ) {
            Ok(_) => {}
            Err(error) if error.to_string().contains("duplicate column") => {}
            Err(error) => return Err(FabricError::Store(error.to_string())),
        }
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

    pub fn retarget_reservation_device(
        &self,
        workspace_id: &str,
        device: &str,
    ) -> Result<(), FabricError> {
        let conn = self.lock()?;
        let changed = conn
            .execute(
                "UPDATE volume_reservations SET device = ?2
                 WHERE workspace_id = ?1 AND state = 'reserved'",
                params![workspace_id, device],
            )
            .map_err(|error| FabricError::Store(error.to_string()))?;
        if changed == 0 {
            return Err(FabricError::NotFound);
        }
        Ok(())
    }

    pub fn retarget_workspace_device(
        &self,
        workspace_id: &str,
        device: &str,
    ) -> Result<(), FabricError> {
        self.retarget_workspace_block(workspace_id, device, None)
    }

    /// Persist the live mapper path and the LV that actually holds the
    /// workspace after restore promotion. Device-only updates leave
    /// `workspaces.lv_name` pointing at a retired volume.
    pub fn retarget_workspace_block(
        &self,
        workspace_id: &str,
        device: &str,
        lv_name: Option<&str>,
    ) -> Result<(), FabricError> {
        let conn = self.lock()?;
        let changed = if let Some(lv_name) = lv_name {
            conn.execute(
                "UPDATE workspaces SET device = ?2, lv_name = ?3 WHERE id = ?1 AND state != 'deleted'",
                params![workspace_id, device, lv_name],
            )
        } else {
            conn.execute(
                "UPDATE workspaces SET device = ?2 WHERE id = ?1 AND state != 'deleted'",
                params![workspace_id, device],
            )
        }
        .map_err(|error| FabricError::Store(error.to_string()))?;
        if changed == 0 {
            return Err(FabricError::NotFound);
        }
        Ok(())
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

    pub fn delete_cleanup(&self, workspace_id: &str) -> Result<(), FabricError> {
        let conn = self.lock()?;
        conn.execute(
            "DELETE FROM cleanup_state WHERE workspace_id=?1",
            params![workspace_id],
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

    /// At-most-once product operation journal. Same operation+hash returns
    /// the stored state; a different hash is a conflict; dispatched/unknown
    /// is never executed again. Backup and snapshot staging admits one
    /// in-flight artifact at a time.
    pub fn begin_product_operation(
        &self,
        kind: &str,
        resource_id: &str,
        operation_id: &str,
        request_hash: &str,
    ) -> Result<String, FabricError> {
        let conn = self.lock()?;
        if is_staging_kind(kind) {
            let inflight: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM product_operations
                     WHERE kind IN ('workspace-snapshot', 'database-backup',
                                    'workspace-restore-artifact', 'database-restore-artifact')
                       AND state IN ('dispatched', 'terminal')",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| FabricError::Store(error.to_string()))?;
            let existing: Option<String> = conn
                .query_row(
                    "SELECT state FROM product_operations
                     WHERE kind=?1 AND resource_id=?2 AND operation_id=?3",
                    params![kind, resource_id, operation_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| FabricError::Store(error.to_string()))?;
            let occupying = matches!(existing.as_deref(), Some("dispatched") | Some("terminal"));
            if !occupying && inflight >= 1 {
                return Err(FabricError::Conflict("staging admission is full".into()));
            }
        }
        let inserted = conn
            .execute(
                "INSERT INTO product_operations
                 (kind, resource_id, operation_id, request_hash, state, created_at)
                 VALUES (?1, ?2, ?3, ?4, 'dispatched', ?5)
                 ON CONFLICT(kind, resource_id, operation_id) DO NOTHING",
                params![kind, resource_id, operation_id, request_hash, now_secs()],
            )
            .map_err(|error| FabricError::Store(error.to_string()))?;
        if inserted == 1 {
            return Ok("dispatched".to_owned());
        }
        let (stored_hash, state): (String, String) = conn
            .query_row(
                "SELECT request_hash, state FROM product_operations
                 WHERE kind=?1 AND resource_id=?2 AND operation_id=?3",
                params![kind, resource_id, operation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| FabricError::Store(error.to_string()))?;
        if stored_hash != request_hash {
            return Err(FabricError::Conflict(
                "product operation hash conflict".into(),
            ));
        }
        if is_restore_artifact_kind(kind)
            && matches!(state.as_str(), "acked" | "unknown" | "failed")
        {
            conn.execute(
                "UPDATE product_operations
                 SET state = 'dispatched', request_hash = ?4, created_at = ?5
                 WHERE kind=?1 AND resource_id=?2 AND operation_id=?3
                   AND state IN ('acked', 'unknown', 'failed')",
                params![kind, resource_id, operation_id, request_hash, now_secs()],
            )
            .map_err(|error| FabricError::Store(error.to_string()))?;
            return Ok("dispatched".to_owned());
        }
        if state == "dispatched" || state == "unknown" {
            return Ok("unknown".to_owned());
        }
        Ok(state)
    }

    pub fn ack_staging_operation(
        &self,
        kind: &str,
        resource_id: &str,
        operation_id: &str,
    ) -> Result<(), FabricError> {
        let conn = self.lock()?;
        let updated = conn
            .execute(
                "UPDATE product_operations SET state = 'acked'
                 WHERE kind=?1 AND resource_id=?2 AND operation_id=?3
                   AND state = 'terminal'",
                params![kind, resource_id, operation_id],
            )
            .map_err(|error| FabricError::Store(error.to_string()))?;
        if updated == 0 {
            return Err(FabricError::Conflict(
                "staging operation is not terminal".into(),
            ));
        }
        Ok(())
    }

    pub fn product_operation_state(
        &self,
        kind: &str,
        resource_id: &str,
        operation_id: &str,
    ) -> Result<Option<String>, FabricError> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT state FROM product_operations
             WHERE kind=?1 AND resource_id=?2 AND operation_id=?3",
            params![kind, resource_id, operation_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| FabricError::Store(error.to_string()))
    }

    /// Crash leftovers: a dispatched staging write did not finish. The
    /// caller must unlink the staged file before releasing the slot.
    pub fn list_dispatched_staging(&self) -> Result<Vec<(String, String, String)>, FabricError> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT kind, resource_id, operation_id FROM product_operations
                 WHERE kind IN ('workspace-snapshot', 'database-backup',
                                'workspace-restore-artifact', 'database-restore-artifact')
                   AND state = 'dispatched'",
            )
            .map_err(|error| FabricError::Store(error.to_string()))?;
        let mapped = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .map_err(|error| FabricError::Store(error.to_string()))?;
        mapped
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| FabricError::Store(error.to_string()))
    }

    pub fn list_terminal_staging(&self) -> Result<Vec<(String, String, String)>, FabricError> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT kind, resource_id, operation_id FROM product_operations
                 WHERE kind IN ('workspace-snapshot', 'database-backup',
                                'workspace-restore-artifact', 'database-restore-artifact')
                   AND state = 'terminal'",
            )
            .map_err(|error| FabricError::Store(error.to_string()))?;
        let mapped = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .map_err(|error| FabricError::Store(error.to_string()))?;
        mapped
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| FabricError::Store(error.to_string()))
    }

    pub fn complete_product_operation(
        &self,
        kind: &str,
        resource_id: &str,
        operation_id: &str,
        state: &str,
    ) -> Result<(), FabricError> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE product_operations SET state = ?4
             WHERE kind=?1 AND resource_id=?2 AND operation_id=?3
               AND state = 'dispatched'",
            params![kind, resource_id, operation_id, state],
        )
        .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(())
    }

    pub fn upsert_gateway_route(
        &self,
        slug: &str,
        kind: &str,
        service: &str,
        console_host: &str,
    ) -> Result<(), FabricError> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO gateway_routes (slug, kind, service, console_host)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(slug, kind) DO UPDATE SET
                service = excluded.service,
                console_host = excluded.console_host",
            params![slug, kind, service, console_host],
        )
        .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(())
    }

    pub fn delete_gateway_route(&self, slug: &str, kind: &str) -> Result<(), FabricError> {
        let conn = self.lock()?;
        conn.execute(
            "DELETE FROM gateway_routes WHERE slug = ?1 AND kind = ?2",
            params![slug, kind],
        )
        .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(())
    }

    pub fn delete_gateway_routes_for_slug(&self, slug: &str) -> Result<(), FabricError> {
        let conn = self.lock()?;
        conn.execute("DELETE FROM gateway_routes WHERE slug = ?1", params![slug])
            .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(())
    }

    pub fn list_gateway_routes(&self) -> Result<Vec<crate::routes::RouteIntent>, FabricError> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare("SELECT slug, kind, service FROM gateway_routes ORDER BY slug, kind")
            .map_err(|error| FabricError::Store(error.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(crate::routes::RouteIntent {
                    slug: row.get(0)?,
                    kind: row.get(1)?,
                    service: row.get(2)?,
                })
            })
            .map_err(|error| FabricError::Store(error.to_string()))?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row.map_err(|error| FabricError::Store(error.to_string()))?);
        }
        Ok(items)
    }

    pub fn gateway_console_host(&self) -> Result<Option<String>, FabricError> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT console_host FROM gateway_routes LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| FabricError::Store(error.to_string()))
    }

    pub fn upsert_product_resource(
        &self,
        kind: &str,
        resource_id: &str,
        pod_name: Option<&str>,
        service_name: Option<&str>,
        artifact_hash: Option<&str>,
        state: &str,
    ) -> Result<(), FabricError> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO product_resources
             (kind, resource_id, pod_name, service_name, artifact_hash, state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(kind, resource_id) DO UPDATE SET
                pod_name = excluded.pod_name,
                service_name = excluded.service_name,
                artifact_hash = COALESCE(excluded.artifact_hash, product_resources.artifact_hash),
                state = excluded.state",
            params![
                kind,
                resource_id,
                pod_name,
                service_name,
                artifact_hash,
                state
            ],
        )
        .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(())
    }

    pub fn delete_product_resource(
        &self,
        kind: &str,
        resource_id: &str,
    ) -> Result<(), FabricError> {
        let conn = self.lock()?;
        conn.execute(
            "DELETE FROM product_resources WHERE kind = ?1 AND resource_id = ?2",
            params![kind, resource_id],
        )
        .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(())
    }

    pub fn get_product_resource(
        &self,
        kind: &str,
        resource_id: &str,
    ) -> Result<Option<(Option<String>, Option<String>, String)>, FabricError> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT pod_name, service_name, state FROM product_resources
             WHERE kind = ?1 AND resource_id = ?2",
            params![kind, resource_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|error| FabricError::Store(error.to_string()))
    }

    pub fn set_product_desired_yaml(
        &self,
        kind: &str,
        resource_id: &str,
        yaml: &str,
    ) -> Result<(), FabricError> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE product_resources SET desired_yaml = ?3
             WHERE kind = ?1 AND resource_id = ?2",
            params![kind, resource_id, yaml],
        )
        .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(())
    }

    pub fn product_desired_yaml(
        &self,
        kind: &str,
        resource_id: &str,
    ) -> Result<Option<String>, FabricError> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT desired_yaml FROM product_resources
             WHERE kind = ?1 AND resource_id = ?2",
            params![kind, resource_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map(|value| value.flatten().filter(|yaml| !yaml.is_empty()))
        .map_err(|error| FabricError::Store(error.to_string()))
    }

    pub fn delete_product_operations(
        &self,
        kind: &str,
        resource_id: &str,
    ) -> Result<(), FabricError> {
        let conn = self.lock()?;
        conn.execute(
            "DELETE FROM product_operations WHERE kind = ?1 AND resource_id = ?2",
            params![kind, resource_id],
        )
        .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(())
    }

    pub fn reserve_allocation(
        &self,
        kind: crate::storage::VolumeKind,
        resource_id: &str,
        lv_name: &str,
        allocated_bytes: u64,
        operation_id: Option<&str>,
    ) -> Result<crate::storage::VolumeAllocation, FabricError> {
        if let Some(existing) = self.get_allocation(kind, resource_id)? {
            if existing.lv_name != lv_name {
                return Err(FabricError::Conflict(format!(
                    "{kind} {resource_id} already allocated {existing_lv}",
                    kind = kind.as_str(),
                    existing_lv = existing.lv_name
                )));
            }
            return Ok(existing);
        }
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO volume_allocations
             (kind, resource_id, lv_name, allocated_bytes, state, operation_id, created_at)
             VALUES (?1, ?2, ?3, ?4, 'allocated', ?5, ?6)",
            params![
                kind.as_str(),
                resource_id,
                lv_name,
                allocated_bytes as i64,
                operation_id,
                now_secs()
            ],
        )
        .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(crate::storage::VolumeAllocation {
            kind,
            resource_id: resource_id.to_owned(),
            lv_name: lv_name.to_owned(),
            allocated_bytes,
            state: "allocated".into(),
            operation_id: operation_id.map(ToOwned::to_owned),
        })
    }

    pub fn get_allocation(
        &self,
        kind: crate::storage::VolumeKind,
        resource_id: &str,
    ) -> Result<Option<crate::storage::VolumeAllocation>, FabricError> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT kind, resource_id, lv_name, allocated_bytes, state, operation_id
             FROM volume_allocations WHERE kind = ?1 AND resource_id = ?2",
            params![kind.as_str(), resource_id],
            allocation_from_row,
        )
        .optional()
        .map_err(|error| FabricError::Store(error.to_string()))
    }

    pub fn delete_allocation(
        &self,
        kind: crate::storage::VolumeKind,
        resource_id: &str,
    ) -> Result<(), FabricError> {
        let conn = self.lock()?;
        conn.execute(
            "DELETE FROM volume_allocations WHERE kind = ?1 AND resource_id = ?2",
            params![kind.as_str(), resource_id],
        )
        .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(())
    }

    pub fn promote_restore_to_database(&self, resource_id: &str) -> Result<(), FabricError> {
        self.promote_restore(resource_id, crate::storage::VolumeKind::Database)
    }

    pub fn promote_restore(
        &self,
        resource_id: &str,
        kind: crate::storage::VolumeKind,
    ) -> Result<(), FabricError> {
        let Some(source) = kind.restore_source() else {
            return Err(FabricError::Config(
                "only workspace or database restore candidates can be promoted",
            ));
        };
        let conn = self.lock()?;
        conn.execute(
            "DELETE FROM volume_allocations WHERE kind = ?1 AND resource_id = ?2",
            params![kind.as_str(), resource_id],
        )
        .map_err(|error| FabricError::Store(error.to_string()))?;
        let changed = conn
            .execute(
                "UPDATE volume_allocations SET kind = ?1
                 WHERE kind = ?2 AND resource_id = ?3",
                params![kind.as_str(), source.as_str(), resource_id],
            )
            .map_err(|error| FabricError::Store(error.to_string()))?;
        if changed == 0 {
            // Leftover linear-layout rows used kind `restore` for Databases.
            if kind == crate::storage::VolumeKind::Database {
                let legacy = conn
                    .execute(
                        "UPDATE volume_allocations SET kind = ?1
                         WHERE kind = 'restore' AND resource_id = ?2",
                        params![kind.as_str(), resource_id],
                    )
                    .map_err(|error| FabricError::Store(error.to_string()))?;
                if legacy == 0 {
                    return Err(FabricError::NotFound);
                }
            } else {
                return Err(FabricError::NotFound);
            }
        }
        Ok(())
    }

    pub fn update_allocation_bytes(
        &self,
        kind: crate::storage::VolumeKind,
        resource_id: &str,
        allocated_bytes: u64,
    ) -> Result<(), FabricError> {
        let conn = self.lock()?;
        let changed = conn
            .execute(
                "UPDATE volume_allocations SET allocated_bytes = ?3
                 WHERE kind = ?1 AND resource_id = ?2",
                params![kind.as_str(), resource_id, allocated_bytes as i64],
            )
            .map_err(|error| FabricError::Store(error.to_string()))?;
        if changed == 0 {
            return Err(FabricError::NotFound);
        }
        Ok(())
    }

    pub fn list_allocations(&self) -> Result<Vec<crate::storage::VolumeAllocation>, FabricError> {
        let conn = self.lock()?;
        let mut statement = conn
            .prepare(
                "SELECT kind, resource_id, lv_name, allocated_bytes, state, operation_id
                 FROM volume_allocations ORDER BY created_at, lv_name",
            )
            .map_err(|error| FabricError::Store(error.to_string()))?;
        let rows = statement
            .query_map([], allocation_from_row)
            .map_err(|error| FabricError::Store(error.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(rows)
    }

    pub fn workspace_allocated_bytes(&self) -> Result<u64, FabricError> {
        self.allocated_bytes_by_kind(crate::storage::VolumeKind::Workspace)
    }

    pub fn workspace_restore_allocated_bytes(&self) -> Result<u64, FabricError> {
        self.allocated_bytes_by_kind(crate::storage::VolumeKind::WorkspaceRestore)
    }

    pub fn linear_allocated_bytes(&self) -> Result<u64, FabricError> {
        let conn = self.lock()?;
        let total: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(allocated_bytes), 0) FROM volume_allocations
                 WHERE kind IN ('database', 'deployment')",
                [],
                |row| row.get(0),
            )
            .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(total.max(0) as u64)
    }

    pub fn database_restore_allocated_bytes(&self) -> Result<u64, FabricError> {
        let conn = self.lock()?;
        let total: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(allocated_bytes), 0) FROM volume_allocations
                 WHERE kind IN ('database_restore', 'restore')",
                [],
                |row| row.get(0),
            )
            .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(total.max(0) as u64)
    }

    pub fn normal_allocated_bytes(&self) -> Result<u64, FabricError> {
        self.linear_allocated_bytes()
    }

    pub fn allocated_bytes_by_kind(
        &self,
        kind: crate::storage::VolumeKind,
    ) -> Result<u64, FabricError> {
        let conn = self.lock()?;
        let total: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(allocated_bytes), 0) FROM volume_allocations
                 WHERE kind = ?1",
                params![kind.as_str()],
                |row| row.get(0),
            )
            .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(total.max(0) as u64)
    }

    pub fn claimed_lv_names(&self) -> Result<Vec<String>, FabricError> {
        let conn = self.lock()?;
        let mut statement = conn
            .prepare("SELECT lv_name FROM volume_allocations")
            .map_err(|error| FabricError::Store(error.to_string()))?;
        let rows = statement
            .query_map([], |row| row.get(0))
            .map_err(|error| FabricError::Store(error.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| FabricError::Store(error.to_string()))?;
        Ok(rows)
    }
}

fn is_staging_kind(kind: &str) -> bool {
    matches!(
        kind,
        "workspace-snapshot"
            | "database-backup"
            | "workspace-restore-artifact"
            | "database-restore-artifact"
    )
}

fn is_restore_artifact_kind(kind: &str) -> bool {
    matches!(
        kind,
        "workspace-restore-artifact" | "database-restore-artifact"
    )
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

fn allocation_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<crate::storage::VolumeAllocation> {
    let kind = crate::storage::VolumeKind::parse(&row.get::<_, String>(0)?)
        .ok_or_else(|| rusqlite::Error::InvalidQuery)?;
    Ok(crate::storage::VolumeAllocation {
        kind,
        resource_id: row.get(1)?,
        lv_name: row.get(2)?,
        allocated_bytes: row.get::<_, i64>(3)?.max(0) as u64,
        state: row.get(4)?,
        operation_id: row.get(5)?,
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

        // Restore switch: the same workspace must be able to reserve the
        // candidate mapper after the live volume is dropped.
        store.release_reservation("ws-b", "restore-switch").unwrap();
        store
            .reserve_volume("ws-b", "/dev/loop1", "baremetal-1", "pv-b2")
            .unwrap();
        let row = store.get_reservation("ws-b").unwrap().unwrap();
        assert_eq!(row.device, "/dev/loop1");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn stale_dm_n_reservation_does_not_own_a_stable_mapper_path() {
        let (store, path) = temp_store("dm-recycle");
        store
            .reserve_volume("ws-a", "/dev/dm-29", "baremetal-1", "pv-a")
            .unwrap();
        store
            .reserve_volume(
                "ws-b",
                "/dev/mapper/voie-crypt-wsbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "baremetal-1",
                "pv-b",
            )
            .unwrap();
        store
            .retarget_reservation_device(
                "ws-a",
                "/dev/mapper/voie-crypt-wsaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .unwrap();
        let row = store.get_reservation("ws-a").unwrap().unwrap();
        assert_eq!(
            row.device,
            "/dev/mapper/voie-crypt-wsaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
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

    #[test]
    fn gateway_routes_and_product_operations_are_durable() {
        let (store, path) = temp_store("product-routes");
        assert_eq!(
            store
                .begin_product_operation("deployment", "d1", "op1", "hash-a")
                .unwrap(),
            "dispatched"
        );
        assert_eq!(
            store
                .begin_product_operation("deployment", "d1", "op1", "hash-a")
                .unwrap(),
            "unknown"
        );
        store
            .complete_product_operation("deployment", "d1", "op1", "ready")
            .unwrap();
        assert_eq!(
            store
                .begin_product_operation("deployment", "d1", "op1", "hash-a")
                .unwrap(),
            "ready"
        );
        store
            .upsert_gateway_route(
                "invoice-demo",
                "dev",
                "app-invoice-demo-dev:3000",
                "console.test",
            )
            .unwrap();
        let items = store.list_gateway_routes().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].slug, "invoice-demo");
        store
            .delete_gateway_routes_for_slug("invoice-demo")
            .unwrap();
        assert!(store.list_gateway_routes().unwrap().is_empty());
        store
            .upsert_product_resource("deployment", "d1", Some("pod"), None, None, "starting")
            .unwrap();
        store
            .set_product_desired_yaml("deployment", "d1", "kind: Pod\n")
            .unwrap();
        assert_eq!(
            store.product_desired_yaml("deployment", "d1").unwrap(),
            Some("kind: Pod\n".into())
        );
        assert!(store
            .get_product_resource("deployment", "d1")
            .unwrap()
            .is_some());
        store.delete_product_operations("deployment", "d1").unwrap();
        store.delete_product_resource("deployment", "d1").unwrap();
        assert!(
            store
                .get_product_resource("deployment", "d1")
                .unwrap()
                .is_none(),
            "purged Fabric journal row must not remain"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn staging_operations_admit_one_inflight_artifact() {
        let (store, path) = temp_store("staging-cap");
        assert_eq!(
            store
                .begin_product_operation("database-backup", "db1", "op-a", "hash-a")
                .unwrap(),
            "dispatched"
        );
        let conflict = store.begin_product_operation("workspace-snapshot", "ws1", "op-b", "hash-b");
        assert!(
            matches!(conflict, Err(crate::FabricError::Conflict(_))),
            "second staging dispatch must refuse: {conflict:?}"
        );
        store
            .complete_product_operation("database-backup", "db1", "op-a", "terminal")
            .unwrap();
        let still_full =
            store.begin_product_operation("workspace-snapshot", "ws1", "op-b", "hash-b");
        assert!(
            matches!(still_full, Err(crate::FabricError::Conflict(_))),
            "terminal-unacked staging still occupies the slot: {still_full:?}"
        );
        store
            .ack_staging_operation("database-backup", "db1", "op-a")
            .unwrap();
        assert_eq!(
            store
                .begin_product_operation("workspace-snapshot", "ws1", "op-b", "hash-b")
                .unwrap(),
            "dispatched"
        );
        store
            .complete_product_operation("workspace-snapshot", "ws1", "op-b", "terminal")
            .unwrap();
        store
            .ack_staging_operation("workspace-snapshot", "ws1", "op-b")
            .unwrap();
        assert_eq!(
            store
                .begin_product_operation("workspace-restore-artifact", "ws1", "artifact", "put")
                .unwrap(),
            "dispatched"
        );
        let restore_conflict =
            store.begin_product_operation("database-backup", "db2", "op-c", "hash-c");
        assert!(
            matches!(restore_conflict, Err(crate::FabricError::Conflict(_))),
            "restore artifact occupies the same staging slot: {restore_conflict:?}"
        );
        store
            .complete_product_operation("workspace-restore-artifact", "ws1", "artifact", "terminal")
            .unwrap();
        store
            .ack_staging_operation("workspace-restore-artifact", "ws1", "artifact")
            .unwrap();
        assert_eq!(
            store
                .begin_product_operation("workspace-restore-artifact", "ws1", "artifact", "put")
                .unwrap(),
            "dispatched",
            "acked restore artifact must re-admit the same resource"
        );
        store
            .complete_product_operation("workspace-restore-artifact", "ws1", "artifact", "failed")
            .unwrap();
        assert_eq!(
            store
                .begin_product_operation("workspace-snapshot", "ws2", "op-d", "hash-d")
                .unwrap(),
            "dispatched",
            "failed restore artifact must release the global staging slot"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn restore_candidate_promotes_to_workspace_without_renaming_the_lv() {
        let (store, path) = temp_store("promote-restore-ws");
        let op = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
        let lv = format!(
            "rst{}",
            op.chars().filter(|ch| *ch != '-').collect::<String>()
        );
        store
            .reserve_allocation(
                crate::storage::VolumeKind::WorkspaceRestore,
                "ws-1",
                &lv,
                16 * 1024 * 1024 * 1024,
                Some(op),
            )
            .unwrap();
        store
            .promote_restore("ws-1", crate::storage::VolumeKind::Workspace)
            .unwrap();
        let row = store
            .get_allocation(crate::storage::VolumeKind::Workspace, "ws-1")
            .unwrap()
            .expect("promoted");
        assert_eq!(row.lv_name, lv);
        assert!(store
            .get_allocation(crate::storage::VolumeKind::WorkspaceRestore, "ws-1")
            .unwrap()
            .is_none());
        assert!(store
            .promote_restore("ws-1", crate::storage::VolumeKind::WorkspaceRestore)
            .is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn retarget_workspace_block_persists_restore_lv_name() {
        let (store, path) = temp_store("retarget-lv");
        store
            .upsert_workspace(&WorkspaceRow {
                id: "ws-1".into(),
                state: "ready".into(),
                device: "/dev/dm-4".into(),
                node: "baremetal-1".into(),
                pv_name: "voie-ws-ws-1".into(),
                pvc_name: "voie-ws-ws-1".into(),
                lv_name: Some("wsdeadbeef".into()),
            })
            .unwrap();
        store
            .retarget_workspace_block(
                "ws-1",
                "/dev/mapper/voie-crypt-rsta2b0705",
                Some("rsta2b0705"),
            )
            .unwrap();
        let row = store.get_workspace("ws-1").unwrap().expect("row");
        assert_eq!(row.device, "/dev/mapper/voie-crypt-rsta2b0705");
        assert_eq!(row.lv_name.as_deref(), Some("rsta2b0705"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sqlite_opens_with_full_synchronous() {
        let (store, path) = temp_store("pragma-sync");
        let mode: i32 = store
            .lock()
            .expect("store lock")
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .expect("pragma reads");
        assert_eq!(mode, 2, "FULL is integer 2, got {mode}");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn listing_dispatched_staging_does_not_release_the_slot() {
        let (store, path) = temp_store("staging-list");
        assert_eq!(
            store
                .begin_product_operation("database-backup", "db1", "op-a", "hash-a")
                .unwrap(),
            "dispatched"
        );
        let listed = store.list_dispatched_staging().unwrap();
        assert_eq!(listed.len(), 1);
        let still_full =
            store.begin_product_operation("workspace-snapshot", "ws1", "op-b", "hash-b");
        assert!(
            matches!(still_full, Err(crate::FabricError::Conflict(_))),
            "listing dispatched leftovers must not release the slot: {still_full:?}"
        );
        let _ = std::fs::remove_file(path);
    }
}
