-- CTL2: PostgreSQL hot Session log. Blob is not used for events.

create table session_events (
    global_seq         bigint generated always as identity,
    session_id         uuid not null references sessions (id),
    revision           bigint not null,
    append_id          uuid not null,
    content_hash       bytea not null,
    byte_length        bigint not null,
    prompt_tokens      bigint,
    completion_tokens  bigint,
    total_tokens       bigint,
    payload            bytea not null,
    first_event_seq    bigint,
    last_event_seq     bigint,
    created_at         timestamptz not null default now(),
    primary key (session_id, revision),
    unique (session_id, append_id)
);

create index session_events_seq_seek_idx
    on session_events (session_id, first_event_seq);
