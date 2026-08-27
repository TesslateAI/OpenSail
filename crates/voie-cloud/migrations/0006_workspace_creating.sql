-- Indeterminate Fabric workspace creates need a durable reservation.
-- Control reserves a `creating` Workspace before invoking the Fabric: an
-- indeterminate outcome (Fabricd's Unknown verdict, HTTP 202) keeps the
-- `creating` row instead of exposing it as ready, so callers and session
-- attachment never see an unprovisioned Workspace. Only Fabric's own 200
-- success promotes `creating` back to `ready`; definite refusals (non-2xx
-- or transport) release the reservation, and a read-only existence probe
-- (GET /v1/workspaces/{id} on the Fabric) converges any earlier
-- indeterminate reservation on the next user-initiated create for that id
-- without automatically retrying the unknown create.
--
-- The development estate rebuilds this disposable schema from version 1, so
-- dropping and recreating the check constraint is safe; converging
-- estates already at the prior constraint are handled idempotently by
-- re-adding the constraint with the wider state set.

alter table workspaces
    drop constraint if exists workspaces_state_check;

do $$
begin
    if not exists (
        select 1 from pg_constraint where conname = 'workspaces_state_check'
    ) then
        alter table workspaces
            add constraint workspaces_state_check
            check (state in ('creating', 'ready', 'fenced'));
    end if;
end $$;

-- Existing workspaces are all `ready` under the former constraint, but a
-- converging estate may carry orphaned rows from a partially-applied prior
-- revision; normalizing any unexpected state to `ready` is bounded and
-- keeps the constraint applicable without an explicit scan of every row.
-- The statement is a no-op in the common case.
update workspaces set state = 'ready' where state not in ('creating', 'ready', 'fenced');
