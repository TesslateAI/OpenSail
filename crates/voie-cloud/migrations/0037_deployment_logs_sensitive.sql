-- One-way fact: this Deployment received secret material. Model-facing
-- log text must not be reconstructed from historical Blob bytes after
-- rotation or unbind. Do not store previous plaintext for redaction.
--
-- Existing rows are not provably safe (bindings may already be gone).
-- Mark them all sensitive. New rows keep default false and use the
-- injection-time fence.

alter table application_deployments
    add column if not exists logs_sensitive boolean not null default false;

update application_deployments
set logs_sensitive = true;
