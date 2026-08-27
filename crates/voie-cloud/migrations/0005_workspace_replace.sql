-- Workspace replace semantics and durable project ownership.
--
-- One execution generation per Workspace: it advances only after the Fabric
-- confirms the replacement, so PostgreSQL always names the generation the
-- Fabric actually realizes.
--
-- Every Workspace is durably owned by exactly one Project. Ownership decides
-- authorization everywhere. A legacy estate converges deterministically: when
-- exactly one Project exists, its ownerless Workspaces (with their Sessions
-- and ExecCalls) are attributed to it; otherwise the migration fails with an
-- explicit data-rebuild instruction instead of a raw foreign-key error. The
-- development estate rebuilds this disposable schema from version 1.
--
-- The lifecycle fence makes every mutation (delete, replace) transactional
-- with respect to session attachment: a fenced Workspace accepts no new
-- Sessions, and generations advance only inside the fence.

alter table workspaces
    add column if not exists exec_generation bigint not null default 0;

alter table workspaces
    add column if not exists project_id uuid references projects (id);

alter table workspaces
    add column if not exists state text not null default 'ready';

do $$
declare
    project_count bigint;
    orphan_count bigint;
begin
    select count(*) into project_count from projects;

    update workspaces w
    set project_id = (select id from projects order by created_at limit 1)
    where w.project_id is null
      and project_count = 1;

    select count(*) into orphan_count from workspaces where project_id is null;

    if orphan_count > 0 then
        raise exception using message = format(
            'workspace ownership migration: %s ownerless workspace(s) cannot be attributed deterministically with %s project(s) present; rebuild the disposable development schema from version 1',
            orphan_count,
            project_count
        );
    end if;
end
$$;

delete from workspaces
where project_id is null;

alter table workspaces
    alter column project_id set not null;

do $$
begin
    if not exists (
        select 1 from pg_constraint where conname = 'workspaces_state_check'
    ) then
        alter table workspaces
            add constraint workspaces_state_check
            check (state in ('ready', 'fenced'));
    end if;
end $$;

create index if not exists workspaces_project_idx
    on workspaces (project_id);
