-- Profile 1 Application platform. Project remains the authorization scope.
-- Application is the agent-created deployable, distinct from `projects`.

create table applications (
    id                  uuid primary key,
    project_id          uuid not null references projects (id),
    workspace_id        uuid not null references workspaces (id),
    name                text not null,
    slug                text not null unique,
    root_path           text not null default '.',
    runtime_profile     text not null,
    state               text not null,
    created_by_user_id  uuid not null references users (id),
    created_at          timestamptz not null default now(),
    updated_at          timestamptz not null default now(),
    unique (workspace_id),
    check (state in ('creating', 'ready', 'suspended', 'deleting')),
    check (runtime_profile in ('universal-v1')),
    check (root_path = '.' or root_path ~ '^[A-Za-z0-9][A-Za-z0-9._/-]{0,126}$'),
    check (char_length(name) between 1 and 80)
);

create index applications_project_idx on applications (project_id);

create table application_environments (
    id                    uuid primary key,
    application_id        uuid not null references applications (id),
    kind                  text not null,
    visibility            text not null,
    hostname              text not null unique,
    revision              bigint not null default 0,
    active_deployment_id  uuid,
    state                 text not null,
    unique (application_id, kind),
    check (kind in ('dev', 'prod')),
    check (visibility in ('private', 'public')),
    check (state in ('ready', 'updating', 'suspended')),
    check (revision >= 0)
);

create table application_releases (
    id                       uuid primary key,
    application_id           uuid not null references applications (id),
    build_intent_id          uuid not null unique,
    request_hash             bytea not null,
    source_workspace_id      uuid not null references workspaces (id),
    source_exec_generation   bigint not null,
    runtime_profile          text not null,
    manifest                 jsonb not null,
    manifest_hash            bytea not null,
    artifact_key             text,
    artifact_hash            bytea,
    artifact_bytes           bigint,
    test_summary             text,
    state                    text not null,
    created_by_user_id       uuid not null references users (id),
    created_at               timestamptz not null default now(),
    unique (application_id, request_hash),
    check (state in ('reserved', 'dispatched', 'ready', 'failed', 'unknown')),
    check (source_exec_generation >= 0)
);

create index application_releases_app_idx on application_releases (application_id, created_at);

create table application_deployments (
    id                       uuid primary key,
    environment_id           uuid not null references application_environments (id),
    release_id               uuid not null references application_releases (id),
    deployment_intent_id     uuid not null unique,
    request_hash             bytea not null,
    state                    text not null,
    desired_revision         bigint not null,
    observed_revision        bigint not null default 0,
    previous_deployment_id   uuid references application_deployments (id),
    created_by_user_id       uuid not null references users (id),
    accepted_at              timestamptz not null default now(),
    dispatched_at            timestamptz,
    active_at                timestamptz,
    terminal_at              timestamptz,
    unique (environment_id, request_hash),
    check (state in (
        'accepted', 'materializing', 'starting', 'healthy', 'activating',
        'active', 'failed', 'unknown', 'superseded', 'stopped'
    )),
    check (desired_revision >= 0),
    check (observed_revision >= 0)
);

create index application_deployments_env_idx on application_deployments (environment_id, accepted_at);

alter table application_environments
    add constraint application_environments_active_deployment_fk
    foreign key (active_deployment_id) references application_deployments (id);

create table application_databases (
    id                    uuid primary key,
    application_id        uuid not null references applications (id),
    environment_id        uuid not null references application_environments (id) unique,
    engine                text not null default 'postgres',
    engine_profile        text not null,
    fabric_id             uuid not null references fabrics (id),
    state                 text not null,
    desired_revision      bigint not null default 0,
    observed_revision     bigint not null default 0,
    credential_secret_id  uuid references user_secrets (id),
    storage_bytes         bigint not null default 0,
    created_at            timestamptz not null default now(),
    deleted_at            timestamptz,
    check (engine = 'postgres'),
    check (engine_profile in ('voie-postgres:v1')),
    check (state in (
        'creating', 'ready', 'unknown', 'failed',
        'backing_up', 'restoring', 'deleting', 'deleted'
    )),
    check (desired_revision >= 0),
    check (observed_revision >= 0),
    check (storage_bytes >= 0)
);

create table database_operations (
    id              uuid primary key,
    database_id     uuid not null references application_databases (id),
    release_id      uuid references application_releases (id),
    operation_id    uuid not null,
    kind            text not null,
    request_hash    bytea not null,
    state           text not null,
    created_at      timestamptz not null default now(),
    unique (database_id, operation_id),
    check (kind in ('create', 'backup', 'restore', 'migrate', 'delete')),
    check (state in ('reserved', 'dispatched', 'ready', 'failed', 'unknown'))
);

create unique index database_operations_migrate_idx
    on database_operations (database_id, release_id, operation_id)
    where kind = 'migrate';

create table database_backups (
    id              uuid primary key,
    database_id     uuid not null references application_databases (id),
    object_key      text not null,
    content_hash    bytea not null,
    byte_length     bigint not null,
    kind            text not null,
    created_at      timestamptz not null default now(),
    check (kind in ('manual', 'pre_migration', 'daily')),
    check (byte_length >= 0)
);

create table environment_secret_bindings (
    environment_id     uuid not null references application_environments (id),
    secret_id          uuid not null references user_secrets (id),
    environment_name   text not null,
    binding_revision   bigint not null default 0,
    created_at         timestamptz not null default now(),
    primary key (environment_id, environment_name),
    check (char_length(environment_name) between 1 and 128),
    check (binding_revision >= 0)
);

create table approval_requests (
    id                 uuid primary key,
    project_id         uuid not null references projects (id),
    application_id     uuid references applications (id),
    environment_id     uuid references application_environments (id),
    release_id         uuid references application_releases (id),
    kind               text not null,
    action_hash        bytea not null,
    state              text not null,
    requested_by       uuid not null references users (id),
    accepted_by        uuid references users (id),
    accepted_event_id  uuid,
    created_at         timestamptz not null default now(),
    resolved_at        timestamptz,
    check (kind in (
        'publish_production',
        'make_environment_public',
        'bind_production_secret',
        'restore_database',
        'delete_database',
        'delete_application',
        'increase_resource_tier'
    )),
    check (state in ('pending', 'accepted', 'refused'))
);

create table deployment_log_chunks (
    deployment_id   uuid not null references application_deployments (id),
    seq             bigint not null,
    object_key      text not null,
    content_hash    bytea not null,
    byte_length     bigint not null,
    first_timestamp timestamptz not null,
    last_timestamp  timestamptz not null,
    primary key (deployment_id, seq),
    check (seq >= 0),
    check (byte_length >= 0)
);

create table preview_codes (
    code            text primary key,
    user_id         uuid not null references users (id),
    application_id  uuid not null references applications (id),
    environment_id  uuid not null references application_environments (id),
    hostname        text not null,
    expires_at      timestamptz not null,
    consumed_at     timestamptz
);

create table preview_sessions (
    id              uuid primary key,
    user_id         uuid not null references users (id),
    application_id  uuid not null references applications (id),
    environment_id  uuid not null references application_environments (id),
    hostname        text not null,
    token_hash      bytea not null unique,
    expires_at      timestamptz not null,
    created_at      timestamptz not null default now()
);
