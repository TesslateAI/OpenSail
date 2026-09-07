-- Leftover process is the exclusive Workspace replace fence (and a CHECK
-- dummy). Tombstone and archive identity are desired_state. Database
-- leftover process is unused; product ready is observed_state.

update workspaces
    set desired_state = 'deleted',
        desired_revision = case
            when desired_state = 'deleted' then desired_revision
            else desired_revision + 1
        end,
        reconcile_after = now()
    where state = 'deleted' and desired_state <> 'deleted';

update workspaces
    set desired_state = 'archived',
        desired_revision = case
            when desired_state = 'archived' then desired_revision
            else desired_revision + 1
        end,
        reconcile_after = now()
    where state = 'archived' and desired_state <> 'archived';

update workspaces
    set state = 'creating'
    where state in ('deleted', 'archived');

alter table workspaces
    drop constraint if exists workspaces_state_check;

alter table workspaces
    add constraint workspaces_state_check
        check (state in ('creating', 'fenced'));

update application_databases
    set state = 'creating'
    where state <> 'creating';

alter table application_databases
    drop constraint if exists application_databases_state_check;

alter table application_databases
    add constraint application_databases_state_check
        check (state = 'creating');
