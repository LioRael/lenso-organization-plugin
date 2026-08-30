//! PostgreSQL-backed Organization behavior for Lenso vNext.

mod operator;
mod schema;

use std::{cell::RefCell, fmt, fmt::Write as _, rc::Rc, time::Duration};

use lenso_capability_organization_admin::{
    CreateOrganizationError, CreateOrganizationRequest, CreateOrganizationResponse,
    OrganizationAdmin, OrganizationAdminEndpoint, OrganizationAdminProvider,
};
use lenso_capability_organization_directory::{
    GetOrganizationError, GetOrganizationRequest, GetOrganizationResponse, OrganizationDirectory,
    OrganizationDirectoryEndpoint, OrganizationDirectoryProvider,
};
use lenso_capability_organization_membership::{
    CheckMembershipError, CheckMembershipRequest, CheckMembershipResponse, OrganizationMembership,
    OrganizationMembershipEndpoint, OrganizationMembershipProvider,
};
use lenso_capability_organization_membership_admin::{
    AddMemberError, AddMemberRequest, AddMemberResponse, OrganizationMembershipAdminAddMember,
    OrganizationMembershipAdminEndpoint, OrganizationMembershipAdminProvider,
    OrganizationMembershipAdminRemoveMember, RemoveMemberError, RemoveMemberRequest,
    RemoveMemberResponse,
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
    #[serde(default)]
    directory_callers: Vec<String>,
    #[serde(default)]
    membership_admin_callers: Vec<String>,
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
            directory_callers: Vec::new(),
            membership_admin_callers: Vec::new(),
        };
        value.validate()?;
        Ok(value)
    }

    pub fn with_membership_admin_callers(
        mut self,
        membership_admin_callers: Vec<String>,
    ) -> Result<Self, OrganizationConfigError> {
        self.membership_admin_callers = membership_admin_callers;
        self.validate()?;
        Ok(self)
    }

    pub fn with_directory_callers(
        mut self,
        directory_callers: Vec<String>,
    ) -> Result<Self, OrganizationConfigError> {
        self.directory_callers = directory_callers;
        self.validate()?;
        Ok(self)
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
        if self
            .directory_callers
            .iter()
            .any(|value| !valid_name(value, 256))
        {
            return Err(OrganizationConfigError::InvalidDirectoryCallers);
        }
        if self
            .membership_admin_callers
            .iter()
            .any(|value| !valid_name(value, 256))
        {
            return Err(OrganizationConfigError::InvalidMembershipAdminCallers);
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
    #[error("every Organization Directory caller must be a valid Instance key")]
    InvalidDirectoryCallers,
    #[error("every Organization Membership Admin caller must be a valid Instance key")]
    InvalidMembershipAdminCallers,
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
            directory_callers: config.directory_callers.clone(),
            membership_admin_callers: config.membership_admin_callers.clone(),
        };
        let endpoints: Vec<Rc<dyn NativeRequestEndpoint>> = vec![
            Rc::new(OrganizationAdminEndpoint::new(provider.clone())),
            Rc::new(OrganizationDirectoryEndpoint::new(provider.clone())),
            Rc::new(OrganizationMembershipEndpoint::new(provider.clone())),
            Rc::new(OrganizationMembershipAdminEndpoint::new(provider)),
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
    directory_callers: Vec<String>,
    membership_admin_callers: Vec<String>,
}

impl fmt::Debug for OrganizationProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OrganizationProvider")
            .field("prepared", &self.state.borrow().is_some())
            .field("admin_caller_count", &self.admin_callers.len())
            .field("directory_caller_count", &self.directory_callers.len())
            .field(
                "membership_admin_caller_count",
                &self.membership_admin_callers.len(),
            )
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

    fn authorized_membership_admin_caller<'a>(
        &self,
        context: &'a InvocationContext,
    ) -> Option<&'a str> {
        context.caller_instance().filter(|caller| {
            self.membership_admin_callers
                .iter()
                .any(|allowed| allowed == *caller)
        })
    }

    fn authorized_directory_caller<'a>(&self, context: &'a InvocationContext) -> Option<&'a str> {
        context.caller_instance().filter(|caller| {
            self.directory_callers
                .iter()
                .any(|allowed| allowed == *caller)
        })
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

impl OrganizationMembershipAdminProvider for OrganizationProvider {
    fn add_member(
        &self,
        context: InvocationContext,
        request: AddMemberRequest,
    ) -> NativeRequestFuture<OrganizationMembershipAdminAddMember> {
        let caller_instance = self
            .authorized_membership_admin_caller(&context)
            .map(str::to_owned);
        let prepared = self.prepared();
        Box::pin(async move {
            let Some(caller_instance) = caller_instance else {
                return Ok(Err(AddMemberError::Forbidden));
            };
            if !valid_membership_request(
                &request.idempotency_key,
                &request.organization_id,
                &request.subject,
            ) {
                return Ok(Err(AddMemberError::InvalidRequest));
            }
            let prepared = prepared?;
            add_member_in_postgres(prepared, caller_instance, request).await
        })
    }

    fn remove_member(
        &self,
        context: InvocationContext,
        request: RemoveMemberRequest,
    ) -> NativeRequestFuture<OrganizationMembershipAdminRemoveMember> {
        let caller_instance = self
            .authorized_membership_admin_caller(&context)
            .map(str::to_owned);
        let prepared = self.prepared();
        Box::pin(async move {
            let Some(caller_instance) = caller_instance else {
                return Ok(Err(RemoveMemberError::Forbidden));
            };
            if !valid_membership_request(
                &request.idempotency_key,
                &request.organization_id,
                &request.subject,
            ) {
                return Ok(Err(RemoveMemberError::InvalidRequest));
            }
            let prepared = prepared?;
            remove_member_in_postgres(prepared, caller_instance, request).await
        })
    }
}

impl OrganizationDirectoryProvider for OrganizationProvider {
    fn get_organization(
        &self,
        context: InvocationContext,
        request: GetOrganizationRequest,
    ) -> NativeRequestFuture<OrganizationDirectory> {
        let authorized = self.authorized_directory_caller(&context).is_some();
        let prepared = self.prepared();
        Box::pin(async move {
            if !authorized {
                return Ok(Err(GetOrganizationError::Forbidden));
            }
            if !valid_name(&request.organization_id, 256) {
                return Ok(Err(GetOrganizationError::InvalidRequest));
            }
            let prepared = prepared?;
            let row: Option<(String, String, bool, i64)> = sqlx::query_as(
                "SELECT name,slug,archived_at IS NULL,revision FROM organizations WHERE organization_id=$1",
            )
            .bind(&request.organization_id)
            .fetch_optional(prepared.postgres.pool())
            .await
            .map_err(|source| {
                runtime(OrganizationError::Database {
                    operation: "get organization directory entry",
                    source,
                })
            })?;
            let Some((name, slug, active, revision)) = row else {
                return Ok(Err(GetOrganizationError::OrganizationNotFound));
            };
            Ok(Ok(GetOrganizationResponse {
                active,
                name,
                organization_id: request.organization_id,
                revision: revision.to_string(),
                slug,
            }))
        })
    }
}

#[derive(Debug)]
enum MembershipCommandReplay {
    Exact {
        membership_id: String,
        revision: i64,
    },
    Conflict,
}

async fn add_member_in_postgres(
    prepared: PreparedOrganization,
    caller_instance: String,
    request: AddMemberRequest,
) -> Result<Result<AddMemberResponse, AddMemberError>, RuntimeFailure> {
    let generated_membership_id = random_id("member_").map_err(runtime)?;
    let mut transaction = prepared
        .postgres
        .pool()
        .begin()
        .await
        .map_err(|source| database_runtime("begin member addition", source))?;
    let reserved = reserve_membership_command(
        &mut transaction,
        &caller_instance,
        &request.idempotency_key,
        "add_member",
        &request.organization_id,
        &request.subject,
    )
    .await?;
    if !reserved {
        let replay = read_membership_command_replay(
            &mut transaction,
            &caller_instance,
            &request.idempotency_key,
            "add_member",
            &request.organization_id,
            &request.subject,
        )
        .await?;
        let MembershipCommandReplay::Exact {
            membership_id,
            revision,
        } = replay
        else {
            return Ok(Err(AddMemberError::IdempotencyConflict));
        };
        transaction
            .commit()
            .await
            .map_err(|source| database_runtime("commit member addition replay", source))?;
        return Ok(Ok(AddMemberResponse {
            created: false,
            membership_id,
            revision: revision.to_string(),
        }));
    }
    if !lock_active_organization(&mut transaction, &request.organization_id).await? {
        return Ok(Err(AddMemberError::OrganizationNotFound));
    }
    let existing: Option<(String, i64)> = sqlx::query_as(
        "SELECT membership_id,revision FROM organization_memberships WHERE organization_id=$1 AND subject=$2 AND removed_at IS NULL FOR UPDATE",
    )
    .bind(&request.organization_id)
    .bind(&request.subject)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|source| {
        runtime(OrganizationError::Database {
            operation: "read active member for addition",
            source,
        })
    })?;
    let (membership_id, revision, created) = if let Some((membership_id, revision)) = existing {
        (membership_id, revision, false)
    } else {
        sqlx::query(
            "INSERT INTO organization_memberships (membership_id,organization_id,subject,is_owner,revision) VALUES ($1,$2,$3,false,1)",
        )
        .bind(&generated_membership_id)
        .bind(&request.organization_id)
        .bind(&request.subject)
        .execute(&mut *transaction)
        .await
        .map_err(|source| {
            runtime(OrganizationError::Database {
                operation: "insert organization member",
                source,
            })
        })?;
        (generated_membership_id, 1, true)
    };
    complete_membership_command(
        &mut transaction,
        &caller_instance,
        &request.idempotency_key,
        &membership_id,
        revision,
        created,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|source| database_runtime("commit member addition", source))?;
    Ok(Ok(AddMemberResponse {
        created,
        membership_id,
        revision: revision.to_string(),
    }))
}

async fn remove_member_in_postgres(
    prepared: PreparedOrganization,
    caller_instance: String,
    request: RemoveMemberRequest,
) -> Result<Result<RemoveMemberResponse, RemoveMemberError>, RuntimeFailure> {
    let mut transaction = prepared
        .postgres
        .pool()
        .begin()
        .await
        .map_err(|source| database_runtime("begin member removal", source))?;
    let reserved = reserve_membership_command(
        &mut transaction,
        &caller_instance,
        &request.idempotency_key,
        "remove_member",
        &request.organization_id,
        &request.subject,
    )
    .await?;
    if !reserved {
        let replay = read_membership_command_replay(
            &mut transaction,
            &caller_instance,
            &request.idempotency_key,
            "remove_member",
            &request.organization_id,
            &request.subject,
        )
        .await?;
        let MembershipCommandReplay::Exact {
            membership_id,
            revision,
        } = replay
        else {
            return Ok(Err(RemoveMemberError::IdempotencyConflict));
        };
        transaction
            .commit()
            .await
            .map_err(|source| database_runtime("commit member removal replay", source))?;
        return Ok(Ok(RemoveMemberResponse {
            membership_id,
            removed: false,
            revision: revision.to_string(),
        }));
    }
    if !lock_active_organization(&mut transaction, &request.organization_id).await? {
        return Ok(Err(RemoveMemberError::OrganizationNotFound));
    }
    let existing: Option<(String, bool, i64)> = sqlx::query_as(
        "SELECT membership_id,is_owner,revision FROM organization_memberships WHERE organization_id=$1 AND subject=$2 AND removed_at IS NULL FOR UPDATE",
    )
    .bind(&request.organization_id)
    .bind(&request.subject)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|source| {
        runtime(OrganizationError::Database {
            operation: "read active member for removal",
            source,
        })
    })?;
    let Some((membership_id, is_owner, revision)) = existing else {
        return Ok(Err(RemoveMemberError::MembershipNotFound));
    };
    if is_owner {
        return Ok(Err(RemoveMemberError::OwnerProtected));
    }
    let next_revision = next_membership_revision(revision)?;
    sqlx::query(
        "UPDATE organization_memberships SET removed_at=transaction_timestamp(),updated_at=transaction_timestamp(),revision=$3 WHERE organization_id=$1 AND membership_id=$2 AND removed_at IS NULL",
    )
    .bind(&request.organization_id)
    .bind(&membership_id)
    .bind(next_revision)
    .execute(&mut *transaction)
    .await
    .map_err(|source| {
        runtime(OrganizationError::Database {
            operation: "remove organization member",
            source,
        })
    })?;
    complete_membership_command(
        &mut transaction,
        &caller_instance,
        &request.idempotency_key,
        &membership_id,
        next_revision,
        true,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|source| database_runtime("commit member removal", source))?;
    Ok(Ok(RemoveMemberResponse {
        membership_id,
        removed: true,
        revision: next_revision.to_string(),
    }))
}

async fn reserve_membership_command(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    caller_instance: &str,
    idempotency_key: &str,
    operation: &str,
    organization_id: &str,
    subject: &str,
) -> Result<bool, RuntimeFailure> {
    sqlx::query(
        "INSERT INTO organization_membership_commands (caller_instance,idempotency_key,operation,organization_id,subject) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (caller_instance,idempotency_key) DO NOTHING",
    )
    .bind(caller_instance)
    .bind(idempotency_key)
    .bind(operation)
    .bind(organization_id)
    .bind(subject)
    .execute(&mut **transaction)
    .await
    .map(|result| result.rows_affected() == 1)
    .map_err(|source| {
        runtime(OrganizationError::Database {
            operation: "reserve membership command",
            source,
        })
    })
}

async fn read_membership_command_replay(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    caller_instance: &str,
    idempotency_key: &str,
    operation: &str,
    organization_id: &str,
    subject: &str,
) -> Result<MembershipCommandReplay, RuntimeFailure> {
    let row: (
        String,
        String,
        String,
        Option<String>,
        Option<i64>,
        Option<bool>,
    ) = sqlx::query_as(
        "SELECT operation,organization_id,subject,membership_id,result_revision,changed FROM organization_membership_commands WHERE caller_instance=$1 AND idempotency_key=$2 FOR UPDATE",
    )
    .bind(caller_instance)
    .bind(idempotency_key)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|source| {
        runtime(OrganizationError::Database {
            operation: "read membership command replay",
            source,
        })
    })?;
    if row.0 != operation || row.1 != organization_id || row.2 != subject {
        return Ok(MembershipCommandReplay::Conflict);
    }
    let (Some(membership_id), Some(revision), Some(_changed)) = (row.3, row.4, row.5) else {
        return Err(runtime(OrganizationError::Invariant {
            detail: "committed membership command has no result",
        }));
    };
    Ok(MembershipCommandReplay::Exact {
        membership_id,
        revision,
    })
}

async fn complete_membership_command(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    caller_instance: &str,
    idempotency_key: &str,
    membership_id: &str,
    revision: i64,
    changed: bool,
) -> Result<(), RuntimeFailure> {
    sqlx::query(
        "UPDATE organization_membership_commands SET membership_id=$3,result_revision=$4,changed=$5,completed_at=transaction_timestamp() WHERE caller_instance=$1 AND idempotency_key=$2 AND completed_at IS NULL",
    )
    .bind(caller_instance)
    .bind(idempotency_key)
    .bind(membership_id)
    .bind(revision)
    .bind(changed)
    .execute(&mut **transaction)
    .await
    .and_then(|result| {
        if result.rows_affected() == 1 {
            Ok(result)
        } else {
            Err(sqlx::Error::RowNotFound)
        }
    })
    .map(|_| ())
    .map_err(|source| {
        runtime(OrganizationError::Database {
            operation: "complete membership command",
            source,
        })
    })
}

async fn lock_active_organization(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    organization_id: &str,
) -> Result<bool, RuntimeFailure> {
    sqlx::query_scalar::<_, bool>(
        "SELECT archived_at IS NULL FROM organizations WHERE organization_id=$1 FOR UPDATE",
    )
    .bind(organization_id)
    .fetch_optional(&mut **transaction)
    .await
    .map(|value| value.unwrap_or(false))
    .map_err(|source| {
        runtime(OrganizationError::Database {
            operation: "lock organization for membership command",
            source,
        })
    })
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
    #[error("Organization invariant failed: {detail}")]
    Invariant { detail: &'static str },
}

fn runtime(error: impl fmt::Display) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: error.to_string(),
    }
}

fn database_runtime(operation: &'static str, source: sqlx::Error) -> RuntimeFailure {
    runtime(OrganizationError::Database { operation, source })
}

fn next_membership_revision(revision: i64) -> Result<i64, RuntimeFailure> {
    revision.checked_add(1).ok_or_else(|| {
        runtime(OrganizationError::Invariant {
            detail: "membership revision overflow",
        })
    })
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

fn valid_membership_request(idempotency_key: &str, organization_id: &str, subject: &str) -> bool {
    valid_name(idempotency_key, 256) && valid_name(organization_id, 256) && valid_name(subject, 256)
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
    fn configuration_rejects_invalid_directory_caller_keys() {
        let error = OrganizationConfig::new(
            "organization",
            "organization/database",
            vec!["business-admin".to_owned()],
        )
        .unwrap()
        .with_directory_callers(vec!["invalid caller".to_owned()])
        .unwrap_err();
        assert_eq!(error, OrganizationConfigError::InvalidDirectoryCallers);
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
            directory_callers: Vec::new(),
            membership_admin_callers: Vec::new(),
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
            directory_callers: Vec::new(),
            membership_admin_callers: Vec::new(),
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
    async fn forbidden_membership_admin_is_a_domain_error_before_storage_access() {
        let provider = OrganizationProvider {
            state: Rc::new(RefCell::new(None)),
            admin_callers: vec!["business-admin".to_owned()],
            directory_callers: Vec::new(),
            membership_admin_callers: vec!["membership-admin".to_owned()],
        };
        let context = InvocationContext::new(1, None, CancellationToken::new())
            .with_caller_instance("untrusted");

        let add = provider
            .add_member(
                context.clone(),
                AddMemberRequest {
                    idempotency_key: "add-member".to_owned(),
                    organization_id: "org_acme".to_owned(),
                    subject: "usr_member".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(add, Err(AddMemberError::Forbidden));

        let remove = provider
            .remove_member(
                context,
                RemoveMemberRequest {
                    idempotency_key: "remove-member".to_owned(),
                    organization_id: "org_acme".to_owned(),
                    subject: "usr_member".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(remove, Err(RemoveMemberError::Forbidden));
    }

    #[tokio::test]
    async fn invalid_membership_admin_request_is_rejected_before_storage_access() {
        let provider = OrganizationProvider {
            state: Rc::new(RefCell::new(None)),
            admin_callers: vec!["business-admin".to_owned()],
            directory_callers: Vec::new(),
            membership_admin_callers: vec!["membership-admin".to_owned()],
        };
        let context = InvocationContext::new(1, None, CancellationToken::new())
            .with_caller_instance("membership-admin");

        let add = provider
            .add_member(
                context.clone(),
                AddMemberRequest {
                    idempotency_key: String::new(),
                    organization_id: "org_acme".to_owned(),
                    subject: "usr_member".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(add, Err(AddMemberError::InvalidRequest));

        let remove = provider
            .remove_member(
                context,
                RemoveMemberRequest {
                    idempotency_key: "remove-member".to_owned(),
                    organization_id: "org_acme".to_owned(),
                    subject: "invalid subject".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(remove, Err(RemoveMemberError::InvalidRequest));
    }

    #[tokio::test]
    async fn directory_authorization_and_validation_happen_before_storage_access() {
        let provider = OrganizationProvider {
            state: Rc::new(RefCell::new(None)),
            admin_callers: vec!["business-admin".to_owned()],
            directory_callers: vec!["directory-consumer".to_owned()],
            membership_admin_callers: Vec::new(),
        };
        let request = GetOrganizationRequest {
            organization_id: "org_acme".to_owned(),
        };
        let forbidden = provider
            .get_organization(
                InvocationContext::new(1, None, CancellationToken::new())
                    .with_caller_instance("untrusted"),
                request,
            )
            .await
            .unwrap();
        assert_eq!(forbidden, Err(GetOrganizationError::Forbidden));

        let invalid = provider
            .get_organization(
                InvocationContext::new(2, None, CancellationToken::new())
                    .with_caller_instance("directory-consumer"),
                GetOrganizationRequest {
                    organization_id: String::new(),
                },
            )
            .await
            .unwrap();
        assert_eq!(invalid, Err(GetOrganizationError::InvalidRequest));
    }

    #[tokio::test]
    async fn unprepared_membership_reports_runtime_failure() {
        let provider = OrganizationProvider {
            state: Rc::new(RefCell::new(None)),
            admin_callers: vec!["business-admin".to_owned()],
            directory_callers: Vec::new(),
            membership_admin_callers: Vec::new(),
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
            directory_callers: vec!["directory-consumer".to_owned()],
            membership_admin_callers: vec!["membership-admin".to_owned()],
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
    #[allow(
        clippy::too_many_lines,
        reason = "the acceptance scenario keeps membership concurrency, replay, and owner protection together"
    )]
    async fn membership_admin_is_caller_scoped_idempotent_and_owner_safe() {
        let database_url =
            std::env::var("LENSO_POSTGRES_TEST_URL").expect("LENSO_POSTGRES_TEST_URL is required");
        let schema = random_id("org_member_test_").unwrap();
        OrganizationOperator::setup(&database_url, &schema)
            .await
            .unwrap();
        let postgres = OwnedPostgres::prepare(&database_url, schema_plan(schema.clone()).unwrap())
            .await
            .unwrap();
        let provider = OrganizationProvider {
            state: Rc::new(RefCell::new(Some(PreparedOrganization { postgres }))),
            admin_callers: vec!["business-admin".to_owned()],
            directory_callers: vec!["directory-consumer".to_owned()],
            membership_admin_callers: vec![
                "membership-admin".to_owned(),
                "second-membership-admin".to_owned(),
            ],
        };
        let organization = provider
            .create_organization(
                InvocationContext::new(1, None, CancellationToken::new())
                    .with_caller_instance("business-admin"),
                CreateOrganizationRequest {
                    idempotency_key: "create-membership-test".to_owned(),
                    name: "Membership Test".to_owned(),
                    owner_subject: "usr_owner".to_owned(),
                    slug: "membership-test".to_owned(),
                },
            )
            .await
            .unwrap()
            .unwrap();
        let directory_entry = provider
            .get_organization(
                InvocationContext::new(2, None, CancellationToken::new())
                    .with_caller_instance("directory-consumer"),
                GetOrganizationRequest {
                    organization_id: organization.organization_id.clone(),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert!(directory_entry.active);
        assert_eq!(directory_entry.name, "Membership Test");
        assert_eq!(directory_entry.slug, "membership-test");
        assert_eq!(directory_entry.revision, "1");

        let missing_directory_entry = provider
            .get_organization(
                InvocationContext::new(3, None, CancellationToken::new())
                    .with_caller_instance("directory-consumer"),
                GetOrganizationRequest {
                    organization_id: "org_missing".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            missing_directory_entry,
            Err(GetOrganizationError::OrganizationNotFound)
        );
        let add_request = AddMemberRequest {
            idempotency_key: "add-primary-member".to_owned(),
            organization_id: organization.organization_id.clone(),
            subject: "usr_member".to_owned(),
        };
        let first_add = provider.add_member(
            InvocationContext::new(4, None, CancellationToken::new())
                .with_caller_instance("membership-admin"),
            add_request.clone(),
        );
        let concurrent_replay = provider.add_member(
            InvocationContext::new(5, None, CancellationToken::new())
                .with_caller_instance("membership-admin"),
            add_request.clone(),
        );
        let (first_add, concurrent_replay) = tokio::join!(first_add, concurrent_replay);
        let first_add = first_add.unwrap().unwrap();
        let concurrent_replay = concurrent_replay.unwrap().unwrap();
        assert_ne!(first_add.created, concurrent_replay.created);
        assert_eq!(first_add.membership_id, concurrent_replay.membership_id);
        assert_eq!(first_add.revision, "1");
        assert_eq!(concurrent_replay.revision, "1");

        let active = provider
            .check_membership(
                InvocationContext::new(4, None, CancellationToken::new()),
                CheckMembershipRequest {
                    organization_id: organization.organization_id.clone(),
                    subject: "usr_member".to_owned(),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert!(active.active);
        assert!(!active.owner);

        let conflict = provider
            .add_member(
                InvocationContext::new(5, None, CancellationToken::new())
                    .with_caller_instance("membership-admin"),
                AddMemberRequest {
                    subject: "usr_other".to_owned(),
                    ..add_request.clone()
                },
            )
            .await
            .unwrap();
        assert_eq!(conflict, Err(AddMemberError::IdempotencyConflict));

        let second_caller = provider
            .add_member(
                InvocationContext::new(6, None, CancellationToken::new())
                    .with_caller_instance("second-membership-admin"),
                AddMemberRequest {
                    subject: "usr_second".to_owned(),
                    ..add_request.clone()
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert!(second_caller.created);

        let owner_removal = provider
            .remove_member(
                InvocationContext::new(7, None, CancellationToken::new())
                    .with_caller_instance("membership-admin"),
                RemoveMemberRequest {
                    idempotency_key: "remove-owner".to_owned(),
                    organization_id: organization.organization_id.clone(),
                    subject: "usr_owner".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(owner_removal, Err(RemoveMemberError::OwnerProtected));

        let remove_request = RemoveMemberRequest {
            idempotency_key: "remove-primary-member".to_owned(),
            organization_id: organization.organization_id.clone(),
            subject: "usr_member".to_owned(),
        };
        let removal = provider
            .remove_member(
                InvocationContext::new(8, None, CancellationToken::new())
                    .with_caller_instance("membership-admin"),
                remove_request.clone(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(removal.removed);
        assert_eq!(removal.membership_id, first_add.membership_id);
        assert_eq!(removal.revision, "2");

        let replay = provider
            .remove_member(
                InvocationContext::new(9, None, CancellationToken::new())
                    .with_caller_instance("membership-admin"),
                remove_request,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(!replay.removed);
        assert_eq!(replay.membership_id, first_add.membership_id);
        assert_eq!(replay.revision, "2");

        let inactive = provider
            .check_membership(
                InvocationContext::new(10, None, CancellationToken::new()),
                CheckMembershipRequest {
                    organization_id: organization.organization_id.clone(),
                    subject: "usr_member".to_owned(),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert!(!inactive.active);
        assert!(!inactive.owner);

        let prepared = provider.prepared().unwrap();
        sqlx::query(
            "UPDATE organizations SET archived_at=transaction_timestamp(),revision=2 WHERE organization_id=$1",
        )
        .bind(&organization.organization_id)
        .execute(prepared.postgres.pool())
        .await
        .unwrap();
        let archived_directory_entry = provider
            .get_organization(
                InvocationContext::new(11, None, CancellationToken::new())
                    .with_caller_instance("directory-consumer"),
                GetOrganizationRequest {
                    organization_id: organization.organization_id.clone(),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert!(!archived_directory_entry.active);
        assert_eq!(archived_directory_entry.revision, "2");

        let archived_replay = provider
            .add_member(
                InvocationContext::new(12, None, CancellationToken::new())
                    .with_caller_instance("membership-admin"),
                add_request.clone(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(!archived_replay.created);
        assert_eq!(archived_replay.membership_id, first_add.membership_id);
        assert_eq!(archived_replay.revision, "1");

        let archived_new_command = provider
            .add_member(
                InvocationContext::new(13, None, CancellationToken::new())
                    .with_caller_instance("membership-admin"),
                AddMemberRequest {
                    idempotency_key: "archived-new-command".to_owned(),
                    organization_id: organization.organization_id.clone(),
                    subject: "usr_after_archive".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            archived_new_command,
            Err(AddMemberError::OrganizationNotFound)
        );

        let operation_conflict = provider
            .remove_member(
                InvocationContext::new(14, None, CancellationToken::new())
                    .with_caller_instance("membership-admin"),
                RemoveMemberRequest {
                    idempotency_key: add_request.idempotency_key,
                    organization_id: organization.organization_id.clone(),
                    subject: add_request.subject,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            operation_conflict,
            Err(RemoveMemberError::IdempotencyConflict)
        );

        let missing_organization = provider
            .add_member(
                InvocationContext::new(15, None, CancellationToken::new())
                    .with_caller_instance("membership-admin"),
                AddMemberRequest {
                    idempotency_key: "missing-organization".to_owned(),
                    organization_id: "org_missing".to_owned(),
                    subject: "usr_member".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            missing_organization,
            Err(AddMemberError::OrganizationNotFound)
        );

        prepared.postgres.pool().close().await;
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
