-- One-shot heal for Control rows that recorded process `deleted` while
-- leaving desired_state live. After this cut, write sites persist desired
-- deleted/absent themselves; reconcilers do not rewrite history each tick.

update workspaces
    set desired_state = 'deleted',
        desired_revision = desired_revision + 1,
        reconcile_after = now()
    where state = 'deleted' and desired_state <> 'deleted';
