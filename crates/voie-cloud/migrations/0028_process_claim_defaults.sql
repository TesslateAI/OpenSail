-- Leftover process columns are claim / prove / journal / tombstone.
-- Mutation INSERT omits process adjectives; schema defaults satisfy CHECK.
-- Product ready is observed_state, not leftover process ready.

update workspaces
    set state = 'creating'
    where state = 'ready';

alter table workspaces
    alter column state set default 'creating';

alter table workspaces
    drop constraint if exists workspaces_state_check;

alter table workspaces
    add constraint workspaces_state_check
        check (state in ('creating', 'fenced', 'archived', 'deleted'));

update application_databases
    set state = 'creating'
    where state = 'ready';

alter table application_databases
    alter column state set default 'creating';

update application_deployments
    set state = 'accepted'
    where state in ('starting', 'materializing', 'activating');

alter table application_deployments
    alter column state set default 'accepted';
