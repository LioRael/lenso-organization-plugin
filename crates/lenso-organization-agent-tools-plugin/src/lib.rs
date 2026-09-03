//! Agent-facing Tools over explicitly bound Organization administration capabilities.

use lenso::prelude::*;
use lenso_capability_agent_tool_provider::{
    self as tool_contract, CatalogRequest, CatalogResponse, ContentType, ExecuteError,
    ExecuteRequest, ExecuteResponse, ExecutionFailedPayload, ToolDefinition, ToolExecutionClass,
};
use lenso_capability_organization_admin::{
    self as admin, CreateOrganizationRequest, ListOrganizationsRequest,
};
use lenso_capability_organization_directory::{self as directory, GetOrganizationRequest};
use lenso_capability_organization_membership_admin::{
    self as membership_admin, AddMemberRequest, ListMembersRequest, RemoveMemberRequest,
};
use lenso_kernel::RuntimeFailure;
use serde::{Serialize, de::DeserializeOwned};

pub const GET_ORGANIZATION_TOOL: &str = "organization_admin_get_organization";
pub const LIST_ORGANIZATIONS_TOOL: &str = "organization_admin_list_organizations";
pub const CREATE_ORGANIZATION_TOOL: &str = "organization_admin_create_organization";
pub const LIST_MEMBERS_TOOL: &str = "organization_admin_list_members";
pub const ADD_MEMBER_TOOL: &str = "organization_admin_add_member";
pub const REMOVE_MEMBER_TOOL: &str = "organization_admin_remove_member";

#[lenso::plugin]
#[derive(Clone, Debug)]
struct OrganizationAgentToolsPlugin {
    admin: Port<admin::OrganizationAdminClient>,
    directory: Port<directory::OrganizationDirectoryClient>,
    membership_admin: Port<membership_admin::OrganizationMembershipAdminClient>,
}

#[lenso::provides(tool_contract::ToolProvider)]
impl OrganizationAgentToolsPlugin {
    fn catalog(
        &self,
        _context: Ctx,
        _request: CatalogRequest,
    ) -> impl std::future::Future<Output = PluginResult<CatalogResponse, tool_contract::CatalogError>>
    {
        let _ = self;
        futures::future::ready(Ok(CatalogResponse {
            tools: tool_definitions(),
        }))
    }

    async fn execute(
        &self,
        context: Ctx,
        request: ExecuteRequest,
    ) -> PluginResult<ExecuteResponse, ExecuteError> {
        macro_rules! invoke {
            ($future:expr, $tool:expr, $domain:path, $runtime:path, $map:expr) => {
                match $future.await {
                    Ok(response) => success($tool, &response),
                    Err($domain(error)) => Err(PluginError::domain($map(&error))),
                    Err($runtime(error)) => Err(PluginError::runtime(error)),
                }
            };
        }

        match request.name.as_str() {
            GET_ORGANIZATION_TOOL => {
                let arguments = decode::<GetOrganizationRequest>(&request)?;
                invoke!(
                    self.directory
                        .get_organization_with_context(context, arguments),
                    GET_ORGANIZATION_TOOL,
                    directory::OrganizationDirectoryInvocationError::Domain,
                    directory::OrganizationDirectoryInvocationError::Runtime,
                    map_get_organization_error
                )
            }
            LIST_ORGANIZATIONS_TOOL => {
                let arguments = decode::<ListOrganizationsRequest>(&request)?;
                invoke!(
                    self.admin
                        .list_organizations_with_context(context, arguments),
                    LIST_ORGANIZATIONS_TOOL,
                    admin::OrganizationAdminListOrganizationsInvocationError::Domain,
                    admin::OrganizationAdminListOrganizationsInvocationError::Runtime,
                    map_list_organizations_error
                )
            }
            CREATE_ORGANIZATION_TOOL => {
                let arguments = decode::<CreateOrganizationRequest>(&request)?;
                invoke!(
                    self.admin
                        .create_organization_with_context(context, arguments),
                    CREATE_ORGANIZATION_TOOL,
                    admin::OrganizationAdminCreateOrganizationInvocationError::Domain,
                    admin::OrganizationAdminCreateOrganizationInvocationError::Runtime,
                    map_create_organization_error
                )
            }
            LIST_MEMBERS_TOOL => {
                let arguments = decode::<ListMembersRequest>(&request)?;
                invoke!(
                    self.membership_admin
                        .list_members_with_context(context, arguments),
                    LIST_MEMBERS_TOOL,
                    membership_admin::OrganizationMembershipAdminListMembersInvocationError::Domain,
                    membership_admin::OrganizationMembershipAdminListMembersInvocationError::Runtime,
                    map_list_members_error
                )
            }
            ADD_MEMBER_TOOL => {
                let arguments = decode::<AddMemberRequest>(&request)?;
                invoke!(
                    self.membership_admin
                        .add_member_with_context(context, arguments),
                    ADD_MEMBER_TOOL,
                    membership_admin::OrganizationMembershipAdminAddMemberInvocationError::Domain,
                    membership_admin::OrganizationMembershipAdminAddMemberInvocationError::Runtime,
                    map_add_member_error
                )
            }
            REMOVE_MEMBER_TOOL => {
                let arguments = decode::<RemoveMemberRequest>(&request)?;
                invoke!(
                    self.membership_admin
                        .remove_member_with_context(context, arguments),
                    REMOVE_MEMBER_TOOL,
                    membership_admin::OrganizationMembershipAdminRemoveMemberInvocationError::Domain,
                    membership_admin::OrganizationMembershipAdminRemoveMemberInvocationError::Runtime,
                    map_remove_member_error
                )
            }
            _ => Err(PluginError::domain(ExecuteError::NotFound)),
        }
    }
}

fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        tool(
            GET_ORGANIZATION_TOOL,
            "Get one Organization's authoritative name, slug, active state, and revision.",
            include_str!(
                "../../lenso-capability-organization-directory/schemas/get-organization-request.schema.json"
            ),
            ToolExecutionClass::ParallelSafe,
        ),
        tool(
            LIST_ORGANIZATIONS_TOOL,
            "List Organizations with bounded cursor pagination and optional exact slug and status filters.",
            include_str!(
                "../../lenso-capability-organization-admin/schemas/list-organizations-request.schema.json"
            ),
            ToolExecutionClass::ParallelSafe,
        ),
        tool(
            LIST_MEMBERS_TOOL,
            "List memberships in one exact Organization with optional exact subject and status filters.",
            include_str!(
                "../../lenso-capability-organization-membership-admin/schemas/list-members-request.schema.json"
            ),
            ToolExecutionClass::ParallelSafe,
        ),
        tool(
            CREATE_ORGANIZATION_TOOL,
            "Create one Organization and its protected owner membership. Reuse the same idempotency_key when retrying one intent.",
            include_str!(
                "../../lenso-capability-organization-admin/schemas/create-organization-request.schema.json"
            ),
            ToolExecutionClass::Exclusive,
        ),
        tool(
            ADD_MEMBER_TOOL,
            "Add one active non-owner membership. Reuse the same idempotency_key when retrying one intent.",
            include_str!(
                "../../lenso-capability-organization-membership-admin/schemas/add-member-request.schema.json"
            ),
            ToolExecutionClass::Exclusive,
        ),
        tool(
            REMOVE_MEMBER_TOOL,
            "Remove one non-owner membership. Owners remain protected; reuse the same idempotency_key when retrying one intent.",
            include_str!(
                "../../lenso-capability-organization-membership-admin/schemas/remove-member-request.schema.json"
            ),
            ToolExecutionClass::Exclusive,
        ),
    ]
}

fn tool(
    name: &str,
    description: &str,
    schema: &str,
    execution: ToolExecutionClass,
) -> ToolDefinition {
    let schema: serde_json::Value =
        serde_json::from_str(schema).expect("Organization Agent Tool schema must be valid JSON");
    ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema_json: schema
            .to_string()
            .try_into()
            .expect("Organization Agent Tool schema must remain valid JSON"),
        execution,
    }
}

fn decode<T: DeserializeOwned>(request: &ExecuteRequest) -> PluginResult<T, ExecuteError> {
    serde_json::from_str(request.arguments_json.as_str())
        .map_err(|_| PluginError::domain(ExecuteError::InvalidArguments))
}

fn success<T: Serialize>(
    tool_name: &str,
    response: &T,
) -> PluginResult<ExecuteResponse, ExecuteError> {
    let content = serde_json::to_string_pretty(response).map_err(|error| {
        PluginError::runtime(RuntimeFailure::PluginFailure {
            detail: format!("Organization Agent Tool could not serialize its response: {error}"),
        })
    })?;
    Ok(ExecuteResponse {
        content_blocks: None,
        content,
        content_type: ContentType::Text,
        metadata_json: serde_json::json!({ "tool": tool_name })
            .to_string()
            .try_into()
            .expect("Organization Agent Tool metadata must be valid JSON"),
    })
}

fn map_get_organization_error(error: &directory::GetOrganizationError) -> ExecuteError {
    match error {
        directory::GetOrganizationError::Forbidden => ExecuteError::PermissionDenied,
        directory::GetOrganizationError::InvalidRequest => ExecuteError::InvalidArguments,
        directory::GetOrganizationError::OrganizationNotFound => ExecuteError::NotFound,
        directory::GetOrganizationError::Unknown(_) => rejected("unknown_domain_error"),
    }
}

fn map_list_organizations_error(error: &admin::ListOrganizationsError) -> ExecuteError {
    match error {
        admin::ListOrganizationsError::Forbidden => ExecuteError::PermissionDenied,
        admin::ListOrganizationsError::InvalidPage
        | admin::ListOrganizationsError::InvalidRequest => ExecuteError::InvalidArguments,
        admin::ListOrganizationsError::Unknown(_) => rejected("unknown_domain_error"),
    }
}

fn map_create_organization_error(error: &admin::CreateOrganizationError) -> ExecuteError {
    match error {
        admin::CreateOrganizationError::Forbidden => ExecuteError::PermissionDenied,
        admin::CreateOrganizationError::InvalidOrganization => ExecuteError::InvalidArguments,
        admin::CreateOrganizationError::IdempotencyConflict => rejected("idempotency_conflict"),
        admin::CreateOrganizationError::SlugConflict => rejected("slug_conflict"),
        admin::CreateOrganizationError::Unknown(_) => rejected("unknown_domain_error"),
    }
}

fn map_list_members_error(error: &membership_admin::ListMembersError) -> ExecuteError {
    match error {
        membership_admin::ListMembersError::Forbidden => ExecuteError::PermissionDenied,
        membership_admin::ListMembersError::InvalidPage
        | membership_admin::ListMembersError::InvalidRequest => ExecuteError::InvalidArguments,
        membership_admin::ListMembersError::OrganizationNotFound => ExecuteError::NotFound,
        membership_admin::ListMembersError::Unknown(_) => rejected("unknown_domain_error"),
    }
}

fn map_add_member_error(error: &membership_admin::AddMemberError) -> ExecuteError {
    match error {
        membership_admin::AddMemberError::Forbidden => ExecuteError::PermissionDenied,
        membership_admin::AddMemberError::InvalidRequest => ExecuteError::InvalidArguments,
        membership_admin::AddMemberError::OrganizationNotFound => ExecuteError::NotFound,
        membership_admin::AddMemberError::IdempotencyConflict => rejected("idempotency_conflict"),
        membership_admin::AddMemberError::Unknown(_) => rejected("unknown_domain_error"),
    }
}

fn map_remove_member_error(error: &membership_admin::RemoveMemberError) -> ExecuteError {
    match error {
        membership_admin::RemoveMemberError::Forbidden => ExecuteError::PermissionDenied,
        membership_admin::RemoveMemberError::InvalidRequest => ExecuteError::InvalidArguments,
        membership_admin::RemoveMemberError::MembershipNotFound
        | membership_admin::RemoveMemberError::OrganizationNotFound => ExecuteError::NotFound,
        membership_admin::RemoveMemberError::IdempotencyConflict => {
            rejected("idempotency_conflict")
        }
        membership_admin::RemoveMemberError::OwnerProtected => rejected("owner_protected"),
        membership_admin::RemoveMemberError::Unknown(_) => rejected("unknown_domain_error"),
    }
}

fn rejected(reason_code: &str) -> ExecuteError {
    ExecuteError::ExecutionFailed {
        payload: ExecutionFailedPayload {
            reason_code: reason_code.to_owned(),
            message: "Organization administration rejected the requested operation.".to_owned(),
            details_json: serde_json::json!({ "domain_error": reason_code })
                .to_string()
                .try_into()
                .expect("Organization Agent Tool error metadata must be valid JSON"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(name: &str, arguments: &str) -> ExecuteRequest {
        ExecuteRequest {
            name: name.to_owned(),
            arguments_json: arguments.try_into().unwrap(),
        }
    }

    #[test]
    fn descriptor_is_a_stateless_three_role_adapter() {
        let descriptor: serde_json::Value = serde_json::from_str(PLUGIN_DESCRIPTOR_JSON).unwrap();
        assert_eq!(descriptor["plugin_id"], "lenso.organization.agent-tools");
        assert_eq!(
            descriptor["provided_capabilities"][0]["capability_id"],
            "lenso.agent.tool-provider@2"
        );
        let required = descriptor["required_capabilities"].as_array().unwrap();
        assert_eq!(required.len(), 3);
        for capability in [
            "lenso.organization-admin@2",
            "lenso.organization-directory@1",
            "lenso.organization-membership-admin@1",
        ] {
            assert!(
                required
                    .iter()
                    .any(|requirement| requirement["capability_id"] == capability)
            );
        }
    }

    #[test]
    fn catalog_has_three_reads_and_three_mutations() {
        let tools = tool_definitions();
        assert_eq!(tools.len(), 6);
        assert_eq!(
            tools
                .iter()
                .filter(|tool| tool.execution == ToolExecutionClass::ParallelSafe)
                .count(),
            3
        );
        assert_eq!(
            tools
                .iter()
                .filter(|tool| tool.execution == ToolExecutionClass::Exclusive)
                .count(),
            3
        );
        assert!(
            tools
                .iter()
                .all(|tool| !tool.name.contains("check_membership"))
        );
    }

    #[test]
    fn exact_list_request_and_failures_remain_distinct() {
        let list = decode::<ListMembersRequest>(&request(
            LIST_MEMBERS_TOOL,
            r#"{"organization_id":"org_acme","subject":null,"status":"active","limit":50,"cursor":null}"#,
        ))
        .unwrap();
        assert_eq!(list.limit, 50);
        assert!(
            decode::<ListMembersRequest>(&request(
                LIST_MEMBERS_TOOL,
                r#"{"organization_id":"org_acme","subject":null,"status":"active","limit":"50","cursor":null}"#,
            ))
            .is_err()
        );
        assert_eq!(
            map_remove_member_error(&membership_admin::RemoveMemberError::OwnerProtected),
            rejected("owner_protected")
        );
        assert_eq!(
            map_get_organization_error(&directory::GetOrganizationError::OrganizationNotFound),
            ExecuteError::NotFound
        );
    }
}
