-- Traffic owner is desired/observed. `active_deployment_id` is the settled
-- projection (desired is not distinct from observed). Restart is a pod
-- generation, not a spec-revision bump that Fabric can ack without ReplacePod.

alter table application_environments
    add column if not exists desired_deployment_id uuid,
    add column if not exists observed_deployment_id uuid;

update application_environments
    set desired_deployment_id = active_deployment_id,
        observed_deployment_id = active_deployment_id
    where desired_deployment_id is null
      and observed_deployment_id is null;

alter table application_environments
    drop constraint if exists application_environments_desired_deployment_fk;

alter table application_environments
    add constraint application_environments_desired_deployment_fk
        foreign key (desired_deployment_id) references application_deployments (id);

alter table application_environments
    drop constraint if exists application_environments_observed_deployment_fk;

alter table application_environments
    add constraint application_environments_observed_deployment_fk
        foreign key (observed_deployment_id) references application_deployments (id);

alter table application_deployments
    add column if not exists desired_pod_generation bigint not null default 0,
    add column if not exists observed_pod_generation bigint not null default 0;

alter table application_deployments
    drop constraint if exists application_deployments_pod_generation_check;

alter table application_deployments
    add constraint application_deployments_pod_generation_check
        check (desired_pod_generation >= 0 and observed_pod_generation >= 0);
