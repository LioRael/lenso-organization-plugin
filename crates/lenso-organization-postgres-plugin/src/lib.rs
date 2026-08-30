//! PostgreSQL-backed Organization behavior for Lenso vNext.

mod operator;
mod schema;

use std::{cell::RefCell, fmt, fmt::Write as _, rc::Rc, time::Duration};

use lenso_capability_organization_admin::{
    CreateOrganizationError, CreateOrganizationRequest, CreateOrganizationResponse,
    OrganizationAdmin, OrganizationAdminEndpoint, OrganizationAdminProvider,
};
use lenso_capability_organization_membership::{
    CheckMembershipError, CheckMembershipRequest, CheckMembershipResponse, OrganizationMembership,
    OrganizationMembershipEndpoint, OrganizationMembershipProvider,
};
use lenso_capability_secrets::{ResolveRequest, SecretsClient, SecretsInvocationError};
use lenso_kernel::{
    DeactivateContext, InvocationContext, NativeRequestEndpoint, NativeRequestFuture, PluginFuture,
    PluginLifecycle, PrepareContext, RuntimeFailure,
};
use lenso_native_adapter::{NativePluginFactory, NativePluginFactoryContext, NativePluginInstance};
use lenso_postgres_kit::OwnedPostgres;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::schema::schema_plan;

pub use operator::{OrganizationOperator, OrganizationOperatorError};

pub const PACKAGE_ID: &str = "lenso.organization.postgres";
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
const DEPENDENCY_TIMEOUT: Duration = Duration::from_secs(10);
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OrganizationConfig {
    schema: String,
    database_url_secret: String,
    #[serde(default)]
    admin_callers: Vec<String>,
}

impl OrganizationConfig {
    pub fn new(
        schema: impl Into<String>,
        database_url_secret: impl Into<String>,
        admin_callers: Vec<String>,
    ) -> Result<Self, OrganizationConfigError> {
        let value = Self {
            schema: schema.into(),
            database_url_secret: database_url_secret.into(),
            admin_callers,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), OrganizationConfigError> {
        schema_plan(self.schema.clone()).map_err(|_| OrganizationConfigError::InvalidSchema)?;
        if !valid_secret_reference(&self.database_url_secret) {
            return Err(OrganizationConfigError::InvalidSecretReference);
        }
        if self.admin_callers.is_empty()
            || self
                .admin_callers
                .iter()
                .any(|value| !valid_name(value, 256))
        {
            return Err(OrganizationConfigError::InvalidAdminCallers);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum OrganizationConfigError {
    #[error("invalid owned PostgreSQL schema")]
    InvalidSchema,
    #[error("invalid database URL secret reference")]
    InvalidSecretReference,
    #[error("at least one valid Organization Admin caller is required")]
    InvalidAdminCallers,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OrganizationFactory;

impl NativePluginFactory for OrganizationFactory {
    fn package_id(&self) -> &'static str {
        PACKAGE_ID
    }

    fn package_version(&self) -> &'static str {
        PACKAGE_VERSION
    }

    fn instantiate(
        &self,
        context: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        if context.entrypoint() != "default" {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: format!(
                    "unsupported Organization entrypoint `{}`",
                    context.entrypoint()
                ),
            });
        }
        let config: OrganizationConfig =
            serde_json::from_str(context.configuration()).map_err(|error| {
                RuntimeFailure::InvalidResolvedPlan {
                    detail: format!("Organization configuration is invalid: {error}"),
                }
            })?;
        config
            .validate()
            .map_err(|error| RuntimeFailure::InvalidResolvedPlan {
                detail: error.to_string(),
            })?;

        let state = Rc::new(RefCell::new(None));
        let provider = OrganizationProvider {
            state: state.clone(),
            admin_callers: config.admin_callers.clone(),
        };
        let endpoints: Vec<Rc<dyn NativeRequestEndpoint>> = vec![
            Rc::new(OrganizationAdminEndpoint::new(provider.clone())),
            Rc::new(OrganizationMembershipEndpoint::new(provider)),
        ];
        Ok(NativePluginInstance::with_lifecycle(
            endpoints,
            OrganizationLifecycle { config, state },
        ))
    }
}

#[derive(Clone)]
struct PreparedOrganization {
    postgres: OwnedPostgres,
}

impl fmt::Debug for PreparedOrganization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedOrganization")
            .field("schema", &self.postgres.schema())
            .finish()
    }
}

#[derive(Clone)]
struct OrganizationProvider {
    state: Rc<RefCell<Option<PreparedOrganization>>>,
    admin_callers: Vec<String>,
}

impl fmt::Debug for OrganizationProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OrganizationProvider")
            .field("prepared", &self.state.borrow().is_some())
            .field("admin_caller_count", &self.admin_callers.len())
            .finish()
    }
}

impl OrganizationProvider {
    fn prepared(&self) -> Result<PreparedOrganization, RuntimeFailure> {
        self.state
            .borrow()
            .clone()
            .ok_or(RuntimeFailure::PluginFailure {
                detail: "Organization Plugin is not prepared".to_owned(),
            })
    }

    fn authorized_admin_caller<'a>(&self, context: &'a InvocationContext) -> Option<&'a str> {
        context
            .caller_instance()
            .filter(|caller| self.admin_callers.iter().any(|allowed| allowed == *caller))
    }
}

impl OrganizationAdminProvider for OrganizationProvider {
    fn create_organization(
        &self,
        context: InvocationContext,
        request: CreateOrganizationRequest,
    ) -> NativeRequestFuture<OrganizationAdmin> {
        let caller_instance = self.authorized_admin_caller(&context).map(str::to_owned);
        let prepared = self.prepared();
        Box::pin(async move {
            let Some(caller_instance) = caller_instance else {
                return Ok(Err(CreateOrganizationError::Forbidden));
            };
            let name = request.name.trim().to_owned();
            if !valid_name(&request.idempotency_key, 256)
                || !valid_organization_name(&request.name)
                || !valid_slug(&request.slug)
                || !valid_name(&request.owner_subject, 256)
            {
                return Ok(Err(CreateOrganizationError::InvalidOrganization));
            }
            let prepared = prepared?;
            create_organization_in_postgres(prepared, caller_instance, request, name).await
        })
    }
}

async fn create_organization_in_postgres(
    prepared: PreparedOrganization,
    caller_instance: String,
    request: CreateOrganizationRequest,
    name: String,
) -> Result<Result<CreateOrganizationResponse, CreateOrganizationError>, RuntimeFailure> {
    let organization_id = random_id("org_").map_err(runtime)?;
    let owner_membership_id = random_id("member_").map_err(runtime)?;
    let mut transaction = prepared.postgres.pool().begin().await.map_err(|source| {
        runtime(OrganizationError::Database {
            operation: "begin organization creation",
            source,
        })
    })?;
    if !reserve_creation(
        &mut transaction,
        &caller_instance,
        &request,
        &name,
        &organization_id,
        &owner_membership_id,
    )
    .await?
    {
        let response = match read_creation_replay(
            &mut transaction,
            &caller_instance,
            &request,
            &name,
        )
        .await?
        {
            Ok(response) => response,
            Err(error) => return Ok(Err(error)),
        };
        transaction.commit().await.map_err(|source| {
            runtime(OrganizationError::Database {
                operation: "commit organization creation replay",
                source,
            })
        })?;
        return Ok(Ok(response));
    }
    if let Err(error) = insert_organization_and_owner(
        &mut transaction,
        &request,
        &name,
        &organization_id,
        &owner_membership_id,
    )
    .await?
    {
        return Ok(Err(error));
    }
    transaction.commit().await.map_err(|source| {
        runtime(OrganizationError::Database {
            operation: "commit organization creation",
            source,
        })
    })?;
    Ok(Ok(CreateOrganizationResponse {
        created: true,
        organization_id,
        owner_membership_id,
    }))
}

async fn reserve_creation(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    caller_instance: &str,
    request: &CreateOrganizationRequest,
    name: &str,
    organization_id: &str,
    owner_membership_id: &str,
) -> Result<bool, RuntimeFailure> {
    sqlx::query(
        "INSERT INTO organization_creation_requests (caller_instance,idempotency_key,name,slug,owner_subject,organization_id,owner_membership_id) VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (caller_instance,idempotency_key) DO NOTHING",
    )
    .bind(caller_instance)
    .bind(&request.idempotency_key)
    .bind(name)
    .bind(&request.slug)
    .bind(&request.owner_subject)
    .bind(organization_id)
    .bind(owner_membership_id)
    .execute(&mut **transaction)
    .await
    .map(|result| result.rows_affected() == 1)
    .map_err(|source| {
        runtime(OrganizationError::Database {
            operation: "reserve organization creation",
            source,
        })
    })
}

async fn read_creation_replay(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    caller_instance: &str,
    request: &CreateOrganizationRequest,
    name: &str,
) -> Result<Result<CreateOrganizationResponse, CreateOrganizationError>, RuntimeFailure> {
    let (stored_name, stored_slug, stored_owner, organization_id, owner_membership_id): (
        String,
        String,
        String,
        String,
        String,
    ) = sqlx::query_as(
        "SELECT name,slug,owner_subject,organization_id,owner_membership_id FROM organization_creation_requests WHERE caller_instance=$1 AND idempotency_key=$2",
    )
    .bind(caller_instance)
    .bind(&request.idempotency_key)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|source| {
        runtime(OrganizationError::Database {
            operation: "read organization creation replay",
            source,
        })
    })?;
    if stored_name != name || stored_slug != request.slug || stored_owner != request.owner_subject {
        return Ok(Err(CreateOrganizationError::IdempotencyConflict));
    }
    Ok(Ok(CreateOrganizationResponse {
        created: false,
        organization_id,
        owner_membership_id,
    }))
}

async fn insert_organization_and_owner(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request: &CreateOrganizationRequest,
    name: &str,
    organization_id: &str,
    owner_membership_id: &str,
) -> Result<Result<(), CreateOrganizationError>, RuntimeFailure> {
    let inserted =
        sqlx::query("INSERT INTO organizations (organization_id,name,slug) VALUES ($1,$2,$3)")
            .bind(organization_id)
            .bind(name)
            .bind(&request.slug)
            .execute(&mut **transaction)
            .await;
    if let Err(error) = inserted {
        if error
            .as_database_error()
            .and_then(|database| database.constraint())
            == Some("organizations_active_slug_key")
        {
            return Ok(Err(CreateOrganizationError::SlugConflict));
        }
        return Err(runtime(OrganizationError::Database {
            operation: "insert organization",
            source: error,
        }));
    }
    sqlx::query("INSERT INTO organization_memberships (membership_id,organization_id,subject,is_owner) VALUES ($1,$2,$3,true)")
        .bind(owner_membership_id)
        .bind(organization_id)
        .bind(&request.owner_subject)
        .execute(&mut **transaction)
        .await
        .map_err(|source| runtime(OrganizationError::Database { operation: "insert owner membership", source }))?;
    Ok(Ok(()))
}

impl OrganizationMembershipProvider for OrganizationProvider {
    fn check_membership(
        &self,
        _context: InvocationContext,
        request: CheckMembershipRequest,
    ) -> NativeRequestFuture<OrganizationMembership> {
        let prepared = self.prepared();
        Box::pin(async move {
            if !valid_name(&request.organization_id, 256) || !valid_name(&request.subject, 256) {
                return Ok(Err(CheckMembershipError::InvalidRequest));
            }
            let prepared = prepared?;
            let row = sqlx::query(
                "SELECT EXISTS(SELECT 1 FROM organizations WHERE organization_id=$1 AND archived_at IS NULL) AS organization_exists, COALESCE((SELECT removed_at IS NULL FROM organization_memberships WHERE organization_id=$1 AND subject=$2 ORDER BY created_at DESC LIMIT 1),false) AS active, COALESCE((SELECT is_owner AND removed_at IS NULL FROM organization_memberships WHERE organization_id=$1 AND subject=$2 ORDER BY created_at DESC LIMIT 1),false) AS owner",
            )
            .bind(&request.organization_id)
            .bind(&request.subject)
            .fetch_one(prepared.postgres.pool())
            .await
            .map_err(|source| runtime(OrganizationError::Database { operation: "check organization membership", source }))?;
            let organization_exists: bool =
                row.try_get("organization_exists").map_err(|source| {
                    runtime(OrganizationError::Database {
                        operation: "decode organization existence",
                        source,
                    })
                })?;
            if !organization_exists {
                return Ok(Err(CheckMembershipError::OrganizationNotFound));
            }
            let active = row.try_get("active").map_err(|source| {
                runtime(OrganizationError::Database {
                    operation: "decode organization membership",
                    source,
                })
            })?;
            let owner = row.try_get("owner").map_err(|source| {
                runtime(OrganizationError::Database {
                    operation: "decode organization ownership",
                    source,
                })
            })?;
            Ok(Ok(CheckMembershipResponse { active, owner }))
        })
    }
}

#[derive(Debug)]
struct OrganizationLifecycle {
    config: OrganizationConfig,
    state: Rc<RefCell<Option<PreparedOrganization>>>,
}

impl PluginLifecycle for OrganizationLifecycle {
    fn prepare(&self, context: PrepareContext) -> PluginFuture {
        let config = self.config.clone();
        let state = self.state.clone();
        let dependencies = context.dependencies().clone();
        let cancellation = context.cancellation();
        Box::pin(async move {
            let secrets = SecretsClient::from_dependencies(&dependencies)?;
            let invocation =
                dependencies.invocation_context_after(DEPENDENCY_TIMEOUT, cancellation)?;
            let database_url = secrets
                .resolve_with_context(
                    invocation,
                    ResolveRequest {
                        reference: config.database_url_secret.clone(),
                    },
                )
                .await
                .map_err(|error| match error {
                    SecretsInvocationError::Domain(_) => RuntimeFailure::PluginFailure {
                        detail: format!(
                            "database URL secret `{}` was rejected",
                            config.database_url_secret
                        ),
                    },
                    SecretsInvocationError::Runtime(error) => error,
                })?;
            let database_url = Zeroizing::new(database_url.value);
            let postgres = OwnedPostgres::prepare(
                &database_url,
                schema_plan(config.schema).map_err(|error| {
                    RuntimeFailure::InvalidResolvedPlan {
                        detail: error.to_string(),
                    }
                })?,
            )
            .await
            .map_err(|error| RuntimeFailure::PluginFailure {
                detail: error.to_string(),
            })?;
            state.replace(Some(PreparedOrganization { postgres }));
            Ok(())
        })
    }

    fn deactivate(&self, _context: DeactivateContext) -> PluginFuture {
        let prepared = self.state.borrow_mut().take();
        Box::pin(async move {
            if let Some(prepared) = prepared {
                prepared.postgres.pool().close().await;
            }
            Ok(())
        })
    }
}

#[derive(Debug, Error)]
enum OrganizationError {
    #[error("PostgreSQL operation `{operation}` failed")]
    Database {
        operation: &'static str,
        #[source]
        source: sqlx::Error,
    },
    #[error("random source unavailable")]
    Random,
}

fn runtime(error: impl fmt::Display) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: error.to_string(),
    }
}

fn random_id(prefix: &str) -> Result<String, OrganizationError> {
    let mut bytes = [0_u8; 18];
    getrandom::fill(&mut bytes).map_err(|_| OrganizationError::Random)?;
    let mut id = String::with_capacity(prefix.len() + bytes.len() * 2);
    id.push_str(prefix);
    for byte in bytes {
        write!(&mut id, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(id)
}

fn valid_organization_name(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && value.len() <= 200 && !value.chars().any(char::is_control)
}

fn valid_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

fn valid_name(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

fn valid_secret_reference(reference: &str) -> bool {
    !reference.is_empty()
        && reference.len() <= 256
        && !reference.starts_with('/')
        && !reference.ends_with('/')
        && !reference.contains("//")
        && reference
            .split('/')
            .all(|segment| segment != "." && segment != "..")
        && reference
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lenso_kernel::CancellationToken;
    use lenso_postgres_kit::{Migration, SchemaOperator, SchemaPlan};
    use sqlx::{AssertSqlSafe, Executor};

    const LEGACY_MIGRATIONS: &[Migration] = &[Migration::new(
        1,
        "create-organizations",
        include_str!("../migrations/001_create_organizations.sql"),
    )];

    fn legacy_schema_plan(schema: impl Into<std::sync::Arc<str>>) -> SchemaPlan {
        SchemaPlan::new(schema, LEGACY_MIGRATIONS).unwrap()
    }

    #[test]
    fn configuration_rejects_ambient_admin_authority() {
        let error = OrganizationConfig::new("organization", "organization/database", Vec::new())
            .unwrap_err();
        assert_eq!(error, OrganizationConfigError::InvalidAdminCallers);
    }

    #[test]
    fn slugs_are_stable_and_narrow() {
        assert!(valid_slug("acme-platform"));
        assert!(!valid_slug("Acme Platform"));
        assert!(!valid_slug("-acme"));
    }

    #[test]
    fn generated_ids_do_not_repeat() {
        assert_ne!(random_id("org_").unwrap(), random_id("org_").unwrap());
    }

    #[tokio::test]
    async fn forbidden_admin_is_a_domain_error_before_storage_access() {
        let provider = OrganizationProvider {
            state: Rc::new(RefCell::new(None)),
            admin_callers: vec!["business-admin".to_owned()],
        };
        let context = InvocationContext::new(1, None, CancellationToken::new())
            .with_caller_instance("untrusted");
        let result = provider
            .create_organization(
                context,
                CreateOrganizationRequest {
                    idempotency_key: "forbidden-create".to_owned(),
                    name: "Acme".to_owned(),
                    owner_subject: "usr_owner".to_owned(),
                    slug: "acme".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(result, Err(CreateOrganizationError::Forbidden));
    }

    #[tokio::test]
    async fn invalid_idempotency_key_is_rejected_before_storage_access() {
        let provider = OrganizationProvider {
            state: Rc::new(RefCell::new(None)),
            admin_callers: vec!["business-admin".to_owned()],
        };
        let context = InvocationContext::new(1, None, CancellationToken::new())
            .with_caller_instance("business-admin");
        let result = provider
            .create_organization(
                context,
                CreateOrganizationRequest {
                    idempotency_key: String::new(),
                    name: "Acme".to_owned(),
                    owner_subject: "usr_owner".to_owned(),
                    slug: "acme".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(result, Err(CreateOrganizationError::InvalidOrganization));
    }

    #[tokio::test]
    async fn unprepared_membership_reports_runtime_failure() {
        let provider = OrganizationProvider {
            state: Rc::new(RefCell::new(None)),
            admin_callers: vec!["business-admin".to_owned()],
        };
        let result = provider
            .check_membership(
                InvocationContext::new(1, None, CancellationToken::new()),
                CheckMembershipRequest {
                    organization_id: "org_missing".to_owned(),
                    subject: "usr_owner".to_owned(),
                },
            )
            .await;
        assert!(matches!(result, Err(RuntimeFailure::PluginFailure { .. })));
    }

    #[tokio::test]
    #[ignore = "requires LENSO_POSTGRES_TEST_URL"]
    #[allow(
        clippy::too_many_lines,
        reason = "the acceptance scenario keeps concurrent creation and every replay boundary together"
    )]
    async fn concurrent_create_is_caller_scoped_idempotent_and_preserves_ownership() {
        let database_url =
            std::env::var("LENSO_POSTGRES_TEST_URL").expect("LENSO_POSTGRES_TEST_URL is required");
        let schema = random_id("organization_test_").unwrap();
        OrganizationOperator::setup(&database_url, &schema)
            .await
            .unwrap();
        let postgres = OwnedPostgres::prepare(&database_url, schema_plan(schema.clone()).unwrap())
            .await
            .unwrap();
        let provider = OrganizationProvider {
            state: Rc::new(RefCell::new(Some(PreparedOrganization { postgres }))),
            admin_callers: vec!["business-admin".to_owned(), "second-admin".to_owned()],
        };
        let admin_context = InvocationContext::new(1, None, CancellationToken::new())
            .with_caller_instance("business-admin");
        let create_request = CreateOrganizationRequest {
            idempotency_key: "create-acme".to_owned(),
            name: "Acme".to_owned(),
            owner_subject: "usr_owner".to_owned(),
            slug: "acme".to_owned(),
        };
        let first_creation =
            provider.create_organization(admin_context.clone(), create_request.clone());
        let concurrent_replay = provider.create_organization(
            InvocationContext::new(2, None, CancellationToken::new())
                .with_caller_instance("business-admin"),
            create_request.clone(),
        );
        let (first_creation, concurrent_replay) = tokio::join!(first_creation, concurrent_replay);
        let first_creation = first_creation.unwrap().unwrap();
        let concurrent_replay = concurrent_replay.unwrap().unwrap();
        assert_ne!(first_creation.created, concurrent_replay.created);
        assert_eq!(
            first_creation.organization_id,
            concurrent_replay.organization_id
        );
        assert_eq!(
            first_creation.owner_membership_id,
            concurrent_replay.owner_membership_id
        );
        let created = if first_creation.created {
            first_creation
        } else {
            concurrent_replay
        };
        let membership = provider
            .check_membership(
                InvocationContext::new(3, None, CancellationToken::new()),
                CheckMembershipRequest {
                    organization_id: created.organization_id.clone(),
                    subject: "usr_owner".to_owned(),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert!(membership.active);
        assert!(membership.owner);

        let replay = provider
            .create_organization(admin_context.clone(), create_request.clone())
            .await
            .unwrap()
            .unwrap();
        assert!(!replay.created);
        assert_eq!(replay.organization_id, created.organization_id);
        assert_eq!(replay.owner_membership_id, created.owner_membership_id);

        let conflict = provider
            .create_organization(
                admin_context.clone(),
                CreateOrganizationRequest {
                    owner_subject: "usr_other".to_owned(),
                    ..create_request
                },
            )
            .await
            .unwrap();
        assert_eq!(conflict, Err(CreateOrganizationError::IdempotencyConflict));

        let second_caller = provider
            .create_organization(
                InvocationContext::new(4, None, CancellationToken::new())
                    .with_caller_instance("second-admin"),
                CreateOrganizationRequest {
                    idempotency_key: "create-acme".to_owned(),
                    name: "Second".to_owned(),
                    owner_subject: "usr_second".to_owned(),
                    slug: "second".to_owned(),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert!(second_caller.created);
        assert_ne!(second_caller.organization_id, created.organization_id);

        let duplicate = provider
            .create_organization(
                admin_context,
                CreateOrganizationRequest {
                    idempotency_key: "create-another-acme".to_owned(),
                    name: "Another Acme".to_owned(),
                    owner_subject: "usr_other".to_owned(),
                    slug: "acme".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(duplicate, Err(CreateOrganizationError::SlugConflict));

        let cleanup_pool = sqlx::PgPool::connect(&database_url).await.unwrap();
        cleanup_pool
            .execute(AssertSqlSafe(format!("DROP SCHEMA \"{schema}\" CASCADE")))
            .await
            .unwrap();
        cleanup_pool.close().await;
    }

    #[tokio::test]
    #[ignore = "requires LENSO_POSTGRES_TEST_URL"]
    async fn upgrade_projects_legacy_owner_and_rejects_an_ownerless_active_organization() {
        let database_url =
            std::env::var("LENSO_POSTGRES_TEST_URL").expect("LENSO_POSTGRES_TEST_URL is required");
        let schema = random_id("organization_upgrade_test_").unwrap();
        SchemaOperator::connect(&database_url, legacy_schema_plan(schema.clone()))
            .await
            .unwrap()
            .setup()
            .await
            .unwrap();
        let legacy = OwnedPostgres::prepare(&database_url, legacy_schema_plan(schema.clone()))
            .await
            .unwrap();

        sqlx::query("INSERT INTO organizations (organization_id,name,slug) VALUES ('org_good','Good','good'),('org_bad','Bad','bad')")
            .execute(legacy.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO organization_roles (role_id,organization_id,name,permissions,system_key) VALUES ('role_good','org_good','Owner',ARRAY['organization.read'],'owner'),('role_bad','org_bad','Member',ARRAY['organization.read'],NULL)")
            .execute(legacy.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO organization_memberships (membership_id,organization_id,subject,role_id) VALUES ('membership_good','org_good','usr_good','role_good'),('membership_bad','org_bad','usr_bad','role_bad')")
            .execute(legacy.pool())
            .await
            .unwrap();

        assert!(
            OrganizationOperator::upgrade(&database_url, &schema)
                .await
                .is_err()
        );
        let legacy_roles_remain: bool =
            sqlx::query_scalar("SELECT to_regclass('organization_roles') IS NOT NULL")
                .fetch_one(legacy.pool())
                .await
                .unwrap();
        assert!(legacy_roles_remain);

        sqlx::query("UPDATE organization_roles SET system_key='owner' WHERE role_id='role_bad'")
            .execute(legacy.pool())
            .await
            .unwrap();
        legacy.pool().close().await;
        OrganizationOperator::upgrade(&database_url, &schema)
            .await
            .unwrap();
        let upgraded = OwnedPostgres::prepare(&database_url, schema_plan(schema.clone()).unwrap())
            .await
            .unwrap();
        let owner_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM organization_memberships WHERE removed_at IS NULL AND is_owner",
        )
        .fetch_one(upgraded.pool())
        .await
        .unwrap();
        assert_eq!(owner_count, 2);
        let legacy_roles_remain: bool =
            sqlx::query_scalar("SELECT to_regclass('organization_roles') IS NOT NULL")
                .fetch_one(upgraded.pool())
                .await
                .unwrap();
        assert!(!legacy_roles_remain);
        let creation_receipts_exist: bool =
            sqlx::query_scalar("SELECT to_regclass('organization_creation_requests') IS NOT NULL")
                .fetch_one(upgraded.pool())
                .await
                .unwrap();
        assert!(creation_receipts_exist);

        upgraded.pool().close().await;
        let cleanup_pool = sqlx::PgPool::connect(&database_url).await.unwrap();
        cleanup_pool
            .execute(AssertSqlSafe(format!("DROP SCHEMA \"{schema}\" CASCADE")))
            .await
            .unwrap();
        cleanup_pool.close().await;
    }
}
