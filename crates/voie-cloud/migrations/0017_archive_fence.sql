-- Durable archive fencing: mutations are rejected while archiving or restoring.

alter table applications drop constraint if exists applications_state_check;
alter table applications
    add constraint applications_state_check
        check (state in (
            'creating', 'ready', 'suspended',
            'archiving', 'archived', 'restoring', 'deleting'
        ));
