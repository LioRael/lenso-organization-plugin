use lenso_postgres_kit::{Migration, PlanError, SchemaPlan, sql_migrations};

const MIGRATIONS: &[Migration] = sql_migrations![
    (
        1,
        "create-organizations",
        "migrations/001_create_organizations.sql",
    ),
    (
        2,
        "separate-access-control",
        "migrations/002-separate-access-control.sql",
    ),
    (
        3,
        "add-creation-idempotency",
        "migrations/003-add-creation-idempotency.sql",
    ),
];

pub(crate) fn schema_plan(schema: impl Into<std::sync::Arc<str>>) -> Result<SchemaPlan, PlanError> {
    SchemaPlan::new(schema, MIGRATIONS)
}

#[cfg(test)]
mod tests {
    use super::MIGRATIONS;

    #[test]
    fn separation_fails_closed_after_owner_projection_and_before_role_drop() {
        let migration = MIGRATIONS[1].sql();
        let projection = migration
            .find("UPDATE organization_memberships")
            .expect("owner facts must be projected from the legacy role");
        let invariant = migration
            .find("active organization has no active owner")
            .expect("migration must reject an active Organization without an active owner");
        let destructive_drop = migration
            .find("DROP TABLE organization_roles")
            .expect("legacy role storage must be retired");

        assert!(projection < invariant);
        assert!(invariant < destructive_drop);
        assert!(migration.contains("membership.removed_at IS NULL"));
        assert!(migration.contains("membership.is_owner"));
    }

    #[test]
    fn creation_idempotency_is_caller_scoped_and_keeps_result_references() {
        let migration = MIGRATIONS[2].sql();

        assert!(migration.contains("PRIMARY KEY (caller_instance, idempotency_key)"));
        assert!(migration.contains("REFERENCES organizations(organization_id)"));
        assert!(migration.contains("REFERENCES organization_memberships(membership_id)"));
        assert!(migration.contains("DEFERRABLE INITIALLY DEFERRED"));
    }
}
