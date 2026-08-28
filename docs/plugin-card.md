# vNext Organization Plugin card

## Owner and deletion boundary

`lenso-organization-postgres-plugin` owns Organization identity, active slug
uniqueness, roles, permissions, memberships, and the transaction that creates
an Organization with its first owner. Removing its package selection, Plugin
Instance, bindings, and owned PostgreSQL schema removes all Organization
behavior and state; Kernel keeps no Organization branch or registry entry.

## Roles

- Provides `lenso.organization-admin@1` for explicitly allowed administrative
  callers to create an Organization and its first owner membership.
- Provides `lenso.organization-access@1` so target Plugins can ask whether an
  Auth subject has one named permission. The target Plugin retains final
  authorization authority.
- Requires `lenso.secrets@1` during `prepare` to resolve the PostgreSQL URL.

## Lifecycle and state

Composition supplies an owned schema name, a Secrets reference, and the exact
caller Instance keys allowed to use Organization Admin. `prepare` resolves the
secret and verifies an already-installed schema. Migrations are explicit
operator work and never run during App boot. `deactivate` closes the owned
pool. No background work exists in the first slice.

## First observable behavior

One Admin request atomically creates an Organization, its protected owner role,
and its owner membership. Access requests then return whether that subject has
a requested permission. Duplicate active slugs and forbidden Admin callers are
Domain Errors; unavailable or incompatible PostgreSQL is a Runtime Failure.

Invitations, notification delivery, HTTP/UI contributions, and durable Audit
events are deferred vertical slices with their own explicit Capability edges.
