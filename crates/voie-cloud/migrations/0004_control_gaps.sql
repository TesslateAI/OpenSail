-- Control-plane gap closure: normalized audit rows and the bounded agent
-- bash capability. The development estate rebuilds this schema from version 1,
-- so each statement is idempotent for already-converged databases.

-- Agent capability is one bounded boolean, not a generic tool list.
alter table agents
    add column if not exists bash_enabled boolean not null default true;

alter table agents
    drop column if exists tool_ids;

-- Audit rows capture actor, resource, outcome, and structured metadata next
-- to the stable public event name in `kind`.
alter table audit_events
    add column if not exists actor_user_id uuid references users (id),
    add column if not exists resource_type text not null default '',
    add column if not exists resource_id uuid,
    add column if not exists outcome text not null default 'ok',
    add column if not exists metadata jsonb;

do $$
begin
    if not exists (
        select 1 from pg_constraint where conname = 'audit_events_outcome_check'
    ) then
        alter table audit_events
            add constraint audit_events_outcome_check
            check (outcome in ('ok', 'refused', 'error', 'unknown'));
    end if;
end $$;

create index if not exists audit_events_project_seq_idx
    on audit_events (project_id, seq);
