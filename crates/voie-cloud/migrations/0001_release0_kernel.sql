-- CTL1: Release 0 control-state kernel tables (packet issue #2 frozen set).
-- Smallest columns that carry primary identity, frozen relationships,
-- ownership, and Session writer/attention generations.

create table users (
    id         uuid primary key,
    issuer     text not null,
    subject    text not null,
    created_at timestamptz not null default now(),
    unique (issuer, subject)
);

create table web_sessions (
    id         uuid primary key,
    user_id    uuid not null references users (id),
    token_hash text not null unique,
    created_at timestamptz not null default now()
);

create table projects (
    id            uuid primary key,
    owner_user_id uuid not null references users (id),
    name          text not null,
    created_at    timestamptz not null default now(),
    unique (owner_user_id, name)
);

create table project_members (
    project_id uuid not null references projects (id),
    user_id    uuid not null references users (id),
    role       text not null check (role in ('owner', 'member', 'viewer')),
    created_at timestamptz not null default now(),
    primary key (project_id, user_id)
);

create table fabrics (
    id         uuid primary key,
    name       text not null unique,
    created_at timestamptz not null default now()
);

create table workspaces (
    id         uuid primary key,
    fabric_id  uuid not null references fabrics (id),
    created_at timestamptz not null default now()
);

create table agents (
    id         uuid primary key,
    project_id uuid not null references projects (id),
    name       text not null,
    model      text not null default '',
    system_prompt text not null default '',
    tool_ids   jsonb not null default '["bash"]'::jsonb,
    max_tokens integer not null default 1024 check (max_tokens between 1 and 1024),
    created_at timestamptz not null default now(),
    unique (project_id, name),
    unique (id, project_id)
);

create table sessions (
    id                  uuid primary key,
    project_id          uuid not null references projects (id),
    agent_id            uuid not null,
    workspace_id        uuid not null references workspaces (id),
    writer_generation   bigint not null default 0,
    attention_generation bigint not null default 0,
    head_revision       bigint not null default 0,
    created_at          timestamptz not null default now(),
    foreign key (agent_id, project_id) references agents (id, project_id)
);

create table runs (
    id            uuid primary key,
    intent_id     uuid not null unique,
    session_id    uuid not null references sessions (id),
    request_hash  bytea not null,
    mode          text not null check (mode in ('create', 'resume')),
    prompt        text not null,
    state         text not null check (state in ('accepted', 'dispatched', 'terminal', 'unknown', 'cancelled')),
    result        text,
    accepted_at   timestamptz not null default now(),
    dispatched_at timestamptz,
    cancel_requested_at timestamptz,
    terminal_at   timestamptz,
    cancelled_at  timestamptz
);

create table exec_calls (
    workspace_id uuid not null references workspaces (id),
    call_id      text not null,
    request_hash bytea not null,
    state        text not null check (state in ('accepted', 'dispatched', 'terminal', 'unknown', 'cancelled')),
    result       text,
    created_at   timestamptz not null default now(),
    primary key (workspace_id, call_id)
);

create table audit_events (
    seq         bigint generated always as identity primary key,
    project_id  uuid references projects (id),
    session_id  uuid references sessions (id),
    run_id      uuid references runs (id),
    occurred_at timestamptz not null default now(),
    kind        text not null,
    payload     text
);
