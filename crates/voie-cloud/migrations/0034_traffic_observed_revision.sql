-- Environment traffic has its own desired/observed pair. `revision` is the
-- desired traffic generation. `traffic_observed_revision` is the last Fabric
-- generation Control accepted. Desired `NULL` is a real absent spec, not
-- "never set": drift is `revision > traffic_observed_revision`.

alter table application_environments
    add column if not exists traffic_observed_revision bigint not null default 0;

alter table application_environments
    drop constraint if exists application_environments_traffic_observed_revision_check;

alter table application_environments
    add constraint application_environments_traffic_observed_revision_check
        check (traffic_observed_revision >= 0);
