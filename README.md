# Lenso Organization Plugin

First-party Organization membership and PostgreSQL behavior for Lenso vNext. The
default branch is vNext-only; the former `lenso-module-organization` releases
remain available through their existing crate versions and Git tags.

## Workspace

- `lenso-capability-organization-admin` owns the generated
  `lenso.organization-admin@2` administrative role.
- `lenso-capability-organization-membership` owns the generated
  `lenso.organization-membership@1` membership-query role.
- `lenso-organization-postgres-plugin` atomically owns Organizations,
  memberships, first-class ownership, caller-scoped creation receipts, active
  slug uniqueness, and explicit schema administration.

The Plugin requires one explicitly bound `lenso.secrets@1` provider during
`prepare`. Composition supplies only the database URL reference, owned schema,
and exact caller Instance keys allowed to create Organizations. App boot checks
an existing compatible schema and never applies migrations.

The Membership Capability answers whether a subject is an active member or
owner of one Organization. Roles, permission grants, bindings, and RBAC
decisions belong to the independent Access Control Plugin. Calling target
Plugins combine current membership, Access Control, and resource-local rules
for final authorization and must not read Organization tables directly.

`create_organization` requires an idempotency key scoped to the exact admitted
caller Instance. A replay with the same normalized intent returns the original
Organization and owner-membership IDs with `created = false`; reusing that key
for a different intent returns `idempotency_conflict`. Active-slug conflicts
remain a separate Domain Error and are never inferred to be successful replays.

## Operator setup

Schema installation and upgrades are explicit deployment work:

```rust,no_run
use lenso_organization_postgres_plugin::OrganizationOperator;

# async fn setup(database_url: &str) -> Result<(), Box<dyn std::error::Error>> {
OrganizationOperator::setup(database_url, "organization").await?;
# Ok(())
# }
```

## Development

```sh
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo test --locked --workspace
./scripts/check-repository-boundary.sh
```

PostgreSQL acceptance additionally runs:

```sh
LENSO_POSTGRES_TEST_URL=postgres://... \
  cargo test --locked --workspace -- --include-ignored --test-threads=1
```

Capability Descriptors are authoritative. Each contract crate rejects a stale
generated Rust projection at build time. Invitations, Notification, HTTP/UI,
and Audit are intentionally outside the first vNext slice.
