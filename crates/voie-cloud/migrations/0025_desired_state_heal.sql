-- Heal desired-state columns when version 24 was recorded from an earlier
-- shape. Every statement is idempotent.

alter table application_databases
    add column if not exists desired_state text,
    add column if not exists observed_state text,
    add column if not exists last_error_code text,
    add column if not exists reconcile_after timestamptz,
    add column if not exists security_profile integer;

update application_databases
    set desired_state = coalesce(desired_state, 'present'),
        observed_state = coalesce(nullif(observed_state, ''), state),
        security_profile = coalesce(security_profile, 1);

alter table application_databases
    alter column desired_state set default 'present',
    alter column observed_state set default '',
    alter column security_profile set default 1;

alter table application_databases alter column desired_state set not null;
alter table application_databases alter column observed_state set not null;
alter table application_databases alter column security_profile set not null;

alter table application_deployments
    add column if not exists desired_state text,
    add column if not exists observed_state text,
    add column if not exists last_error_code text,
    add column if not exists reconcile_after timestamptz;

update application_deployments
    set desired_state = coalesce(desired_state, 'running'),
        observed_state = coalesce(nullif(observed_state, ''), state);

alter table application_deployments
    alter column desired_state set default 'running',
    alter column observed_state set default '';
alter table application_deployments alter column desired_state set not null;
alter table application_deployments alter column observed_state set not null;

alter table workspaces
    add column if not exists desired_state text,
    add column if not exists observed_state text,
    add column if not exists desired_revision bigint,
    add column if not exists observed_revision bigint,
    add column if not exists last_error_code text,
    add column if not exists reconcile_after timestamptz;

update workspaces
    set desired_state = coalesce(desired_state, 'active'),
        observed_state = coalesce(nullif(observed_state, ''), state),
        desired_revision = coalesce(desired_revision, 0),
        observed_revision = coalesce(observed_revision, 0);

alter table workspaces
    alter column desired_state set default 'active',
    alter column observed_state set default '',
    alter column desired_revision set default 0,
    alter column observed_revision set default 0;
alter table workspaces alter column desired_state set not null;
alter table workspaces alter column observed_state set not null;
alter table workspaces alter column desired_revision set not null;
alter table workspaces alter column observed_revision set not null;

do $$ begin
    alter table application_databases
        add constraint application_databases_desired_state_check
            check (desired_state in ('present', 'suspended', 'absent'));
exception when duplicate_object then null;
end $$;

do $$ begin
    alter table application_databases
        add constraint application_databases_security_profile_check
            check (security_profile >= 1);
exception when duplicate_object then null;
end $$;

do $$ begin
    alter table application_deployments
        add constraint application_deployments_desired_state_check
            check (desired_state in ('running', 'stopped', 'absent'));
exception when duplicate_object then null;
end $$;

do $$ begin
    alter table workspaces
        add constraint workspaces_desired_state_check
            check (desired_state in ('active', 'suspended', 'archived', 'deleted'));
exception when duplicate_object then null;
end $$;
