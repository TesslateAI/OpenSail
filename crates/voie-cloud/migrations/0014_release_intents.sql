-- Durable Release-intent ledger. application_releases holds the bulky object;
-- this table is the no-replay identity. Unknown classes are never deleted.
create table application_release_intents (
    build_intent_id uuid primary key,
    application_id  uuid not null references applications (id),
    request_hash    bytea not null,
    class           text not null,
    release_id      uuid,
    created_at      timestamptz not null default now(),
    unique (application_id, request_hash),
    check (class in ('dispatched', 'ready', 'failed', 'unknown'))
);

create index application_release_intents_app_idx
    on application_release_intents (application_id, created_at);

insert into application_release_intents (
    build_intent_id, application_id, request_hash, class, release_id, created_at
)
select
    build_intent_id,
    application_id,
    request_hash,
    case
        when state in ('reserved', 'dispatched') then 'dispatched'
        else state
    end,
    id,
    created_at
from application_releases
on conflict (build_intent_id) do nothing;
