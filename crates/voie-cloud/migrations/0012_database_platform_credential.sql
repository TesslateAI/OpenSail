-- Platform Database passwords are Key Vault (or local-encrypted) material
-- addressed by UUID. They are not user_secrets rows and must not appear on
-- user secret APIs or in Workspace/conversation.
alter table application_databases
    drop constraint if exists application_databases_credential_secret_id_fkey;
