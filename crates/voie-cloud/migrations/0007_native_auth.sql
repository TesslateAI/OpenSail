-- Native User + Project-scope correction (packet issue: native users and
-- conversation product).
--
-- Users stay canonical VOIE Users with stable existing IDs. Providers only
-- authenticate or link them: `auth_identities` holds provider
-- (issuer, subject) -> user_id links, and `native_credentials` holds the
-- Argon2id hash for native login. No provider claim, issuer, or subject
-- controls authorization; project membership plus the explicit
-- `platform_role` do.
--
-- Projects gain `kind` (personal | team) as the collaboration scope. The
-- existing project_members table is extended with the team-style role
-- vocabulary (owner | admin | member | viewer) and remains the
-- authorization boundary. There is no first-class Teams table and no FK
-- rewrite.
--
-- The development estate rebuilds this disposable schema from version 1, so
-- each statement is idempotent for already-converged databases.

-- 1. Canonical User profile columns. Legacy issuer/subject values are
-- migrated into auth_identities; future Users are provider-independent.
alter table users
    add column if not exists username text,
    add column if not exists display_name text not null default '',
    add column if not exists email text,
    add column if not exists status text not null default 'active',
    add column if not exists platform_role text not null default 'user',
    add column if not exists updated_at timestamptz not null default now();

alter table users alter column issuer drop not null;
alter table users alter column subject drop not null;

create unique index if not exists users_username_idx
    on users (username)
    where username is not null;

do $$
begin
    if not exists (
        select 1 from pg_constraint where conname = 'users_status_check'
    ) then
        alter table users
            add constraint users_status_check
            check (status in ('active', 'disabled'));
    end if;
end $$;

do $$
begin
    if not exists (
        select 1 from pg_constraint where conname = 'users_platform_role_check'
    ) then
        alter table users
            add constraint users_platform_role_check
            check (platform_role in ('user', 'admin'));
    end if;
end $$;

-- 2. Native credentials: Argon2id PHC string, one per User, in a separate
--    table so provider identities never share a row with a password.
create table if not exists native_credentials (
    user_id    uuid primary key references users (id) on delete cascade,
    password_hash text not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

-- 3. Auth identities: provider (issuer, subject) -> user_id links. The
--    legacy `users.issuer`/`users.subject` columns are backfilled here and
--    remain for compatibility; new links land only in this table.
create table if not exists auth_identities (
    provider   text not null,
    issuer     text not null,
    subject    text not null,
    user_id    uuid not null references users (id) on delete cascade,
    created_at timestamptz not null default now(),
    primary key (provider, issuer, subject)
);

create index if not exists auth_identities_user_idx
    on auth_identities (user_id);

-- Backfill current issuer/subject pairs. The conflict check is a pre-insert
-- assertion: a legacy pair that already maps to a different user is an
-- ambiguity and fails the migration loudly instead of guessing. Only pairs
-- with no existing row are inserted afterwards.
do $$
declare
    conflict_count bigint;
begin
    select count(*) into conflict_count
    from users u
    where u.issuer <> 'native'
      and exists (
          select 1 from auth_identities a
          where a.provider = 'oidc' and a.issuer = u.issuer and a.subject = u.subject
            and a.user_id <> u.id
      );
    if conflict_count > 0 then
        raise exception using message = format(
            'auth identity backfill: %s legacy user(s) map to an identity already linked to a different user; resolve the ambiguity before migrating',
            conflict_count
        );
    end if;
end
$$;

insert into auth_identities (provider, issuer, subject, user_id)
select 'oidc', u.issuer, u.subject, u.id
from users u
where u.issuer <> 'native'
  and not exists (
      select 1 from auth_identities a
      where a.provider = 'oidc' and a.issuer = u.issuer and a.subject = u.subject
  );

-- 4. Project collaboration scope. Existing projects are `personal` (their
--    owner is the single user); `team` is created explicitly later. A
--    converging estate classifies deterministically: a project with more
--    than one active member is a team scope, an owner-only project stays
--    personal.
alter table projects
    add column if not exists kind text not null default 'personal';

update projects p
set kind = 'team'
where (select count(*)
       from project_members m
       join users u on u.id = m.user_id
       where m.project_id = p.id and u.status = 'active') > 1;

do $$
begin
    if not exists (
        select 1 from pg_constraint where conname = 'projects_kind_check'
    ) then
        alter table projects
            add constraint projects_kind_check
            check (kind in ('personal', 'team'));
    end if;
end $$;

-- 5. Workspace creator ownership: the durable creator of each Workspace.
--    Legacy Workspaces are attributed deterministically to their Project's
--    owner; any Workspace that cannot be attributed (no owning Project) is
--    an ambiguity and fails the migration loudly.
alter table workspaces
    add column if not exists created_by_user_id uuid references users (id);

update workspaces w
set created_by_user_id = p.owner_user_id
from projects p
where w.project_id = p.id
  and w.created_by_user_id is null;

do $$
declare
    unattributed bigint;
begin
    select count(*) into unattributed
    from workspaces where created_by_user_id is null;
    if unattributed > 0 then
        raise exception using message = format(
            'workspace creator backfill: %s workspace(s) have no owning Project to attribute; resolve the ambiguity before migrating',
            unattributed
        );
    end if;
end
$$;

-- 6. Run actor attribution and the durable per-session turn ordinal. `seq`
--    is the strict per-session acceptance order: follow-ups queue behind
--    their predecessor and dispatch only after it settles.
alter table runs
    add column if not exists actor_user_id uuid references users (id),
    add column if not exists seq bigint;

-- Backfill the turn ordinal for converging estates: existing runs keep
-- their acceptance order within each session.
do $$
declare
    r record;
    next_seq bigint;
begin
    for r in
        select id, session_id
        from runs
        where seq is null
        order by session_id, accepted_at, id
    loop
        select coalesce(max(seq), 0) + 1 into next_seq
        from runs where session_id = r.session_id;
        update runs set seq = next_seq where id = r.id;
    end loop;
end
$$;

alter table runs
    alter column seq set not null;

create unique index if not exists runs_session_seq_idx
    on runs (session_id, seq);

-- 7. Session actor attribution: the last human who queued a Run.
alter table sessions
    add column if not exists last_actor_user_id uuid references users (id);

-- 8. Project role vocabulary: owner | admin | member | viewer. `admin` is
--    the team-style management role; the durable project owner stays
--    `owner` and remains protected.
alter table project_members
    drop constraint if exists project_members_role_check;

do $$
begin
    if not exists (
        select 1 from pg_constraint where conname = 'project_members_role_check'
    ) then
        alter table project_members
            add constraint project_members_role_check
            check (role in ('owner', 'admin', 'member', 'viewer'));
    end if;
end $$;
