-- Deleted Workspace identities are permanent tombstones. Recreating the same
-- UUID must not inherit a previous lifecycle's generations or exec journal.
alter table workspaces
    drop constraint if exists workspaces_state_check;

alter table workspaces
    add constraint workspaces_state_check
    check (state in ('creating', 'ready', 'fenced', 'deleted'));
