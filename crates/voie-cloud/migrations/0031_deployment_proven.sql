-- Prove-then-switch is a boolean, not leftover process healthy/active.
-- Leftover process remaining values are the CHECK dummy and journals.

alter table application_deployments
    add column if not exists proven boolean not null default false;

update application_deployments
    set proven = true
    where state in ('healthy', 'active')
       or exists (
            select 1 from application_environments e
            where e.active_deployment_id = application_deployments.id
       );

update application_deployments
    set desired_state = 'absent',
        desired_revision = case
            when desired_state = 'absent' then desired_revision
            else desired_revision + 1
        end,
        reconcile_after = now()
    where state in ('superseded', 'stopped', 'failed')
      and desired_state <> 'absent';

update application_deployments
    set state = 'accepted'
    where state not in ('unknown', 'failed', 'accepted');

alter table application_deployments
    drop constraint if exists application_deployments_state_check;

alter table application_deployments
    add constraint application_deployments_state_check
        check (state in ('accepted', 'failed', 'unknown'));
