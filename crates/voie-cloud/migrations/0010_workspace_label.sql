-- Workspace label: human-readable display name for one Workspace, set on
-- create (request value or the sane fallback 'Workspace') and editable by
-- the Project owner or the Workspace creator. Existing rows stay NULL; every
-- surface coalesces NULL to the fallback so no consumer ever renders blank.
alter table workspaces
    add column if not exists label text;