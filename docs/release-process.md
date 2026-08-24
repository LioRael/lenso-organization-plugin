# Release process

The former `lenso-module-organization` line ended at its existing public crate
versions and tags. The default branch now owns vNext packages with different
identities; it must not republish or overwrite the legacy package.

All vNext packages remain `publish = false` while the first vertical slice is
under acceptance. The release workflow is therefore manual and dry-run-only.
Before enabling publication:

1. prove generated contract freshness, workspace tests, PostgreSQL acceptance,
   repository boundary, and independent package verification;
2. make the two Capability packages public before the implementation package;
3. allocate each new crates.io name from a reviewed clean `main` checkout with
   a temporary restricted token, then revoke it;
4. configure crates.io Trusted Publishers for this repository and
   `.github/workflows/release-plz.yml`; and
5. replace the parked workflow only in a separately reviewed release change.

Do not use `--no-verify`, a long-lived registry token, or Git dependencies as a
publication shortcut.

## Local gates

```sh
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo test --locked --workspace
./scripts/check-repository-boundary.sh
```

Run PostgreSQL acceptance with `LENSO_POSTGRES_TEST_URL` and
`--include-ignored --test-threads=1` before any package becomes public.
