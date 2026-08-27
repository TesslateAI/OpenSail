-- Workspace creator ownership. `0007_native_auth.sql` already added
-- `workspaces.created_by_user_id` and backfilled it from each owning
-- Project's owner; this version records the pin so the attribution is a
-- named, durable migration of its own. The statement is idempotent for
-- already-converged databases and adds nothing new to 0007's schema.
alter table workspaces
    add column if not exists created_by_user_id uuid references users (id);