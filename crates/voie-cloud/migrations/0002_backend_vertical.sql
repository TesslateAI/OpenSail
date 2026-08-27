-- CTL2: ordered Blob references, model usage columns, exec journal result.

create table session_events (
    global_seq        bigint generated always as identity,
    session_id         uuid not null references sessions (id),
    revision           bigint not null,
    append_id          uuid not null,
    object_key         text not null unique,
    content_hash       bytea not null,
    byte_length        bigint not null,
    prompt_tokens      bigint,
    completion_tokens  bigint,
    total_tokens       bigint,
    created_at         timestamptz not null default now(),
    primary key (session_id, revision),
    unique (session_id, append_id)
);

