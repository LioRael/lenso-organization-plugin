CREATE TABLE organizations (
    organization_id text PRIMARY KEY,
    name text NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    slug text NOT NULL CHECK (length(slug) BETWEEN 1 AND 100),
    archived_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp()
);

CREATE UNIQUE INDEX organizations_active_slug_key
    ON organizations (slug)
    WHERE archived_at IS NULL;

CREATE TABLE organization_roles (
    role_id text PRIMARY KEY,
    organization_id text NOT NULL REFERENCES organizations(organization_id) ON DELETE CASCADE,
    name text NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    permissions text[] NOT NULL CHECK (cardinality(permissions) > 0),
    system_key text,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    UNIQUE (organization_id, name),
    UNIQUE (organization_id, system_key)
);

CREATE TABLE organization_memberships (
    membership_id text PRIMARY KEY,
    organization_id text NOT NULL REFERENCES organizations(organization_id) ON DELETE CASCADE,
    subject text NOT NULL CHECK (length(subject) BETWEEN 1 AND 256),
    role_id text NOT NULL REFERENCES organization_roles(role_id) ON DELETE RESTRICT,
    removed_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp()
);

CREATE UNIQUE INDEX organization_memberships_active_subject_key
    ON organization_memberships (organization_id, subject)
    WHERE removed_at IS NULL;

CREATE INDEX organization_memberships_subject_idx
    ON organization_memberships (subject);
