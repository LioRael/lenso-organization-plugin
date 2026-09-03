# Organization Agent Tools Plugin card

## Owner and deletion boundary

`lenso-organization-agent-tools-plugin` is a private, stateless adapter.
Removing it removes only the Console Agent's Organization management Tools;
Organizations, memberships, receipts, and revisions remain owned by the
PostgreSQL Plugin.

## Roles

- Provides `lenso.agent.tool-provider@2` in the `tool-providers` root slot.
- Requires exactly one `lenso.organization-admin@2`, one
  `lenso.organization-directory@1`, and one
  `lenso.organization-membership-admin@1` Provider.
- Exposes three parallel-safe reads and three exclusive mutations.

## Authority boundary

The adapter decodes the existing portable Capability request schemas, forwards
the invocation context, preserves Domain Errors, and serializes responses. The
bound Organization provider retains exact caller admission, idempotency,
owner protection, revision semantics, lifecycle, and all durable state.

The adapter does not expose target-facing `check_membership`, infer RBAC roles,
access private PostgreSQL tables, delete Organizations, transfer ownership, or
implement invitations.
