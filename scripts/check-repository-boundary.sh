#!/usr/bin/env bash
set -euo pipefail

forbidden='lenso-platform-|lenso-module-auth|HostBuilder|HostLinkedModule|ModuleManifest|lenso module install|platform_core|platform_module'

if rg -n "$forbidden" Cargo.toml crates README.md --glob '!**/generated.rs'; then
  echo "legacy Lenso framework dependency or API found in vNext source" >&2
  exit 1
fi

if [[ -d crates/organization ]]; then
  echo "legacy crates/organization package still exists" >&2
  exit 1
fi

printf 'repository boundary is vNext-only\n'
