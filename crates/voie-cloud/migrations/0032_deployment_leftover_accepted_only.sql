-- Deployment leftover process is the CHECK dummy only. Realization is
-- desired/observed plus proven. Fabric journals stay on migrate/backup/
-- release-delete. Heal leftover failed onto desired absent, leftover
-- unknown onto accepted so reconcilers retry.
--
-- 0031 may have set proven=true on a leftover-failed row because it was
-- the Environment pointer, then converted that row to desired absent.
-- Clear proven on every leftover failed row before that conversion so
-- the stale bit cannot survive as a proven-absent Environment owner.

update application_deployments
    set proven = false
    where state = 'failed';

update application_deployments
    set desired_state = 'absent',
        desired_revision = case
            when desired_state = 'absent' then desired_revision
            else desired_revision + 1
        end,
        reconcile_after = now()
    where state = 'failed' and desired_state <> 'absent';

update application_deployments
    set state = 'accepted'
    where state <> 'accepted';

alter table application_deployments
    drop constraint if exists application_deployments_state_check;

alter table application_deployments
    add constraint application_deployments_state_check
        check (state = 'accepted');
