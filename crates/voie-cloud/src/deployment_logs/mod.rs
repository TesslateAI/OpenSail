//! Ordered Deployment log chunk index. Bytes live in Blob.

use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::applications::ApplicationError;
use crate::auth::Action;
use crate::deployments::DeploymentStore;

pub const MAX_LOG_CHUNK_BYTES: i64 = 256 * 1024;
pub const MAX_LOG_CHUNKS_PER_DEPLOYMENT: i64 = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogChunk {
    pub deployment_id: Uuid,
    pub seq: i64,
    pub object_key: String,
    pub content_hash: Vec<u8>,
    pub byte_length: i64,
    pub first_timestamp: String,
    pub last_timestamp: String,
}

pub struct DeploymentLogs {
    pool: PgPool,
}

impl DeploymentLogs {
    pub fn new(pool: PgPool) -> Self {
        DeploymentLogs { pool }
    }

    pub async fn append(
        &self,
        deployment_id: Uuid,
        seq: i64,
        object_key: &str,
        content_hash: &[u8; 32],
        byte_length: i64,
        first_timestamp: &str,
        last_timestamp: &str,
    ) -> Result<(), ApplicationError> {
        if byte_length < 0 || byte_length > MAX_LOG_CHUNK_BYTES {
            return Err(ApplicationError::InvalidName);
        }
        let count: i64 = sqlx::query_scalar(
            "select count(*) from deployment_log_chunks where deployment_id = $1",
        )
        .bind(deployment_id)
        .fetch_one(&self.pool)
        .await?;
        if count >= MAX_LOG_CHUNKS_PER_DEPLOYMENT {
            return Err(ApplicationError::Kernel(crate::KernelError::Quota));
        }
        sqlx::query(
            "insert into deployment_log_chunks \
             (deployment_id, seq, object_key, content_hash, byte_length, first_timestamp, last_timestamp) \
             values ($1, $2, $3, $4, $5, $6::timestamptz, $7::timestamptz)",
        )
        .bind(deployment_id)
        .bind(seq)
        .bind(object_key)
        .bind(content_hash.as_slice())
        .bind(byte_length)
        .bind(first_timestamp)
        .bind(last_timestamp)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn next_seq(&self, deployment_id: Uuid) -> Result<i64, ApplicationError> {
        let seq: i64 = sqlx::query_scalar(
            "select coalesce(max(seq), 0) + 1 from deployment_log_chunks where deployment_id = $1",
        )
        .bind(deployment_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(seq)
    }

    pub async fn list(
        &self,
        actor_user_id: Uuid,
        deployment_id: Uuid,
    ) -> Result<Vec<LogChunk>, ApplicationError> {
        let _ = DeploymentStore::new(self.pool.clone())
            .get(actor_user_id, deployment_id)
            .await?;
        let _ = Action::ReadProject;
        let rows = sqlx::query(
            "select deployment_id, seq, object_key, content_hash, byte_length, \
                    first_timestamp::text as first_timestamp, last_timestamp::text as last_timestamp \
             from deployment_log_chunks where deployment_id = $1 order by seq",
        )
        .bind(deployment_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| LogChunk {
                deployment_id: row.get("deployment_id"),
                seq: row.get("seq"),
                object_key: row.get("object_key"),
                content_hash: row.get("content_hash"),
                byte_length: row.get("byte_length"),
                first_timestamp: row.get("first_timestamp"),
                last_timestamp: row.get("last_timestamp"),
            })
            .collect())
    }
}
