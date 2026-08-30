ALTER TABLE organization_memberships
    ADD COLUMN is_owner boolean NOT NULL DEFAULT false;

UPDATE organization_memberships AS membership
SET is_owner = true
FROM organization_roles AS role
WHERE membership.role_id = role.role_id
  AND role.system_key = 'owner';

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM organizations AS organization
        WHERE organization.archived_at IS NULL
          AND NOT EXISTS (
              SELECT 1
              FROM organization_memberships AS membership
              WHERE membership.organization_id = organization.organization_id
                AND membership.removed_at IS NULL
                AND membership.is_owner
          )
    ) THEN
        RAISE EXCEPTION
            'cannot separate Access Control: active organization has no active owner';
    END IF;
END
$$;

ALTER TABLE organization_memberships
    DROP COLUMN role_id;

DROP TABLE organization_roles;

CREATE INDEX organization_memberships_active_owner_idx
    ON organization_memberships (organization_id)
    WHERE removed_at IS NULL AND is_owner;
