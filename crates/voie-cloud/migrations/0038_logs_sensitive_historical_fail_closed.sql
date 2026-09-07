-- 0037 on this branch first backfilled only current bindings. Historical
-- rotated/unbound secret-bearing rows can still be false. Fail closed:
-- any row that predates the injection-time fence is treated as sensitive.
-- The column default stays false for deployments created after this.

update application_deployments
set logs_sensitive = true
where not logs_sensitive;
