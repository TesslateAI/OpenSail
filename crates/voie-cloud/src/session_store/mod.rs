//! Canonical Session history: PostgreSQL ordered references to immutable Azure Blob bytes.

mod blob;

use std::error::Error;
use std::fmt;

use sha2::{Digest, Sha256};
use sqlx::postgres::PgConnection;
use sqlx::{Connection, PgPool, Row};
use uuid::Uuid;

pub use blob::{BlobStore, BlobStoreError};

use crate::Session;
use crate::model::ModelUsage;

/// Session writer fencing and Blob-append failures. Display never includes
/// database URLs, Blob keys, or other secret material.
#[derive(Debug)]
pub enum StoreError {
    Database,
    NotFound,
    Fenced,
    Conflict,
    Revision,
    Blob,
    Hash,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Database => write!(f, "database operation failed"),
            StoreError::NotFound => write!(f, "session not found"),
            StoreError::Fenced => write!(f, "session writer was fenced"),
            StoreError::Conflict => write!(f, "append id reused with different content"),
            StoreError::Revision => write!(f, "expected revision does not match session head"),
            StoreError::Blob => write!(f, "blob operation failed"),
            StoreError::Hash => write!(f, "blob bytes do not match the recorded content hash"),
        }
    }
}

impl Error for StoreError {}

impl From<sqlx::Error> for StoreError {
    fn from(_: sqlx::Error) -> Self {
        StoreError::Database
    }
}

impl From<BlobStoreError> for StoreError {
    fn from(_: BlobStoreError) -> Self {
        StoreError::Blob
    }
}

/// PostgreSQL metadata for one canonical event. Bytes live in Blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEventRef {
    pub session_id: Uuid,
    /// Monotonic PostgreSQL sequence across every Session event.
    pub global_seq: i64,
    pub revision: i64,
    pub append_id: Uuid,
    pub object_key: String,
    pub content_hash: [u8; 32],
    pub byte_length: i64,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
}

/// Loaded ordered history entry: PostgreSQL reference plus Blob bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedEvent {
    pub reference: SessionEventRef,
    pub bytes: Vec<u8>,
}

/// Current Session head fields owned by PostgreSQL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionHead {
    pub writer_generation: i64,
    pub attention_generation: i64,
    pub head_revision: i64,
}

/// Caller-supplied event body. `voie-cloud` hashes the exact bytes.
#[derive(Debug, Clone)]
pub struct AppendEvent {
    pub append_id: Uuid,
    /// Caller expectation for the pinned writer; a mismatch is a fence.
    pub writer_generation: i64,
    pub expected_revision: i64,
    pub bytes: Vec<u8>,
    pub model_usage: Option<ModelUsage>,
}

/// Azure Blob + PostgreSQL Session history.
#[derive(Clone)]
pub struct SessionStore {
    pool: PgPool,
    blob: BlobStore,
}

impl SessionStore {
    pub fn new(pool: PgPool, blob: BlobStore) -> Self {
        SessionStore { pool, blob }
    }

    pub fn blob(&self) -> &BlobStore {
        &self.blob
    }

    /// Creates Session metadata with caller-supplied identity and empty
    /// history (`head_revision = 0`).
    pub async fn create_session(
        &self,
        session_id: Uuid,
        project_id: Uuid,
        agent_id: Uuid,
        workspace_id: Uuid,
    ) -> Result<Session, StoreError> {
        let row = sqlx::query(
            "insert into sessions (id, project_id, agent_id, workspace_id) \
             values ($1, $2, $3, $4) \
             returning id, project_id, agent_id, workspace_id, \
                       writer_generation, attention_generation, head_revision",
        )
        .bind(session_id)
        .bind(project_id)
        .bind(agent_id)
        .bind(workspace_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| {
            if let sqlx::Error::Database(inner) = &error {
                if inner.kind() == sqlx::error::ErrorKind::ForeignKeyViolation {
                    return StoreError::NotFound;
                }
            }
            StoreError::Database
        })?;
        Ok(Session {
            id: row.get("id"),
            project_id: row.get("project_id"),
            agent_id: row.get("agent_id"),
            workspace_id: row.get("workspace_id"),
            writer_generation: row.get("writer_generation"),
            attention_generation: row.get("attention_generation"),
            head_revision: row.get("head_revision"),
            last_actor_user_id: row.get("last_actor_user_id"),
        })
    }

    /// Create-mode bootstrap hook. Existing non-empty history is refused
    /// rather than silently replaced.
    pub async fn bootstrap_history(&self, session_id: Uuid) -> Result<SessionHead, StoreError> {
        let head = self.inspect_head(session_id).await?;
        if head.head_revision != 0 {
            return Err(StoreError::Conflict);
        }
        Ok(head)
    }

    /// Named Session resource hook used by activation assembly.
    pub async fn bootstrap_session(&self, session_id: Uuid) -> Result<SessionHead, StoreError> {
        self.bootstrap_history(session_id).await
    }

    /// Resume-mode hook. Loading verifies PostgreSQL references against the
    /// immutable Blob bytes before an activation can act on the Session.
    pub async fn resume_history(&self, session_id: Uuid) -> Result<Vec<LoadedEvent>, StoreError> {
        let events = self.load_history(session_id).await?;
        sqlx::query(
            "update sessions set attention_generation = attention_generation + 1 \
             where id = $1",
        )
        .bind(session_id)
        .execute(&self.pool)
        .await?;
        Ok(events)
    }

    /// Named Session resource hook used by activation assembly.
    pub async fn resume_session(&self, session_id: Uuid) -> Result<Vec<LoadedEvent>, StoreError> {
        self.resume_history(session_id).await
    }

    pub async fn inspect_head(&self, session_id: Uuid) -> Result<SessionHead, StoreError> {
        let row = sqlx::query(
            "select writer_generation, attention_generation, head_revision \
             from sessions where id = $1",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        Ok(SessionHead {
            writer_generation: row.get("writer_generation"),
            attention_generation: row.get("attention_generation"),
            head_revision: row.get("head_revision"),
        })
    }

    pub async fn load_history(&self, session_id: Uuid) -> Result<Vec<LoadedEvent>, StoreError> {
        let rows = sqlx::query(
            "select session_id, global_seq, revision, append_id, object_key, content_hash, byte_length, \
                    prompt_tokens, completion_tokens, total_tokens \
             from session_events where session_id = $1 order by revision",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;
        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            let reference = event_ref_from_row(row)?;
            let bytes = self.blob.get(&reference.object_key).await?;
            if sha256(&bytes) != reference.content_hash {
                return Err(StoreError::Hash);
            }
            events.push(LoadedEvent { reference, bytes });
        }
        Ok(events)
    }

    /// Loads canonical events after the global cursor, preserving PostgreSQL
    /// order across every supplied Session. The caller supplies identities
    /// that were already authorized by the Project policy.
    pub async fn load_after_global(
        &self,
        session_ids: &[Uuid],
        cursor: i64,
        limit: i64,
    ) -> Result<Vec<LoadedEvent>, StoreError> {
        if session_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "select session_id, global_seq, revision, append_id, object_key, content_hash, byte_length, \
                    prompt_tokens, completion_tokens, total_tokens \
             from session_events \
             where session_id = any($1) and global_seq > $2 \
             order by global_seq limit $3",
        )
        .bind(session_ids)
        .bind(cursor)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            let reference = event_ref_from_row(row)?;
            let bytes = self.blob.get(&reference.object_key).await?;
            if sha256(&bytes) != reference.content_hash {
                return Err(StoreError::Hash);
            }
            events.push(LoadedEvent { reference, bytes });
        }
        Ok(events)
    }

    /// Pins one PostgreSQL connection, takes the Session advisory lock, and
    /// advances `writer_generation`. Dropping the writer closes the connection
    /// and releases the lock.
    pub async fn writer(&self, session_id: Uuid) -> Result<SessionWriter, StoreError> {
        SessionWriter::acquire(&self.pool, session_id).await
    }
}

/// One dedicated Session writer connection. Concurrency fencing, not IAM.
pub struct SessionWriter {
    conn: PgConnection,
    session_id: Uuid,
    writer_generation: i64,
}

impl SessionWriter {
    async fn acquire(pool: &PgPool, session_id: Uuid) -> Result<Self, StoreError> {
        let mut conn = pool.acquire().await?.detach();
        let (key1, key2) = advisory_keys(session_id);
        sqlx::query("select pg_advisory_lock($1, $2)")
            .bind(key1)
            .bind(key2)
            .execute(&mut conn)
            .await?;
        let writer_generation = sqlx::query_scalar::<_, i64>(
            "update sessions set writer_generation = writer_generation + 1 \
             where id = $1 returning writer_generation",
        )
        .bind(session_id)
        .fetch_optional(&mut conn)
        .await?
        .ok_or(StoreError::NotFound)?;
        Ok(SessionWriter {
            conn,
            session_id,
            writer_generation,
        })
    }

    pub fn writer_generation(&self) -> i64 {
        self.writer_generation
    }

    /// Append order: fence (writer generation and revision expectations),
    /// identical append_id retry, content hash, immutable Blob create, then
    /// PostgreSQL reference + head. External effects belong after this
    /// returns success.
    pub async fn append(
        &mut self,
        blob: &BlobStore,
        event: AppendEvent,
    ) -> Result<i64, StoreError> {
        if event.writer_generation != self.writer_generation {
            return Err(StoreError::Fenced);
        }
        let content_hash = sha256(&event.bytes);
        let byte_length = i64::try_from(event.bytes.len()).map_err(|_| StoreError::Revision)?;

        let row =
            sqlx::query("select writer_generation, head_revision from sessions where id = $1")
                .bind(self.session_id)
                .fetch_optional(&mut self.conn)
                .await?
                .ok_or(StoreError::NotFound)?;
        let current_generation: i64 = row.get("writer_generation");
        let head_revision: i64 = row.get("head_revision");
        if current_generation != self.writer_generation {
            return Err(StoreError::Fenced);
        }

        if let Some(existing) = sqlx::query(
            "select revision, content_hash from session_events \
             where session_id = $1 and append_id = $2",
        )
        .bind(self.session_id)
        .bind(event.append_id)
        .fetch_optional(&mut self.conn)
        .await?
        {
            let stored: Vec<u8> = existing.get("content_hash");
            if stored.as_slice() != content_hash.as_slice() {
                return Err(StoreError::Conflict);
            }
            return Ok(existing.get("revision"));
        }

        if event.expected_revision != head_revision + 1 {
            return Err(StoreError::Revision);
        }

        let object_key = object_key(self.session_id, event.expected_revision, &content_hash);
        blob.put_if_absent(&object_key, &event.bytes).await?;

        let (prompt_tokens, completion_tokens, total_tokens) = match event.model_usage {
            Some(usage) => (
                Some(i64::from(usage.prompt_tokens)),
                Some(i64::from(usage.completion_tokens)),
                Some(i64::from(usage.total_tokens)),
            ),
            None => (None, None, None),
        };

        let mut tx = self.conn.begin().await?;
        sqlx::query(
            "insert into session_events (\
                session_id, revision, append_id, object_key, content_hash, byte_length, \
                prompt_tokens, completion_tokens, total_tokens\
             ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(self.session_id)
        .bind(event.expected_revision)
        .bind(event.append_id)
        .bind(&object_key)
        .bind(content_hash.as_slice())
        .bind(byte_length)
        .bind(prompt_tokens)
        .bind(completion_tokens)
        .bind(total_tokens)
        .execute(&mut *tx)
        .await?;
        let updated = sqlx::query(
            "update sessions set head_revision = $1 \
             where id = $2 and head_revision = $3 and writer_generation = $4",
        )
        .bind(event.expected_revision)
        .bind(self.session_id)
        .bind(head_revision)
        .bind(self.writer_generation)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if updated != 1 {
            return Err(StoreError::Fenced);
        }
        tx.commit().await?;
        Ok(event.expected_revision)
    }
}

fn advisory_keys(session_id: Uuid) -> (i32, i32) {
    let bytes = session_id.as_bytes();
    let key1 = i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let key2 = i32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    (key1, key2)
}

fn object_key(session_id: Uuid, revision: i64, content_hash: &[u8; 32]) -> String {
    format!(
        "sessions/{session_id}/events/{revision}-{}.json",
        hex_encode(content_hash)
    )
}

pub(crate) fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn event_ref_from_row(row: sqlx::postgres::PgRow) -> Result<SessionEventRef, StoreError> {
    let hash: Vec<u8> = row.get("content_hash");
    let content_hash: [u8; 32] = hash.try_into().map_err(|_| StoreError::Hash)?;
    Ok(SessionEventRef {
        session_id: row.get("session_id"),
        global_seq: row.get("global_seq"),
        revision: row.get("revision"),
        append_id: row.get("append_id"),
        object_key: row.get("object_key"),
        content_hash,
        byte_length: row.get("byte_length"),
        prompt_tokens: row.get("prompt_tokens"),
        completion_tokens: row.get("completion_tokens"),
        total_tokens: row.get("total_tokens"),
    })
}
