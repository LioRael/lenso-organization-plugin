# Lenso Organization Module

First-party Organization roles and PostgreSQL behavior for Lenso vNext. The
default branch is vNext-only; the former `lenso-module-organization` releases
remain available through their existing crate versions and Git tags.

## Workspace

- `lenso-capability-organization-admin` owns the generated
  `lenso.organization-admin@1` administrative role.
- `lenso-capability-organization-access` owns the generated
  `lenso.organization-access@1` permission-query role.
- `lenso-organization-postgres-module` atomically owns Organizations, roles,
  memberships, active slug uniqueness, and explicit schema administration.

The Module requires one explicitly bound `lenso.secrets@1` provider during
`prepare`. Composition supplies only the database URL reference, owned schema,
and exact caller Instance keys allowed to create Organizations. App boot checks
an existing compatible schema and never applies migrations.

The Access Capability answers whether a subject has one Organization
permission. Calling target Modules retain final authorization and must not read
Organization tables directly.

## Operator setup

Schema installation and upgrades are explicit deployment work:

```rust,no_run
use lenso_organization_postgres_module::OrganizationOperator;

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
