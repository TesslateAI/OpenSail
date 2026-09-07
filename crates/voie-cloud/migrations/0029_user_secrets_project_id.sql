-- Product secrets belong to a Project. JSON already emits projectId;
-- this rename retires the leftover SQL column name.

alter table user_secrets rename column scope_id to project_id;

alter index if exists user_secrets_scope_id_idx
    rename to user_secrets_project_id_idx;
