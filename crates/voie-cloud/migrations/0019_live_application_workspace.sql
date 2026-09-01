-- A deleted Application stays `deleting` until Fabric cleanup finishes.
-- The original UNIQUE (workspace_id) blocked every later Application on
-- that Workspace for the rest of the row's life. One live Application per
-- Workspace is the contract; a deleting fence must not occupy it.
alter table applications drop constraint if exists applications_workspace_id_key;
create unique index if not exists applications_live_workspace_idx
    on applications (workspace_id)
    where state <> 'deleting';
