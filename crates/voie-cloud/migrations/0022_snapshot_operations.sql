create table workspace_snapshot_operations (
    workspace_id uuid not null references workspaces (id),
    operation_id uuid not null,
    state        text not null,
    created_at   timestamptz not null default now(),
    primary key (workspace_id, operation_id),
    check (state in ('dispatched', 'ready', 'unknown'))
);

create unique index workspace_snapshot_operations_dispatched_idx
    on workspace_snapshot_operations (workspace_id)
    where state = 'dispatched';
