# Member workspace discovery

Organization Directory 1.1.0 adds `list_for_subject`. The PostgreSQL Plugin owns membership and organization state; the operation returns only active memberships in active organizations, ordered by organization ID with bounded keyset pagination. Only configured directory callers can use it.

Projects Web derives the subject from authenticated user evidence and exposes `/api/projects/workspaces`. Neither browser query parameters nor Console service input can select an actor. Console adds only the fixed read operation `list_workspaces`; it receives names, slugs and IDs, never credentials or unrestricted organization administration.

Local integration currently uses explicit source patches. Publish the directory contract and provider before releasing consumers that require 1.1.0. Removing the directory provider makes the consumer dependency unavailable; there is no global organization enumeration fallback.
