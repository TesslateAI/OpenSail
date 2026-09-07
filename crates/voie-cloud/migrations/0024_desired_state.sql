-- Desired vs observed product state. PostgreSQL rows are the durable
-- reconcile queue: desired_revision > observed_revision OR reconcile_after.
-- ADD COLUMN IF NOT EXISTS so a partial earlier 24 does not block migrate.

alter table application_databases
    add column if not exists desired_state text,
    add column if not exists observed_state text,
    add column if not exists last_error_code text,
    add column if not exists reconcile_after timestamptz,
    add column if not exists security_profile integer;

update application_databases
    set desired_state = coalesce(desired_state, 'present')
    where desired_state is null;
update application_databases
    set observed_state = coalesce(nullif(observed_state, ''), state)
    where observed_state is null or observed_state = '';
update application_databases
    set security_profile = coalesce(security_profile, 1)
    where security_profile is null;
update application_databases
    set desired_state = 'absent'
    where state in ('deleted', 'deleting');

alter table application_databases
    alter column desired_state set default 'present',
    alter column desired_state set not null,
    alter column observed_state set default '',
    alter column observed_state set not null,
    alter column security_profile set default 1,
    alter column security_profile set not null;

alter table application_deployments
    add column if not exists desired_state text,
    add column if not exists observed_state text,
    add column if not exists last_error_code text,
    add column if not exists reconcile_after timestamptz;

update application_deployments
    set desired_state = coalesce(desired_state, 'running')
    where desired_state is null;
update application_deployments
    set observed_state = coalesce(nullif(observed_state, ''), state)
    where observed_state is null or observed_state = '';
update application_deployments
    set desired_state = 'stopped'
    where state = 'stopped';
update application_deployments
    set desired_state = 'absent'
    where state = 'superseded';

alter table application_deployments
    alter column desired_state set default 'running',
    alter column desired_state set not null,
    alter column observed_state set default '',
    alter column observed_state set not null;

alter table workspaces
    add column if not exists desired_state text,
    add column if not exists observed_state text,
    add column if not exists desired_revision bigint,
    add column if not exists observed_revision bigint,
    add column if not exists last_error_code text,
    add column if not exists reconcile_after timestamptz;

update workspaces
    set desired_state = coalesce(desired_state, 'active')
    where desired_state is null;
update workspaces
    set observed_state = coalesce(nullif(observed_state, ''), state)
    where observed_state is null or observed_state = '';
update workspaces
    set desired_revision = coalesce(desired_revision, 0)
    where desired_revision is null;
update workspaces
    set observed_revision = coalesce(observed_revision, 0)
    where observed_revision is null;
update workspaces
    set desired_state = 'archived'
    where state = 'archived';
update workspaces
    set desired_state = 'deleted'
    where state in ('deleted', 'deleting');
update workspaces
    set desired_state = 'suspended'
    where state = 'fenced';

alter table workspaces
    alter column desired_state set default 'active',
    alter column desired_state set not null,
    alter column observed_state set default '',
    alter column observed_state set not null,
    alter column desired_revision set default 0,
    alter column desired_revision set not null,
    alter column observed_revision set default 0,
    alter column observed_revision set not null;

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
