-- Archive generations: a live Application starts a new capturing generation
-- that never inherits restore points from a prior completed archive.
-- Incremental retry may reuse pointers only on the in-progress generation.
-- The previous complete generation stays until the new one is durable and
-- promoted atomically.

create table application_archive_generations (
    id                         uuid primary key,
    application_id             uuid not null references applications (id),
    generation                 bigint not null,
    state                      text not null,
    workspace_snapshot_id      uuid references workspace_snapshots (id),
    dev_database_backup_id     uuid references database_backups (id),
    prod_database_backup_id    uuid references database_backups (id),
    dev_release_id             uuid references application_releases (id),
    prod_release_id            uuid references application_releases (id),
    created_at                 timestamptz not null default now(),
    unique (application_id, generation),
    check (generation > 0),
    check (state in ('capturing', 'complete', 'superseded'))
);

create unique index application_archive_capturing_one
    on application_archive_generations (application_id)
    where state = 'capturing';

create unique index application_archive_complete_one
    on application_archive_generations (application_id)
    where state = 'complete';

insert into application_archive_generations (
    id, application_id, generation, state,
    workspace_snapshot_id, dev_database_backup_id, prod_database_backup_id,
    dev_release_id, prod_release_id, created_at
)
select gen_random_uuid(), application_id, 1, 'complete',
       workspace_snapshot_id, dev_database_backup_id, prod_database_backup_id,
       dev_release_id, prod_release_id, created_at
from application_archives;

drop table application_archives;
alter table application_archive_generations rename to application_archives;
