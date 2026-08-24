//! PostgreSQL-backed Organization behavior for Lenso vNext.

mod operator;
mod schema;

use std::{cell::RefCell, fmt, fmt::Write as _, rc::Rc, time::Duration};

use lenso_capability_organization_access::{
    CheckPermissionError, CheckPermissionRequest, CheckPermissionResponse, OrganizationAccess,
    OrganizationAccessEndpoint, OrganizationAccessProvider,
};
use lenso_capability_organization_admin::{
    CreateOrganizationError, CreateOrganizationRequest, CreateOrganizationResponse,
    OrganizationAdmin, OrganizationAdminEndpoint, OrganizationAdminProvider,
};
use lenso_capability_secrets::{ResolveRequest, SecretsClient, SecretsInvocationError};
use lenso_kernel::{
    DeactivateContext, InvocationContext, ModuleFuture, ModuleLifecycle, NativeRequestEndpoint,
    NativeRequestFuture, PrepareContext, RuntimeFailure,
};
use lenso_native_adapter::{NativeModuleFactory, NativeModuleFactoryContext, NativeModuleInstance};
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
const OWNER_PERMISSIONS: &[&str] = &[
    "organization.read",
    "organization.manage",
    "organization.members.manage",
    "organization.roles.manage",
    "organization.invitations.manage",
];

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

impl NativeModuleFactory for OrganizationFactory {
    fn package_id(&self) -> &'static str {
        PACKAGE_ID
    }

    fn package_version(&self) -> &'static str {
        PACKAGE_VERSION
    }

    fn instantiate(
        &self,
        context: NativeModuleFactoryContext<'_>,
    ) -> Result<NativeModuleInstance, RuntimeFailure> {
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
            Rc::new(OrganizationAccessEndpoint::new(provider)),
        ];
        Ok(NativeModuleInstance::with_lifecycle(
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
            .ok_or(RuntimeFailure::ModuleFailure {
                detail: "Organization Module is not prepared".to_owned(),
            })
    }

    fn admin_authorized(&self, context: &InvocationContext) -> bool {
        context
            .caller_instance()
            .is_some_and(|caller| self.admin_callers.iter().any(|allowed| allowed == caller))
    }
}

impl OrganizationAdminProvider for OrganizationProvider {
    fn create_organization(
        &self,
        context: InvocationContext,
        request: CreateOrganizationRequest,
    ) -> NativeRequestFuture<OrganizationAdmin> {
        let authorized = self.admin_authorized(&context);
        let prepared = self.prepared();
        Box::pin(async move {
            if !authorized {
                return Ok(Err(CreateOrganizationError::Forbidden));
            }
            if !valid_organization_name(&request.name)
                || !valid_slug(&request.slug)
                || !valid_name(&request.owner_subject, 256)
            {
                return Ok(Err(CreateOrganizationError::InvalidOrganization));
            }
            let prepared = prepared?;
            let organization_id = random_id("org_").map_err(runtime)?;
            let owner_role_id = random_id("role_").map_err(runtime)?;
            let owner_membership_id = random_id("member_").map_err(runtime)?;
            let permissions = OWNER_PERMISSIONS.to_vec();
            let mut transaction = prepared.postgres.pool().begin().await.map_err(|source| {
                runtime(OrganizationError::Database {
                    operation: "begin organization creation",
                    source,
                })
            })?;
            let inserted = sqlx::query(
                "INSERT INTO organizations (organization_id,name,slug) VALUES ($1,$2,$3)",
            )
            .bind(&organization_id)
            .bind(request.name.trim())
            .bind(&request.slug)
            .execute(&mut *transaction)
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
            sqlx::query("INSERT INTO organization_roles (role_id,organization_id,name,permissions,system_key) VALUES ($1,$2,'Owner',$3,'owner')")
                .bind(&owner_role_id)
                .bind(&organization_id)
                .bind(&permissions)
                .execute(&mut *transaction)
                .await
                .map_err(|source| runtime(OrganizationError::Database { operation: "insert owner role", source }))?;
            sqlx::query("INSERT INTO organization_memberships (membership_id,organization_id,subject,role_id) VALUES ($1,$2,$3,$4)")
                .bind(&owner_membership_id)
                .bind(&organization_id)
                .bind(&request.owner_subject)
                .bind(&owner_role_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| runtime(OrganizationError::Database { operation: "insert owner membership", source }))?;
            transaction.commit().await.map_err(|source| {
                runtime(OrganizationError::Database {
                    operation: "commit organization creation",
                    source,
                })
            })?;
            Ok(Ok(CreateOrganizationResponse {
                organization_id,
                owner_membership_id,
                owner_role_id,
            }))
        })
    }
}

impl OrganizationAccessProvider for OrganizationProvider {
    fn check_permission(
        &self,
        _context: InvocationContext,
        request: CheckPermissionRequest,
    ) -> NativeRequestFuture<OrganizationAccess> {
        let prepared = self.prepared();
        Box::pin(async move {
            if !valid_name(&request.organization_id, 256)
                || !valid_name(&request.subject, 256)
                || !valid_permission(&request.permission)
            {
                return Ok(Err(CheckPermissionError::InvalidRequest));
            }
            let prepared = prepared?;
            let row = sqlx::query(
                "SELECT EXISTS(SELECT 1 FROM organizations WHERE organization_id=$1 AND archived_at IS NULL) AS organization_exists, EXISTS(SELECT 1 FROM organization_memberships m JOIN organization_roles r ON r.role_id=m.role_id AND r.organization_id=m.organization_id WHERE m.organization_id=$1 AND m.subject=$2 AND m.removed_at IS NULL AND $3=ANY(r.permissions)) AS allowed",
            )
            .bind(&request.organization_id)
            .bind(&request.subject)
            .bind(&request.permission)
            .fetch_one(prepared.postgres.pool())
            .await
            .map_err(|source| runtime(OrganizationError::Database { operation: "check organization permission", source }))?;
            let organization_exists: bool =
                row.try_get("organization_exists").map_err(|source| {
                    runtime(OrganizationError::Database {
                        operation: "decode organization existence",
                        source,
                    })
                })?;
            if !organization_exists {
                return Ok(Err(CheckPermissionError::OrganizationNotFound));
            }
            let allowed = row.try_get("allowed").map_err(|source| {
                runtime(OrganizationError::Database {
                    operation: "decode organization permission",
                    source,
                })
            })?;
            Ok(Ok(CheckPermissionResponse { allowed }))
        })
    }
}

#[derive(Debug)]
struct OrganizationLifecycle {
    config: OrganizationConfig,
    state: Rc<RefCell<Option<PreparedOrganization>>>,
}

impl ModuleLifecycle for OrganizationLifecycle {
    fn prepare(&self, context: PrepareContext) -> ModuleFuture {
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
                    SecretsInvocationError::Domain(_) => RuntimeFailure::ModuleFailure {
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
            .map_err(|error| RuntimeFailure::ModuleFailure {
                detail: error.to_string(),
            })?;
            state.replace(Some(PreparedOrganization { postgres }));
            Ok(())
        })
    }

    fn deactivate(&self, _context: DeactivateContext) -> ModuleFuture {
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
    RuntimeFailure::ModuleFailure {
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

fn valid_permission(value: &str) -> bool {
    valid_name(value, 200) && value.contains('.')
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
    use sqlx::{AssertSqlSafe, Executor};

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
    async fn unprepared_access_reports_runtime_failure() {
        let provider = OrganizationProvider {
            state: Rc::new(RefCell::new(None)),
            admin_callers: vec!["business-admin".to_owned()],
        };
        let result = provider
            .check_permission(
                InvocationContext::new(1, None, CancellationToken::new()),
                CheckPermissionRequest {
                    organization_id: "org_missing".to_owned(),
                    permission: "organization.read".to_owned(),
                    subject: "usr_owner".to_owned(),
                },
            )
            .await;
        assert!(matches!(result, Err(RuntimeFailure::ModuleFailure { .. })));
    }

    #[tokio::test]
    #[ignore = "requires LENSO_POSTGRES_TEST_URL"]
    async fn create_then_check_permission_preserves_transaction_and_domain_errors() {
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
            admin_callers: vec!["business-admin".to_owned()],
        };
        let admin_context = InvocationContext::new(1, None, CancellationToken::new())
            .with_caller_instance("business-admin");
        let created = provider
            .create_organization(
                admin_context.clone(),
                CreateOrganizationRequest {
                    name: "Acme".to_owned(),
                    owner_subject: "usr_owner".to_owned(),
                    slug: "acme".to_owned(),
                },
            )
            .await
            .unwrap()
            .unwrap();
        let access = provider
            .check_permission(
                InvocationContext::new(2, None, CancellationToken::new()),
                CheckPermissionRequest {
                    organization_id: created.organization_id.clone(),
                    permission: "organization.members.manage".to_owned(),
                    subject: "usr_owner".to_owned(),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert!(access.allowed);

        let duplicate = provider
            .create_organization(
                admin_context,
                CreateOrganizationRequest {
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
}
