//! Generated Organization Directory Capability contract.

include!("generated.rs");

/// Backward-compatible name for the original directory lookup operation.
pub type OrganizationDirectory = OrganizationDirectoryGetOrganization;
/// Backward-compatible lookup error name.
pub type OrganizationDirectoryInvocationError = OrganizationDirectoryGetOrganizationInvocationError;
