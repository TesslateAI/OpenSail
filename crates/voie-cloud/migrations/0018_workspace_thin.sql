-- Workspace virtual tiers: default 16 GiB, large 32 GiB, elevated 64 GiB.
-- New Workspaces start at 16 GiB. Existing rows keep their recorded size.
-- Backup and snapshot metadata is idempotent on the deterministic object key.

alter table workspaces
    alter column allocated_bytes set default 17179869184;

alter table workspaces drop constraint if exists workspaces_storage_tier_check;
alter table workspaces
    add constraint workspaces_storage_tier_check
        check (storage_tier in ('default', 'large', 'elevated'));

alter table database_backups drop constraint if exists database_backups_object_key_unique;
alter table database_backups
    add constraint database_backups_object_key_unique unique (object_key);

alter table workspace_snapshots drop constraint if exists workspace_snapshots_object_key_unique;
alter table workspace_snapshots
    add constraint workspace_snapshots_object_key_unique unique (object_key);
