//! Narrow per-activation loop controls: structured product errors, a known
//! blocker cache, honest tool intersection, and a last-resort spend bound.
//!
//! These are not a governor. They do not score progress, hide tools after
//! runtime failures, or chain distinct product actions.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use uuid::Uuid;

use crate::auth::{Action, Role};

use super::{ActivationAbort, ActivationError, ProductResult};

/// Retry class for a structured product error.
pub const RETRY_IMMEDIATE: &str = "immediate";
pub const RETRY_AFTER_CHANGE: &str = "after_change";
pub const RETRY_AFTER_USER: &str = "after_user";
pub const RETRY_NEVER: &str = "never";

/// Generous last-resort bound. Ordinary coding/build/deploy must not hit it.
pub const MAX_MODEL_CALLS: u32 = 96;
pub const MAX_TOTAL_TOKENS: u64 = 4_000_000;

/// Real provider token counts. Absent when the provider omitted usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompletionUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

/// Structured product error returned to the child/model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductError {
    pub code: String,
    pub message: String,
    pub retry: String,
    pub required: Option<String>,
    pub scope: Option<String>,
    pub state: Option<String>,
    pub revision: Option<i64>,
    pub approval_id: Option<String>,
}

impl ProductError {
    pub fn json_text(&self) -> String {
        let mut body = json!({
            "code": self.code,
            "message": self.message,
            "retry": self.retry,
        });
        if let Some(required) = &self.required {
            body["required"] = json!(required);
        }
        if let Some(scope) = &self.scope {
            body["scope"] = json!(scope);
        }
        if let Some(state) = &self.state {
            body["state"] = json!(state);
        }
        if let Some(revision) = self.revision {
            body["revision"] = json!(revision);
        }
        if let Some(approval_id) = &self.approval_id {
            body["approvalId"] = json!(approval_id);
        }
        body.to_string()
    }

    pub fn known_repeat_text(&self) -> String {
        match self.code.as_str() {
            "PERMISSION_DENIED" => format!(
                "KNOWN_BLOCKER: {} is still unavailable for the current actor. Choose another path or wait for authority to change.",
                self.required.as_deref().unwrap_or("required authority")
            ),
            "APPROVAL_REQUIRED" => format!(
                "KNOWN_BLOCKER: approval {} is still required. Choose another allowed path or wait for the user.",
                self.approval_id
                    .as_deref()
                    .unwrap_or(self.scope.as_deref().unwrap_or("pending"))
            ),
            "APPROVAL_REFUSED" => format!(
                "KNOWN_BLOCKER: approval {} was refused for this action. Choose another path or wait for a new user run.",
                self.approval_id
                    .as_deref()
                    .unwrap_or(self.scope.as_deref().unwrap_or("refused"))
            ),
            "RESOURCE_PENDING" => format!(
                "KNOWN_BLOCKER: {} is unchanged (state={}, revision={}). Choose another path or wait for a change.",
                self.scope.as_deref().unwrap_or("resource"),
                self.state.as_deref().unwrap_or("pending"),
                self.revision
                    .map(|n| n.to_string())
                    .as_deref()
                    .unwrap_or("none")
            ),
            "INVALID_ARGUMENT" => {
                "KNOWN_BLOCKER: that exact invalid request is unchanged. Correct the arguments."
                    .to_owned()
            }
            "OUTCOME_UNKNOWN" => format!(
                "KNOWN_BLOCKER: {} settled unknown and will not be retried.",
                self.scope.as_deref().unwrap_or("that effect")
            ),
            _ => format!(
                "KNOWN_BLOCKER: {} is unchanged. Choose another path.",
                self.code
            ),
        }
    }

    pub fn permission_denied(required: Option<&str>, message: impl Into<String>) -> Self {
        Self {
            code: "PERMISSION_DENIED".to_owned(),
            message: message.into(),
            retry: RETRY_AFTER_CHANGE.to_owned(),
            required: required.map(str::to_owned),
            scope: None,
            state: None,
            revision: None,
            approval_id: None,
        }
    }

    pub fn approval_required(id: &str, scope: Option<String>, message: impl Into<String>) -> Self {
        Self {
            code: "APPROVAL_REQUIRED".to_owned(),
            message: message.into(),
            retry: RETRY_AFTER_USER.to_owned(),
            required: None,
            scope,
            state: None,
            revision: None,
            approval_id: Some(id.to_owned()),
        }
    }

    pub fn approval_accepted(id: &str) -> Self {
        Self {
            code: "APPROVAL_ACCEPTED".to_owned(),
            message: format!("Approval {id} is accepted; retry this action with approval_id {id}."),
            retry: RETRY_IMMEDIATE.to_owned(),
            required: Some("approval_id".to_owned()),
            scope: None,
            state: Some("accepted".to_owned()),
            revision: None,
            approval_id: Some(id.to_owned()),
        }
    }

    pub fn approval_refused(id: &str) -> Self {
        Self {
            code: "APPROVAL_REFUSED".to_owned(),
            message: format!(
                "Approval {id} was refused for this action. Do not request another approval in this run."
            ),
            retry: RETRY_AFTER_USER.to_owned(),
            required: None,
            scope: None,
            state: Some("refused".to_owned()),
            revision: None,
            approval_id: Some(id.to_owned()),
        }
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self {
            code: "INVALID_ARGUMENT".to_owned(),
            message: message.into(),
            retry: RETRY_IMMEDIATE.to_owned(),
            required: None,
            scope: None,
            state: None,
            revision: None,
            approval_id: None,
        }
    }

    pub fn resource_pending(
        scope: String,
        state: String,
        revision: Option<i64>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: "RESOURCE_PENDING".to_owned(),
            message: message.into(),
            retry: RETRY_AFTER_CHANGE.to_owned(),
            required: None,
            scope: Some(scope),
            state: Some(state),
            revision,
            approval_id: None,
        }
    }

    pub fn outcome_unknown(message: impl Into<String>) -> Self {
        Self {
            code: "OUTCOME_UNKNOWN".to_owned(),
            message: message.into(),
            retry: RETRY_NEVER.to_owned(),
            required: None,
            scope: None,
            state: None,
            revision: None,
            approval_id: None,
        }
    }

    pub fn outcome_unknown_at(scope: String, message: impl Into<String>) -> Self {
        Self {
            code: "OUTCOME_UNKNOWN".to_owned(),
            message: message.into(),
            retry: RETRY_NEVER.to_owned(),
            required: None,
            scope: Some(scope),
            state: None,
            revision: None,
            approval_id: None,
        }
    }

    pub fn busy(message: impl Into<String>) -> Self {
        Self {
            code: "BUSY".to_owned(),
            message: message.into(),
            retry: RETRY_AFTER_CHANGE.to_owned(),
            required: None,
            scope: None,
            state: None,
            revision: None,
            approval_id: None,
        }
    }

    pub fn failed(
        scope: String,
        state: String,
        revision: Option<i64>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: "FAILED".to_owned(),
            message: message.into(),
            retry: RETRY_AFTER_CHANGE.to_owned(),
            required: None,
            scope: Some(scope),
            state: Some(state),
            revision,
            approval_id: None,
        }
    }

    pub fn log_unavailable(message: impl Into<String>) -> Self {
        Self {
            code: "LOG_UNAVAILABLE".to_owned(),
            message: message.into(),
            retry: RETRY_AFTER_CHANGE.to_owned(),
            required: None,
            scope: None,
            state: None,
            revision: None,
            approval_id: None,
        }
    }

    pub fn to_result(&self) -> ProductResult {
        ProductResult {
            text: self.json_text(),
            is_error: true,
            error: Some(self.clone()),
        }
    }

    pub fn to_known_result(&self) -> ProductResult {
        ProductResult {
            text: self.known_repeat_text(),
            is_error: true,
            error: Some(ProductError {
                code: "KNOWN_BLOCKER".to_owned(),
                message: self.known_repeat_text(),
                retry: self.retry.clone(),
                required: self.required.clone(),
                scope: self.scope.clone(),
                state: self.state.clone(),
                revision: self.revision.clone(),
                approval_id: self.approval_id.clone(),
            }),
        }
    }
}

/// Per-activation cache of proven blockers. Not durable product state.
#[derive(Clone, Default)]
pub struct KnownBlockers {
    inner: std::sync::Arc<Mutex<HashMap<String, ProductError>>>,
    observations: std::sync::Arc<Mutex<HashMap<String, Value>>>,
}

impl KnownBlockers {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn remember(&self, key: String, error: ProductError) {
        self.inner
            .lock()
            .expect("known blocker lock")
            .entry(key)
            .or_insert(error);
    }

    pub fn lookup(&self, key: &str) -> Option<ProductError> {
        self.inner
            .lock()
            .expect("known blocker lock")
            .get(key)
            .cloned()
    }

    pub fn forget(&self, keys: &[String]) {
        let mut inner = self.inner.lock().expect("known blocker lock");
        for key in keys {
            inner.remove(key);
        }
    }
}

/// Accumulated real provider usage plus a call-count fuse.
#[derive(Default)]
pub struct RunBudget {
    prompt_tokens: AtomicU64,
    completion_tokens: AtomicU64,
    model_calls: AtomicU32,
}

impl RunBudget {
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::default())
    }

    pub fn record(&self, prompt: Option<u32>, completion: Option<u32>) {
        self.model_calls.fetch_add(1, Ordering::Relaxed);
        if let Some(n) = prompt {
            self.prompt_tokens
                .fetch_add(u64::from(n), Ordering::Relaxed);
        }
        if let Some(n) = completion {
            self.completion_tokens
                .fetch_add(u64::from(n), Ordering::Relaxed);
        }
    }

    pub fn exhausted(&self) -> bool {
        let calls = self.model_calls.load(Ordering::Relaxed);
        if calls >= MAX_MODEL_CALLS {
            return true;
        }
        let total = self.prompt_tokens.load(Ordering::Relaxed)
            + self.completion_tokens.load(Ordering::Relaxed);
        total >= MAX_TOTAL_TOKENS
    }

    pub fn bound_text() -> &'static str {
        "This run reached the platform model bound. Durable effects already committed remain. Report the current result; do not retry the same work."
    }
}

/// Action a tool requires as an entire tool, independent of arguments.
pub fn unconditional_tool_action(name: &str) -> Option<Action> {
    match name {
        "application.suspend" | "environment.publish_prod" => Some(Action::ManageProduction),
        "workspace.snapshot" | "workspace.restore" => Some(Action::ManageProduction),
        "database.backup" | "database.restore" => Some(Action::ManageProduction),
        "application.delete" => Some(Action::DestroyApplication),
        "application.archive" | "application.restore" => Some(Action::ManageProduction),
        _ => None,
    }
}

pub fn capability_snapshot(role: Role) -> String {
    format!(
        "Current project capabilities:\n- development operations: {}\n- production management: {}\n- destructive operations: {}",
        yn(role.permits(Action::DeployDev)),
        yn(role.permits(Action::ManageProduction)),
        yn(role.permits(Action::DestroyApplication)),
    )
}

fn yn(ok: bool) -> &'static str {
    if ok { "yes" } else { "no" }
}

/// Child-visible names intersect server-owned definitions. Child cannot broaden.
pub fn intersect_tools(
    child_names: &[String],
    server: Vec<crate::model::ModelToolDefinition>,
) -> Vec<crate::model::ModelToolDefinition> {
    let allowed: std::collections::HashSet<&str> = child_names.iter().map(String::as_str).collect();
    server
        .into_iter()
        .filter(|tool| allowed.contains(tool.name.as_str()))
        .collect()
}

/// Hide only tools that are impossible as an entire tool for this role.
pub fn filter_tools_for_role(
    role: Role,
    tools: Vec<crate::model::ModelToolDefinition>,
) -> Vec<crate::model::ModelToolDefinition> {
    tools
        .into_iter()
        .filter(|tool| match unconditional_tool_action(&tool.name) {
            Some(action) => role.permits(action),
            None => {
                if role == Role::Viewer {
                    !tool.name.contains("create")
                        && !tool.name.contains("delete")
                        && !tool.name.contains("deploy")
                        && !tool.name.contains("publish")
                        && !tool.name.contains("activate")
                        && !tool.name.contains("restore")
                        && !tool.name.contains("suspend")
                        && !tool.name.contains("archive")
                        && !tool.name.contains("build")
                        && !tool.name.contains("stop")
                        && !tool.name.contains("restart")
                        && !tool.name.contains("rollback")
                        && !tool.name.contains("backup")
                        && !tool.name.contains("grow")
                        && !tool.name.contains("request_binding")
                        && !tool.name.contains("set_")
                } else {
                    true
                }
            }
        })
        .collect()
}

pub fn authority_key(required: &str) -> String {
    format!("auth:{required}")
}

pub fn approval_key(approval_id: &str) -> String {
    format!("approval:{approval_id}")
}

pub fn resource_key(kind: &str, id: Uuid, state: &str, revision: i64) -> String {
    format!("obs:{kind}:{id}:{state}:{revision}")
}

pub fn invalid_key(tool: &str, arguments: &Value) -> String {
    format!("invalid:{tool}:{}", compact_args(arguments))
}

pub fn unknown_effect_key(scope: &str) -> String {
    format!("unknown:{scope}")
}

pub fn approval_op_key(tool: &str, arguments: &Value) -> String {
    format!(
        "approval-op:{tool}:{}",
        compact_args_without_ephemeral(arguments)
    )
}

/// True for diagnostic tools that must still query durable state.
pub fn is_observation_tool(name: &str) -> bool {
    matches!(
        name,
        "application.status"
            | "application.inspect"
            | "deployment.status"
            | "deployment.logs"
            | "database.status"
            | "database.list_backups"
            | "release.inspect"
            | "secret.list_metadata"
    )
}

/// Skip an expensive effect when this activation already proved the same fact.
pub fn precheck_blocker(
    blockers: &KnownBlockers,
    name: &str,
    arguments: &Value,
) -> Option<ProductResult> {
    lookup_blocker(blockers, name, arguments).map(|error| error.to_known_result())
}

pub fn lookup_blocker(
    blockers: &KnownBlockers,
    name: &str,
    arguments: &Value,
) -> Option<ProductError> {
    if is_observation_tool(name) {
        return None;
    }
    for key in call_blocker_keys(name, arguments) {
        if let Some(error) = blockers.lookup(&key) {
            return Some(error);
        }
    }
    None
}

pub fn forget_blocker(
    blockers: &KnownBlockers,
    name: &str,
    arguments: &Value,
    error: &ProductError,
) {
    let mut keys = call_blocker_keys(name, arguments);
    keys.extend(error_blocker_keys(name, arguments, error));
    blockers.forget(&keys);
}

/// Drop a previous blocker for this call, then store the replacement.
/// Used when a pending approval becomes refused so the cache holds the
/// proven refusal instead of the stale pending row.
pub fn replace_error(
    blockers: &KnownBlockers,
    name: &str,
    arguments: &Value,
    previous: &ProductError,
    error: &ProductError,
) {
    forget_blocker(blockers, name, arguments, previous);
    remember_error(blockers, name, arguments, error);
}

pub fn remember_error(
    blockers: &KnownBlockers,
    name: &str,
    arguments: &Value,
    error: &ProductError,
) {
    for key in error_blocker_keys(name, arguments, error) {
        blockers.remember(key, error.clone());
    }
}

/// When the model omitted `release_id`, copy the Release from the accepted
/// approval row so a later explicit `{release_id}` hits the same cache key.
pub fn arguments_with_release_id(arguments: &Value, release_id: Uuid) -> Option<Value> {
    let Value::Object(map) = arguments else {
        return Some(json!({ "release_id": release_id }));
    };
    let supplied = map
        .get("release_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty());
    if supplied.is_some() {
        return None;
    }
    let mut filled = map.clone();
    filled.insert("release_id".to_owned(), json!(release_id.to_string()));
    Some(Value::Object(filled))
}

/// After a fresh observation, remember its fingerprint. A later identical
/// observation is a successful `{unchanged:true}` result.
pub fn remember_or_repeat_observation(
    blockers: &KnownBlockers,
    name: &str,
    value: &Value,
) -> Option<ProductResult> {
    let key = observation_key_for(name, value)?;
    let mut observations = blockers.observations.lock().expect("observation lock");
    if observations.get(&key) == Some(value) {
        drop(observations);
        return Some(unchanged_result(name, value));
    }
    observations.insert(key, value.clone());
    None
}

fn call_blocker_keys(name: &str, arguments: &Value) -> Vec<String> {
    let mut keys = Vec::new();
    keys.push(denied_op_key(name, arguments));
    if let Some(id) = arguments.get("approval_id").and_then(Value::as_str) {
        if !id.is_empty() {
            keys.push(approval_key(id));
        }
    }
    keys.push(approval_op_key(name, arguments));
    keys.push(invalid_key(name, arguments));
    keys.extend(argument_unknown_keys(arguments));
    keys
}

fn error_blocker_keys(name: &str, arguments: &Value, error: &ProductError) -> Vec<String> {
    match error.code.as_str() {
        "PERMISSION_DENIED" => vec![denied_op_key(name, arguments)],
        "APPROVAL_REQUIRED" => {
            let mut keys = vec![approval_op_key(name, arguments)];
            if let Some(id) = &error.approval_id {
                keys.push(approval_key(id));
            }
            keys
        }
        "APPROVAL_REFUSED" => vec![approval_op_key(name, arguments)],
        "INVALID_ARGUMENT" => vec![invalid_key(name, arguments)],
        "OUTCOME_UNKNOWN" => unknown_error_keys(error),
        _ => Vec::new(),
    }
}

fn unknown_error_keys(error: &ProductError) -> Vec<String> {
    let Some(scope) = error.scope.as_deref().filter(|scope| !scope.is_empty()) else {
        return Vec::new();
    };
    let mut keys = vec![unknown_effect_key(scope)];
    if let Some((_, intent)) = scope.rsplit_once("/intent:") {
        if !intent.is_empty() {
            keys.push(unknown_effect_key(&format!("intent:{intent}")));
        }
    }
    keys
}

fn argument_unknown_keys(arguments: &Value) -> Vec<String> {
    ["build_intent_id", "deployment_intent_id", "operation_id"]
        .into_iter()
        .filter_map(|field| arguments.get(field).and_then(Value::as_str))
        .filter(|id| !id.is_empty())
        .map(|id| unknown_effect_key(&format!("intent:{id}")))
        .collect()
}

fn denied_op_key(tool: &str, arguments: &Value) -> String {
    format!(
        "denied-op:{tool}:{}",
        compact_args_without_ephemeral(arguments)
    )
}

fn observation_key_for(name: &str, value: &Value) -> Option<String> {
    match name {
        "deployment.status" => resource_observation_key("deployment", value.get("deployment")?),
        "database.status" => resource_observation_key("database", value.get("database")?),
        "release.inspect" => resource_observation_key("release", value.get("release")?),
        "deployment.logs" => {
            let id = value
                .get("deploymentId")
                .and_then(Value::as_str)
                .and_then(|text| Uuid::parse_str(text).ok())?;
            if value.get("unavailable").and_then(Value::as_bool) == Some(true) {
                return Some(resource_key("logs", id, "sensitive", 0));
            }
            let last = value.get("lastSeq").and_then(Value::as_i64).unwrap_or(0);
            Some(resource_key("logs", id, "tail", last))
        }
        "application.status" => {
            let id = value
                .get("application")
                .and_then(|item| item.get("id"))
                .and_then(Value::as_str)
                .and_then(|text| Uuid::parse_str(text).ok())?;
            Some(format!("obs:application:{id}:{}", compact_args(value)))
        }
        _ => None,
    }
}

fn unchanged_result(name: &str, value: &Value) -> ProductResult {
    let mut body = match name {
        "deployment.status" => json!({
            "unchanged": true,
            "deployment": value.get("deployment"),
        }),
        "database.status" => json!({
            "unchanged": true,
            "database": value.get("database"),
        }),
        "release.inspect" => json!({
            "unchanged": true,
            "release": value.get("release"),
        }),
        "deployment.logs" if value.get("unavailable").and_then(Value::as_bool) == Some(true) => {
            json!({
                "unchanged": true,
                "deploymentId": value.get("deploymentId"),
                "unavailable": true,
                "reason": value.get("reason"),
                "truncated": value.get("truncated"),
            })
        }
        "deployment.logs" => json!({
            "unchanged": true,
            "deploymentId": value.get("deploymentId"),
            "lastSeq": value.get("lastSeq"),
            "nextSeq": value.get("nextSeq"),
        }),
        "application.status" => json!({
            "unchanged": true,
            "application": value.get("application"),
            "environmentViews": value.get("environmentViews"),
            "latestRelease": value.get("latestRelease"),
            "approvals": value.get("approvals"),
        }),
        _ => json!({ "unchanged": true }),
    };
    if let Value::Object(map) = &mut body {
        map.retain(|_, item| !item.is_null());
    }
    ProductResult::ok(body.to_string())
}

fn resource_observation_key(kind: &str, object: &Value) -> Option<String> {
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .and_then(|text| Uuid::parse_str(text).ok())?;
    let state = object
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let revision = object
        .get("desiredRevision")
        .and_then(Value::as_i64)
        .or_else(|| object.get("observedRevision").and_then(Value::as_i64))
        .unwrap_or(0);
    Some(resource_key(kind, id, state, revision))
}

pub enum WaitTick {
    Continue,
    Done,
}

/// Bounded abort-aware poll with simple backoff. `Ok(true)` settled; `Ok(false)` still pending.
pub async fn wait_until<F, Fut>(
    abort: Option<&ActivationAbort>,
    bound: Duration,
    interval: Duration,
    mut tick: F,
) -> Result<bool, ActivationError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<WaitTick, ActivationError>>,
{
    let deadline = Instant::now() + bound;
    let max_delay = interval.max(Duration::from_secs(2));
    let mut delay = interval;
    loop {
        if abort.is_some_and(|item| item.is_signaled()) {
            return Err(ActivationError::Cancelled);
        }
        match tick().await? {
            WaitTick::Done => return Ok(true),
            WaitTick::Continue => {}
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        let sleep = tokio::time::sleep(delay);
        if let Some(abort) = abort {
            let mut abort = abort.clone();
            tokio::select! {
                biased;
                _ = abort.wait() => return Err(ActivationError::Cancelled),
                _ = sleep => {}
            }
        } else {
            sleep.await;
        }
        delay = delay.saturating_mul(2).min(max_delay);
    }
}

fn compact_args(arguments: &Value) -> String {
    match arguments {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let body: Vec<String> = keys
                .into_iter()
                .filter_map(|key| map.get(key).map(|value| format!("{key}:{value}")))
                .collect();
            body.join(",")
        }
        other => other.to_string(),
    }
}

fn is_ephemeral_arg(key: &str) -> bool {
    matches!(
        key,
        "approval_id" | "build_intent_id" | "deployment_intent_id" | "operation_id"
    )
}

fn compact_args_without_ephemeral(arguments: &Value) -> String {
    match arguments {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let body: Vec<String> = keys
                .into_iter()
                .filter(|key| !is_ephemeral_arg(key.as_str()))
                .filter_map(|key| map.get(key).map(|value| format!("{key}:{value}")))
                .collect();
            body.join(",")
        }
        other => other.to_string(),
    }
}

pub const WAIT_POLL: Duration = Duration::from_millis(250);
pub const WAIT_RELEASE: Duration = Duration::from_secs(600);
pub const WAIT_DEPLOY: Duration = Duration::from_secs(180);
pub const WAIT_DATABASE: Duration = Duration::from_secs(180);
pub const WAIT_ACTIVATE: Duration = Duration::from_secs(90);

/// Generic unusable-completion retry. No product-workflow assumptions.
pub const UNUSABLE_COMPLETION_RETRY: &str = "The previous completion was unusable. Continue the current task with at most one tool call, or return a final response.";

pub fn is_cancelled_error(error: &ActivationError) -> bool {
    matches!(error, ActivationError::Cancelled)
        || matches!(error, ActivationError::Protocol("run was cancelled"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ModelToolDefinition;

    fn tool(name: &str) -> ModelToolDefinition {
        ModelToolDefinition {
            id: name.to_owned(),
            name: name.to_owned(),
            description: String::new(),
            parameters: json!({ "type": "object" }),
        }
    }

    #[test]
    fn member_keeps_conditional_database_create_and_loses_publish_prod() {
        let tools = vec![
            tool("database.create"),
            tool("environment.publish_prod"),
            tool("environment.deploy_dev"),
            tool("application.delete"),
            tool("application.suspend"),
            tool("database.set_security_profile"),
        ];
        let filtered = filter_tools_for_role(Role::Member, tools);
        let names: Vec<_> = filtered.iter().map(|tool| tool.name.as_str()).collect();
        assert!(names.contains(&"database.create"));
        assert!(names.contains(&"environment.deploy_dev"));
        assert!(names.contains(&"database.set_security_profile"));
        assert!(!names.contains(&"environment.publish_prod"));
        assert!(!names.contains(&"application.delete"));
        assert!(!names.contains(&"application.suspend"));
    }

    #[test]
    fn child_cannot_broaden_intersected_tools() {
        let server = vec![tool("environment.deploy_dev"), tool("bash")];
        let child = vec![
            "environment.deploy_dev".to_owned(),
            "secret.explode".to_owned(),
        ];
        let names: Vec<_> = intersect_tools(&child, server)
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert_eq!(names, vec!["environment.deploy_dev"]);
    }

    #[test]
    fn child_omission_drops_server_tool() {
        let server = vec![tool("environment.deploy_dev"), tool("application.status")];
        let child = vec!["environment.deploy_dev".to_owned()];
        let names: Vec<_> = intersect_tools(&child, server)
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert_eq!(names, vec!["environment.deploy_dev"]);
    }

    #[test]
    fn second_lookup_returns_the_stored_blocker() {
        let cache = KnownBlockers::new();
        let error = ProductError {
            code: "PERMISSION_DENIED".to_owned(),
            message: "denied".to_owned(),
            retry: RETRY_AFTER_CHANGE.to_owned(),
            required: Some("ManageProduction".to_owned()),
            scope: None,
            state: None,
            revision: None,
            approval_id: None,
        };
        assert!(cache.lookup("auth:ManageProduction").is_none());
        cache.remember(authority_key("ManageProduction"), error.clone());
        let again = cache.lookup("auth:ManageProduction").expect("stored");
        assert_eq!(again.code, "PERMISSION_DENIED");
        assert!(again.to_known_result().text.contains("KNOWN_BLOCKER"));
    }

    #[test]
    fn precheck_skips_the_same_denied_operation_only() {
        let cache = KnownBlockers::new();
        let denied = ProductError::permission_denied(Some("ManageProduction"), "denied");
        remember_error(&cache, "application.suspend", &json!({}), &denied);
        let repeat = precheck_blocker(&cache, "application.suspend", &json!({})).expect("known");
        assert!(repeat.text.contains("KNOWN_BLOCKER"));
        assert!(
            precheck_blocker(&cache, "environment.deploy_dev", &json!({})).is_none(),
            "a ManageProduction denial must not poison DeployDev"
        );
        assert!(precheck_blocker(&cache, "database.create", &json!({ "kind": "dev" })).is_none());
        remember_error(
            &cache,
            "environment.publish_prod",
            &json!({}),
            &ProductError::permission_denied(Some("ManageProduction"), "denied"),
        );
        assert!(precheck_blocker(&cache, "environment.publish_prod", &json!({})).is_some());
        assert!(
            precheck_blocker(&cache, "database.create", &json!({ "kind": "prod" })).is_none(),
            "cross-tool permission cache requires the same denied operation"
        );
    }

    #[test]
    fn accepted_approval_can_drop_the_cached_blocker() {
        let cache = KnownBlockers::new();
        let error = ProductError::approval_required(
            "11111111-1111-1111-1111-111111111111",
            Some("publish".to_owned()),
            "approval required",
        );
        let args = json!({});
        remember_error(&cache, "environment.publish_prod", &args, &error);
        assert!(precheck_blocker(&cache, "environment.publish_prod", &args).is_some());
        forget_blocker(&cache, "environment.publish_prod", &args, &error);
        assert!(precheck_blocker(&cache, "environment.publish_prod", &args).is_none());
    }

    #[test]
    fn refused_approval_is_remembered_for_the_semantic_action() {
        let cache = KnownBlockers::new();
        let error = ProductError::approval_refused("11111111-1111-1111-1111-111111111111");
        remember_error(&cache, "environment.publish_prod", &json!({}), &error);
        assert!(precheck_blocker(&cache, "environment.publish_prod", &json!({})).is_some());
        assert!(
            precheck_blocker(
                &cache,
                "environment.publish_prod",
                &json!({ "approval_id": "22222222-2222-2222-2222-222222222222" })
            )
            .is_some(),
            "ephemeral approval_id must not bypass a refused semantic action"
        );
        assert!(precheck_blocker(&cache, "environment.deploy_dev", &json!({})).is_none());
    }

    #[test]
    fn refused_approval_replaces_the_pending_cache_entry() {
        let cache = KnownBlockers::new();
        let pending = ProductError::approval_required(
            "11111111-1111-1111-1111-111111111111",
            Some("publish".to_owned()),
            "approval required",
        );
        let refused = ProductError::approval_refused("11111111-1111-1111-1111-111111111111");
        let args = json!({});
        remember_error(&cache, "environment.publish_prod", &args, &pending);
        replace_error(
            &cache,
            "environment.publish_prod",
            &args,
            &pending,
            &refused,
        );
        let found = lookup_blocker(&cache, "environment.publish_prod", &args).expect("stored");
        assert_eq!(found.code, "APPROVAL_REFUSED");
    }

    #[test]
    fn omitted_release_id_shares_the_approval_cache_key_once_filled() {
        let cache = KnownBlockers::new();
        let release = uuid::Uuid::from_u128(0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa);
        let refused = ProductError::approval_refused("11111111-1111-1111-1111-111111111111");
        let omitted = json!({});
        remember_error(&cache, "environment.publish_prod", &omitted, &refused);
        let filled = arguments_with_release_id(&omitted, release).expect("fill");
        remember_error(&cache, "environment.publish_prod", &filled, &refused);
        assert!(
            lookup_blocker(
                &cache,
                "environment.publish_prod",
                &json!({ "release_id": release.to_string() })
            )
            .is_some()
        );
        assert!(arguments_with_release_id(&filled, release).is_none());
    }

    #[test]
    fn unknown_outcome_is_keyed_by_intent_not_omitted_args() {
        let cache = KnownBlockers::new();
        let intent = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
        let error = ProductError::outcome_unknown_at(
            format!("release:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb/intent:{intent}"),
            "unknown",
        );
        remember_error(&cache, "release.build", &json!({}), &error);
        assert!(
            precheck_blocker(&cache, "release.build", &json!({})).is_none(),
            "omitted build_intent_id must not cheap-refuse a later anonymous build"
        );
        assert!(
            precheck_blocker(
                &cache,
                "release.build",
                &json!({ "build_intent_id": intent })
            )
            .is_some()
        );
        assert!(
            precheck_blocker(
                &cache,
                "release.build",
                &json!({ "build_intent_id": "cccccccc-cccc-cccc-cccc-cccccccccccc" })
            )
            .is_none()
        );
    }

    #[test]
    fn permission_and_approval_ignore_ephemeral_intent_ids() {
        let cache = KnownBlockers::new();
        let denied = ProductError::permission_denied(Some("ManageProduction"), "denied");
        remember_error(
            &cache,
            "database.create",
            &json!({
                "kind": "prod",
                "operation_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
            }),
            &denied,
        );
        assert!(
            precheck_blocker(
                &cache,
                "database.create",
                &json!({
                    "kind": "prod",
                    "operation_id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"
                })
            )
            .is_some()
        );
        assert!(precheck_blocker(&cache, "database.create", &json!({ "kind": "dev" })).is_none());
        let approval = ProductError::approval_required(
            "11111111-1111-1111-1111-111111111111",
            Some("publish".to_owned()),
            "approval required",
        );
        remember_error(
            &cache,
            "environment.publish_prod",
            &json!({
                "deployment_intent_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
            }),
            &approval,
        );
        assert!(
            precheck_blocker(
                &cache,
                "environment.publish_prod",
                &json!({
                    "deployment_intent_id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"
                })
            )
            .is_some()
        );
    }

    #[test]
    fn unchanged_observation_is_a_successful_result() {
        let cache = KnownBlockers::new();
        let value = json!({
            "deployment": {
                "id": "22222222-2222-2222-2222-222222222222",
                "state": "creating",
                "desiredRevision": 7
            }
        });
        assert!(remember_or_repeat_observation(&cache, "deployment.status", &value).is_none());
        let repeat =
            remember_or_repeat_observation(&cache, "deployment.status", &value).expect("unchanged");
        assert!(!repeat.is_error);
        assert!(
            repeat.text.contains("\"unchanged\":true"),
            "{}",
            repeat.text
        );
        assert!(repeat.text.contains("creating"), "{}", repeat.text);
    }

    #[test]
    fn sensitive_logs_unchanged_keeps_the_unavailability_reason() {
        let cache = KnownBlockers::new();
        let value = json!({
            "deploymentId": "22222222-2222-2222-2222-222222222222",
            "unavailable": true,
            "reason": "sensitive",
            "truncated": true
        });
        assert!(remember_or_repeat_observation(&cache, "deployment.logs", &value).is_none());
        let repeat =
            remember_or_repeat_observation(&cache, "deployment.logs", &value).expect("unchanged");
        assert!(!repeat.is_error);
        assert!(
            repeat.text.contains("\"unchanged\":true"),
            "{}",
            repeat.text
        );
        assert!(
            repeat.text.contains("\"unavailable\":true"),
            "{}",
            repeat.text
        );
        assert!(repeat.text.contains("sensitive"), "{}", repeat.text);
    }

    #[tokio::test]
    async fn wait_until_exits_on_abort() {
        let aborts = crate::activation::LiveActivationAborts::new();
        let run_id = Uuid::new_v4();
        let abort = aborts.register(run_id);
        let pending = aborts.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            pending.abort(run_id);
        });
        let err = wait_until(
            Some(&abort),
            Duration::from_secs(5),
            Duration::from_millis(5),
            || async { Ok(WaitTick::Continue) },
        )
        .await
        .expect_err("abort wins");
        assert!(matches!(err, ActivationError::Cancelled));
    }
}
