use crate::environment::models::router::Router;
use crate::environment::models::types::{GCP, ToTeraContext};
use crate::errors::EngineError;
use crate::infrastructure::models::cloud_provider::DeploymentTarget;
use tera::Context as TeraContext;

impl ToTeraContext for Router<GCP> {
    fn to_tera_context(&self, target: &DeploymentTarget) -> Result<TeraContext, Box<EngineError>> {
        self.default_tera_context(target)
    }
}
