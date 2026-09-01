alter table workspace_snapshot_operations
    add column if not exists purpose text not null default 'manual';
alter table workspace_snapshot_operations
    add column if not exists archive_generation bigint;
alter table workspace_snapshot_operations
    drop constraint if exists workspace_snapshot_operations_purpose_check;
alter table workspace_snapshot_operations
    add constraint workspace_snapshot_operations_purpose_check
    check (purpose in ('manual', 'archive'));
alter table workspace_snapshot_operations
    drop constraint if exists workspace_snapshot_operations_generation_check;
alter table workspace_snapshot_operations
    add constraint workspace_snapshot_operations_generation_check
    check (
        (purpose = 'manual' and archive_generation is null)
        or (purpose = 'archive' and archive_generation is not null)
    );

create table if not exists workspace_grow_operations (
    workspace_id uuid not null references workspaces (id),
    operation_id uuid not null,
    target_bytes bigint not null,
    state        text not null,
    created_at   timestamptz not null default now(),
    primary key (workspace_id, operation_id)
);

create unique index if not exists workspace_grow_operations_dispatched_idx
    on workspace_grow_operations (workspace_id)
    where state = 'dispatched';
