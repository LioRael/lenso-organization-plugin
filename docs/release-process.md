# Release process

The former `lenso-module-organization` line ended at its existing public crate
versions and tags. The default branch now owns vNext packages with different
identities; it must not republish or overwrite the legacy package.

The four Capability packages and the PostgreSQL Plugin are public release
artifacts. The composition-only Secrets fixture remains private. Publication
is manual and must run from a clean `main` checkout through
`.github/workflows/release-plz.yml`.

Every push to `main` also asks release-plz to open or update the repository's
release pull request. Merging that pull request does not publish by itself;
publication still requires the explicitly confirmed live workflow dispatch.

Before the first publication of a new crate name:

1. prove generated contract freshness, workspace tests, PostgreSQL acceptance,
   repository boundary, and independent package verification;
2. make the four Capability packages public before the implementation package;
3. allocate the name using crates.io's required one-time initial-publish
   process; Trusted Publishing cannot create a new crate name;
4. configure a crates.io Trusted Publisher for every published crate with
   owner `LioRael`, repository `lenso-organization-plugin`, and workflow
   `release-plz.yml`; and
5. run the live workflow only after every crate has the matching publisher.

The workflow never accepts or falls back to a long-lived registry token.
Release-plz obtains a short-lived crates.io credential from GitHub OIDC, and
the live job has only the `id-token: write` permission needed for that exchange.

Do not use `--no-verify`, a long-lived registry token, or Git dependencies as a
publication shortcut.

Run a validation-only workflow first. A live run requires `live=true`,
`confirm=publish`, and the `main` ref. Publish order is:

1. `lenso-capability-organization-admin`;
2. `lenso-capability-organization-directory`;
3. `lenso-capability-organization-membership`;
4. `lenso-capability-organization-membership-admin`; and
5. `lenso-organization-postgres-plugin`.

## Local gates

```sh
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo test --locked --workspace
./scripts/check-repository-boundary.sh
```

Run PostgreSQL acceptance with `LENSO_POSTGRES_TEST_URL` and
`--include-ignored --test-threads=1` before any package becomes public.
