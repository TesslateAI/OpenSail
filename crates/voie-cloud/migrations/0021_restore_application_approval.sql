-- Aggregate Application restore is its own durable approval kind.

alter table approval_requests drop constraint if exists approval_requests_kind_check;
alter table approval_requests
    add constraint approval_requests_kind_check
        check (kind in (
            'publish_production',
            'make_environment_public',
            'bind_production_secret',
            'restore_database',
            'restore_application',
            'delete_database',
            'delete_application',
            'increase_resource_tier'
        ));
