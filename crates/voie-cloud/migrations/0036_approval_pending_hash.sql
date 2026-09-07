-- One pending Approval per canonical action. Concurrent publish/delete
-- retries reuse that row instead of minting duplicates.

delete from approval_requests a
    using approval_requests b
    where a.state = 'pending'
      and b.state = 'pending'
      and a.project_id = b.project_id
      and a.kind = b.kind
      and a.action_hash = b.action_hash
      and a.created_at > b.created_at;

delete from approval_requests a
    using approval_requests b
    where a.state = 'pending'
      and b.state = 'pending'
      and a.project_id = b.project_id
      and a.kind = b.kind
      and a.action_hash = b.action_hash
      and a.created_at = b.created_at
      and a.id > b.id;

create unique index if not exists approval_requests_pending_hash_uidx
    on approval_requests (project_id, kind, action_hash)
    where state = 'pending';
