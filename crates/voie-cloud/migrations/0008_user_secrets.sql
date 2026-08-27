-- User-secret metadata only. Secret material remains in the configured backend
-- (Azure Key Vault or local encrypted development storage).
-- scope_id is the project id used for fixed project-membership authorization.
create table user_secrets (
    id         uuid primary key,
    scope_id   uuid not null references projects (id),
    name       text not null check (length(trim(name)) between 1 and 128),
    kv_name    text not null unique,
    version    bigint not null default 0 check (version >= 0),
    created_by uuid not null references users (id),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (scope_id, name)
);

create index user_secrets_scope_id_idx on user_secrets (scope_id);
