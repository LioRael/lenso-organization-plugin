ALTER TABLE organization_memberships
    ADD COLUMN revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0);

ALTER TABLE organizations
    ADD COLUMN revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0);

ALTER TABLE organization_memberships
    ADD CONSTRAINT organization_memberships_organization_membership_key
    UNIQUE (organization_id, membership_id);

CREATE TABLE organization_membership_commands (
    caller_instance text NOT NULL CHECK (length(caller_instance) BETWEEN 1 AND 256),
    idempotency_key text NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 256),
    operation text NOT NULL CHECK (operation IN ('add_member', 'remove_member')),
    organization_id text NOT NULL,
    subject text NOT NULL CHECK (length(subject) BETWEEN 1 AND 256),
    membership_id text,
    result_revision bigint CHECK (result_revision > 0),
    changed boolean,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    completed_at timestamptz,
    PRIMARY KEY (caller_instance, idempotency_key),
    FOREIGN KEY (organization_id)
        REFERENCES organizations(organization_id)
        ON DELETE CASCADE
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (organization_id, membership_id)
        REFERENCES organization_memberships(organization_id, membership_id)
        ON DELETE RESTRICT,
    CHECK (
        (membership_id IS NULL AND result_revision IS NULL AND changed IS NULL AND completed_at IS NULL)
        OR
        (membership_id IS NOT NULL AND result_revision IS NOT NULL AND changed IS NOT NULL AND completed_at IS NOT NULL)
    )
);

CREATE INDEX organization_membership_commands_organization_idx
    ON organization_membership_commands (organization_id, created_at);
