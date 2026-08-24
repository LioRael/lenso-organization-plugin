//! Non-production Secrets provider used only by Composition fixtures.

use std::rc::Rc;

use lenso_capability_secrets::{
    ResolveError, ResolveRequest, Secrets, SecretsEndpoint, SecretsProvider,
};
use lenso_kernel::{InvocationContext, NativeRequestEndpoint, NativeRequestFuture, RuntimeFailure};
use lenso_native_adapter::{NativeModuleFactory, NativeModuleFactoryContext, NativeModuleInstance};

pub const PACKAGE_ID: &str = "lenso.organization.fixture";
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, Debug, Default)]
pub struct FixtureSecretsFactory;

impl NativeModuleFactory for FixtureSecretsFactory {
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
        if context.configuration() != "{}" {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: "Organization fixture accepts only empty configuration".to_owned(),
            });
        }
        match context.entrypoint() {
            "secrets" => {
                let endpoint =
                    Rc::new(SecretsEndpoint::new(FixtureSecrets)) as Rc<dyn NativeRequestEndpoint>;
                Ok(NativeModuleInstance::new(vec![endpoint]))
            }
            "consumer" => Ok(NativeModuleInstance::new(Vec::new())),
            other => Err(RuntimeFailure::InvalidResolvedPlan {
                detail: format!("unsupported Organization fixture entrypoint `{other}`"),
            }),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct FixtureSecrets;

impl SecretsProvider for FixtureSecrets {
    fn resolve(
        &self,
        _context: InvocationContext,
        _request: ResolveRequest,
    ) -> NativeRequestFuture<Secrets> {
        Box::pin(async { Ok(Err(ResolveError::UnknownReference)) })
    }
}
