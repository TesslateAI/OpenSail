-- One canonical Owner membership per Project. The durable owner is
-- `projects.owner_user_id`; that User holds the only `role='owner'` row.
-- Extra owner memberships become admin. No ownership-transfer API.

insert into project_members (project_id, user_id, role)
select p.id, p.owner_user_id, 'owner'
from projects p
where not exists (
    select 1
    from project_members m
    where m.project_id = p.id
      and m.user_id = p.owner_user_id
);

update project_members m
set role = 'owner'
from projects p
where m.project_id = p.id
  and m.user_id = p.owner_user_id
  and m.role <> 'owner';

update project_members m
set role = 'admin'
from projects p
where m.project_id = p.id
  and m.user_id <> p.owner_user_id
  and m.role = 'owner';

create unique index if not exists project_members_one_owner_idx
    on project_members (project_id)
    where role = 'owner';
