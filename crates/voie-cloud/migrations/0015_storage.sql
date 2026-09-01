-- Platform storage accounting, restore-point retention, and archive.

alter table workspaces
    add column allocated_bytes bigint not null default 34359738368,
    add column storage_tier text not null default 'default';

alter table workspaces
    add constraint workspaces_storage_tier_check
        check (storage_tier in ('default', 'elevated')),
    add constraint workspaces_allocated_bytes_check
        check (allocated_bytes > 0);

update application_databases
    set storage_bytes = case
        when environment_id in (
            select id from application_environments where kind = 'prod'
        ) then 17179869184
        else 8589934592
    end
    where storage_bytes = 0;

alter table application_databases
    add column storage_tier text not null default 'default';

alter table application_databases
    add constraint application_databases_storage_tier_check
        check (storage_tier in ('default', 'elevated'));

alter table applications drop constraint if exists applications_state_check;
alter table applications
    add constraint applications_state_check
        check (state in ('creating', 'ready', 'suspended', 'archived', 'deleting'));

alter table application_databases drop constraint if exists application_databases_state_check;
alter table application_databases
    add constraint application_databases_state_check
        check (state in (
            'creating', 'ready', 'unknown', 'failed',
            'backing_up', 'restoring', 'archived',
            'deleting', 'deleted'
        ));

create table workspace_snapshots (
    id              uuid primary key,
    workspace_id    uuid not null references workspaces (id),
    object_key      text not null,
    content_hash    bytea not null,
    byte_length     bigint not null,
    kind            text not null,
    pinned          boolean not null default false,
    created_at      timestamptz not null default now(),
    check (kind in ('daily', 'manual', 'archive')),
    check (byte_length >= 0)
);

create index workspace_snapshots_workspace_idx
    on workspace_snapshots (workspace_id, created_at desc);

alter table database_backups
    add column pinned boolean not null default false;

alter table database_backups drop constraint if exists database_backups_kind_check;
alter table database_backups
    add constraint database_backups_kind_check
        check (kind in ('manual', 'pre_migration', 'daily', 'archive', 'pre_restore'));

create table application_archives (
    application_id            uuid primary key references applications (id),
    workspace_snapshot_id     uuid references workspace_snapshots (id),
    dev_database_backup_id    uuid references database_backups (id),
    prod_database_backup_id   uuid references database_backups (id),
    dev_release_id            uuid references application_releases (id),
    prod_release_id           uuid references application_releases (id),
    created_at                timestamptz not null default now()
);
