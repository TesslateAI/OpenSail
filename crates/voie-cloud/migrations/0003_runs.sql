-- Unified durable Run resource and normalized backend resource fields.
-- The development estate may rebuild this disposable schema from version 1.

alter table agents
    add column if not exists model text not null default '',
    add column if not exists system_prompt text not null default '',
    add column if not exists tool_ids jsonb not null default '["bash"]'::jsonb,
    add column if not exists max_tokens integer not null default 1024;

alter table agents
    drop constraint if exists agents_max_tokens_check;

alter table agents
    add constraint agents_max_tokens_check
    check (max_tokens between 1 and 1024);

alter table session_events
    add column if not exists global_seq bigint generated always as identity;

create unique index if not exists session_events_global_seq_idx
    on session_events (global_seq);

alter table runs
    add column if not exists intent_id uuid,
    add column if not exists request_hash bytea not null default ''::bytea,
    add column if not exists mode text not null default 'resume',
    add column if not exists prompt text not null default '',
    add column if not exists result text,
    add column if not exists accepted_at timestamptz not null default now(),
    add column if not exists dispatched_at timestamptz,
    add column if not exists cancel_requested_at timestamptz,
    add column if not exists terminal_at timestamptz,
    add column if not exists cancelled_at timestamptz;

update runs
set intent_id = id
where intent_id is null;

alter table runs
    alter column intent_id set not null;

create unique index if not exists runs_intent_id_idx
    on runs (intent_id);

alter table runs
    drop constraint if exists runs_state_check;

alter table runs
    add constraint runs_state_check
    check (state in ('accepted', 'dispatched', 'terminal', 'unknown', 'cancelled'));

alter table runs
    drop constraint if exists runs_mode_check;

alter table runs
    add constraint runs_mode_check
    check (mode in ('create', 'resume'));

alter table exec_calls
    add column if not exists result text;

alter table exec_calls
    drop constraint if exists exec_calls_state_check;

alter table exec_calls
    add constraint exec_calls_state_check
    check (state in ('accepted', 'dispatched', 'terminal', 'unknown', 'cancelled'));

alter table audit_events
    add column if not exists project_id uuid references projects (id),
    add column if not exists session_id uuid references sessions (id),
    add column if not exists run_id uuid references runs (id),
    add column if not exists payload text;
