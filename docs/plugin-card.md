# vNext Organization Plugin card

## Owner and deletion boundary

`lenso-organization-postgres-plugin` owns Organization identity, active slug
uniqueness, memberships, first-class ownership, caller-scoped creation
and membership-command receipts, membership revisions, and the transaction
that creates an Organization with its first owner. Removing its package
selection, Plugin Instance, bindings, and owned
PostgreSQL schema removes all Organization behavior and state; Kernel keeps no
Organization branch or registry entry.

## Roles

- Provides `lenso.organization-admin@2` for explicitly allowed administrative
  callers to create an Organization and its first owner membership.
- Provides `lenso.organization-membership@1` so target Plugins can ask whether
  an Auth subject is an active member or owner. The target Plugin retains final
  authorization authority and obtains role decisions from Access Control.
- Provides `lenso.organization-directory@1` to separately admitted peer
  Plugins that need the authoritative Organization name, slug, active state,
  and revision without direct database access.
- Provides `lenso.organization-membership-admin@1` for separately admitted
  callers to add and remove non-owner members with caller-scoped idempotency.
- Requires `lenso.secrets@1` during `prepare` to resolve the PostgreSQL URL.

## Lifecycle and state

Composition supplies an owned schema name, a Secrets reference, and separate
exact caller Instance allowlists for Organization Admin and Membership Admin.
`prepare` resolves the secret and verifies an already-installed schema.
Migrations are explicit operator work and never run during App boot.
`deactivate` closes the owned pool. No background work exists in this slice.

## First observable behavior

One Admin request atomically persists its caller-scoped idempotency receipt,
creates an Organization, and creates a first-class owner membership. An exact
replay returns the same IDs with `created = false`; reusing the key for another
intent returns `idempotency_conflict`, while duplicate active slugs remain a
separate Domain Error. Membership requests then return whether that subject is
active and an owner. Forbidden Admin callers are Domain Errors; unavailable or
incompatible PostgreSQL is a Runtime Failure.

Directory reads expose no membership or RBAC facts. Membership Admin commands
lock the active Organization, reserve a durable
caller-scoped command identity, and then add an active membership or tombstone
a non-owner membership. Replays return the original membership ID and revision
without repeating the mutation. Concurrent adds converge on one active
membership, and owner removal fails closed.

Role definitions, permission grants, and subject-role bindings are explicitly
outside this deletion boundary and belong to the independent Access Control
Plugin. Existing Organization schemas upgrade by projecting the former
protected owner role into the `is_owner` membership fact before dropping all
role and permission storage. The upgrade aborts before that destructive drop
if any active Organization would have no active owner.

Invitations, provisioning, notification delivery, HTTP/UI contributions, and
durable Audit events are separate vertical slices with explicit Capability
edges; they may call Membership Admin but do not gain access to its tables.
