-- Archive drops the local Workspace LV. The identity stays; it is not
-- ready and it is not a deleted tombstone.

alter table workspaces
    drop constraint if exists workspaces_state_check;

alter table workspaces
    add constraint workspaces_state_check
    check (state in ('creating', 'ready', 'fenced', 'archived', 'deleted'));
