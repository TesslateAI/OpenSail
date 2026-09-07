-- PostgreSQL owns active route intent. Fabric SQLite owns the realized map.
-- One Fabric row carries the map revision; derived routes are not a journal.

alter table fabrics
    add column if not exists desired_route_revision bigint,
    add column if not exists observed_route_revision bigint;

update fabrics
    set desired_route_revision = coalesce(desired_route_revision, 0)
    where desired_route_revision is null;
update fabrics
    set observed_route_revision = coalesce(observed_route_revision, 0)
    where observed_route_revision is null;

alter table fabrics
    alter column desired_route_revision set default 0,
    alter column desired_route_revision set not null,
    alter column observed_route_revision set default 0,
    alter column observed_route_revision set not null;
