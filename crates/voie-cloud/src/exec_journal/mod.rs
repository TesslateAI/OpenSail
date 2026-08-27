//! Cloud exec journal: unique `(workspace_id, call_id)` and at-most-one dispatch.

use std::error::Error;
use std::fmt;

use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::fabric_client::{FabricClient, FabricError};

#[derive(Debug)]
pub enum JournalError {
    Database,
    /// The call resolved to an unknown outcome before this completion.
    ResolvedUnknown,
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JournalError::Database => write!(f, "database operation failed"),
            JournalError::ResolvedUnknown => {
                write!(f, "exec call already resolved to an unknown outcome")
            }
        }
    }
}

impl Error for JournalError {}

impl From<sqlx::Error> for JournalError {
    fn from(_: sqlx::Error) -> Self {
        JournalError::Database
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeginDispatch {
    /// `dispatched` is durable; the caller may send one Fabric request.
    ReadyToDispatch,
    Terminal {
        result: String,
    },
    OutcomeUnknown,
    Conflict,
}

impl BeginDispatch {
    /// Retained terminal text, when a previous attempt produced a result.
    pub fn retained_result(&self) -> Option<&str> {
        match self {
            BeginDispatch::Terminal { result } => Some(result),
            _ => None,
        }
    }

    /// True exactly when the call dispatched or resolved unknown and no
    /// outcome ever arrived; callers must not retry the effect.
    pub fn is_outcome_unknown(&self) -> bool {
        matches!(self, BeginDispatch::OutcomeUnknown)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecOutcome {
    Terminal { result: String },
    OutcomeUnknown,
    Conflict,
}

impl ExecOutcome {
    /// Retained terminal text of the one attempt, when definitive.
    pub fn retained_result(&self) -> Option<&str> {
        match self {
            ExecOutcome::Terminal { result } => Some(result),
            _ => None,
        }
    }

    /// True exactly when no outcome ever arrived; callers record an aborted
    /// effect and never redispatch.
    pub fn is_outcome_unknown(&self) -> bool {
        matches!(self, ExecOutcome::OutcomeUnknown)
    }
}

pub struct ExecJournal {
    pool: PgPool,
}

impl ExecJournal {
    pub fn new(pool: PgPool) -> Self {
        ExecJournal { pool }
    }

    pub fn request_hash(command: &str) -> [u8; 32] {
        Sha256::digest(command.as_bytes()).into()
    }

    /// Persist `dispatched` before returning `ReadyToDispatch`.
    pub async fn begin_dispatch(
        &self,
        workspace_id: Uuid,
        call_id: &str,
        request_hash: &[u8; 32],
    ) -> Result<BeginDispatch, JournalError> {
        let inserted = sqlx::query(
            "insert into exec_calls (workspace_id, call_id, request_hash, state) \
             values ($1, $2, $3, 'accepted') \
             on conflict (workspace_id, call_id) do nothing \
             returning call_id",
        )
        .bind(workspace_id)
        .bind(call_id)
        .bind(request_hash.as_slice())
        .fetch_optional(&self.pool)
        .await?;
        if inserted.is_some() {
            sqlx::query(
                "update exec_calls set state = 'dispatched' \
                 where workspace_id = $1 and call_id = $2 and state = 'accepted'",
            )
            .bind(workspace_id)
            .bind(call_id)
            .execute(&self.pool)
            .await?;
            return Ok(BeginDispatch::ReadyToDispatch);
        }
        let row = sqlx::query(
            "select request_hash, state, result \
             from exec_calls where workspace_id = $1 and call_id = $2",
        )
        .bind(workspace_id)
        .bind(call_id)
        .fetch_one(&self.pool)
        .await?;
        let stored: Vec<u8> = row.get("request_hash");
        if stored.as_slice() != request_hash.as_slice() {
            return Ok(BeginDispatch::Conflict);
        }
        let state: String = row.get("state");
        match state.as_str() {
            "terminal" => Ok(BeginDispatch::Terminal {
                result: row.get::<Option<String>, _>("result").unwrap_or_default(),
            }),
            _ => Ok(BeginDispatch::OutcomeUnknown),
        }
    }

    pub async fn complete(
        &self,
        workspace_id: Uuid,
        call_id: &str,
        request_hash: &[u8; 32],
        result: &str,
    ) -> Result<(), JournalError> {
        let updated = sqlx::query(
            "update exec_calls set state = 'terminal', result = $4 \
             where workspace_id = $1 and call_id = $2 and request_hash = $3 \
             and state = 'dispatched'",
        )
        .bind(workspace_id)
        .bind(call_id)
        .bind(request_hash.as_slice())
        .bind(result)
        .execute(&self.pool)
        .await?;
        if updated.rows_affected() == 1 {
            return Ok(());
        }
        // The dispatch already resolved some other way.
        let row = sqlx::query(
            "select state, result from exec_calls \
             where workspace_id = $1 and call_id = $2 and request_hash = $3",
        )
        .bind(workspace_id)
        .bind(call_id)
        .bind(request_hash.as_slice())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(JournalError::Database)?;
        let state: String = row.get("state");
        match state.as_str() {
            "terminal" => {
                let retained: Option<String> = row.get("result");
                if retained.as_deref() == Some(result) {
                    Ok(())
                } else {
                    Err(JournalError::Database)
                }
            }
            "unknown" => Err(JournalError::ResolvedUnknown),
            _ => Err(JournalError::Database),
        }
    }

    /// Durably records that the one dispatch attempt's outcome never arrived.
    async fn mark_unknown(
        &self,
        workspace_id: Uuid,
        call_id: &str,
        request_hash: &[u8; 32],
    ) -> Result<(), JournalError> {
        sqlx::query(
            "update exec_calls set state = 'unknown' \
             where workspace_id = $1 and call_id = $2 and request_hash = $3 \
             and state in ('accepted', 'dispatched')",
        )
        .bind(workspace_id)
        .bind(call_id)
        .bind(request_hash.as_slice())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// One Fabric request after durable `dispatched`. Never redispatches.
    pub async fn execute(
        &self,
        fabric: &FabricClient,
        workspace_id: Uuid,
        call_id: &str,
        command: &str,
    ) -> Result<ExecOutcome, JournalError> {
        let hash = Self::request_hash(command);
        match self.begin_dispatch(workspace_id, call_id, &hash).await? {
            BeginDispatch::Conflict => Ok(ExecOutcome::Conflict),
            BeginDispatch::Terminal { result } => Ok(ExecOutcome::Terminal { result }),
            BeginDispatch::OutcomeUnknown => Ok(ExecOutcome::OutcomeUnknown),
            BeginDispatch::ReadyToDispatch => {
                match send_once(fabric, workspace_id, call_id, command).await {
                    Ok(Some(result)) => {
                        let payload = result.payload();
                        self.complete(workspace_id, call_id, &hash, &payload)
                            .await?;
                        Ok(ExecOutcome::Terminal { result: payload })
                    }
                    Ok(None) => {
                        self.mark_unknown(workspace_id, call_id, &hash).await?;
                        Ok(ExecOutcome::OutcomeUnknown)
                    }
                    Err(_) => {
                        self.mark_unknown(workspace_id, call_id, &hash).await?;
                        Ok(ExecOutcome::OutcomeUnknown)
                    }
                }
            }
        }
    }
}

async fn send_once(
    fabric: &FabricClient,
    workspace_id: Uuid,
    call_id: &str,
    command: &str,
) -> Result<Option<crate::fabric_client::ExecResult>, FabricError> {
    // One request; `voie-fabricd` answers after its single attempt.
    let row = fabric.exec(workspace_id, call_id, command).await?;
    if row.is_completed() {
        Ok(Some(row))
    } else {
        Ok(None)
    }
}
