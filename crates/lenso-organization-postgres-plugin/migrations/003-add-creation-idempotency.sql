CREATE TABLE organization_creation_requests (
    caller_instance text NOT NULL CHECK (length(caller_instance) BETWEEN 1 AND 256),
    idempotency_key text NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 256),
    name text NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    slug text NOT NULL CHECK (length(slug) BETWEEN 1 AND 100),
    owner_subject text NOT NULL CHECK (length(owner_subject) BETWEEN 1 AND 256),
    organization_id text NOT NULL UNIQUE,
    owner_membership_id text NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (caller_instance, idempotency_key),
    FOREIGN KEY (organization_id)
        REFERENCES organizations(organization_id)
        ON DELETE RESTRICT
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (owner_membership_id)
        REFERENCES organization_memberships(membership_id)
        ON DELETE RESTRICT
        DEFERRABLE INITIALLY DEFERRED
);
