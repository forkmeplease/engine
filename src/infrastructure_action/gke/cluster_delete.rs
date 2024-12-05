use crate::cloud_provider::gcp::kubernetes::Gke;
use crate::cloud_provider::kubeconfig_helper::update_kubeconfig_file;
use crate::cloud_provider::kubernetes::Kubernetes;
use crate::engine::InfrastructureContext;
use crate::errors::EngineError;
use crate::events::Stage::Infrastructure;
use crate::events::{EventDetails, EventMessage, InfrastructureStep};
use crate::infrastructure_action::delete_kube_apps::delete_kube_apps;
use crate::infrastructure_action::deploy_terraform::TerraformInfraResources;
use crate::infrastructure_action::gke::GkeQoveryTerraformOutput;
use crate::infrastructure_action::{InfraLogger, ToInfraTeraContext};
use crate::object_storage::ObjectStorage;
use crate::secret_manager;
use crate::secret_manager::vault::QVaultClient;
use crate::utilities::envs_to_string;
use std::collections::HashSet;

pub(super) fn delete_gke_cluster(
    cluster: &Gke,
    infra_ctx: &InfrastructureContext,
    logger: impl InfraLogger,
) -> Result<(), Box<EngineError>> {
    let event_details = cluster.get_event_details(Infrastructure(InfrastructureStep::Delete));

    logger.info("Preparing to delete cluster.");
    let temp_dir = cluster.temp_dir();

    // should apply before destroy to be sure destroy will compute on all resources
    // don't exit on failure, it can happen if we resume a destroy process
    let message = format!(
        "Ensuring everything is up to date before deleting cluster {}/{}",
        cluster.name(),
        cluster.short_id()
    );
    logger.info(message);
    logger.info("Running Terraform apply before running a delete.");
    let tera_context = cluster.to_infra_tera_context(infra_ctx)?;
    let tf_resources = TerraformInfraResources::new(
        tera_context.clone(),
        cluster.template_directory.join("terraform"),
        temp_dir.join("terraform"),
        event_details.clone(),
        envs_to_string(infra_ctx.cloud_provider().credentials_environment_variables()),
        cluster.context().is_dry_run_deploy(),
    );
    let qovery_terraform_output: GkeQoveryTerraformOutput = tf_resources.create(&logger)?;
    update_kubeconfig_file(cluster, &qovery_terraform_output.kubeconfig)?;

    // Configure kubectl to be able to connect to cluster
    let _ = cluster.configure_gcloud_for_cluster(infra_ctx); // TODO(ENG-1802): properly handle this error
    delete_kube_apps(cluster, infra_ctx, event_details.clone(), &logger, HashSet::with_capacity(0))?;

    logger.info(format!("Deleting Kubernetes cluster {}/{}", cluster.name(), cluster.short_id()));
    tf_resources.delete(&[], &logger)?;

    // delete info on vault
    let _ = delete_vault_data(cluster, event_details.clone(), &logger);

    delete_object_storage(cluster, &logger)?;
    logger.info("Kubernetes cluster deleted successfully.");
    Ok(())
}

fn delete_object_storage(cluster: &Gke, logger: &impl InfraLogger) -> Result<(), Box<EngineError>> {
    // Because cluster logs buckets can be sometimes very beefy, we delete them in a non-blocking way via a GCP job.
    if let Err(e) = cluster
        .object_storage
        .delete_bucket_non_blocking(&cluster.logs_bucket_name())
    {
        logger.warn(EventMessage::new(
            format!("Cannot delete cluster logs object storage `{}`", &cluster.logs_bucket_name()),
            Some(e.to_string()),
        ));
    }

    Ok(())
}

fn delete_vault_data(
    cluster: &Gke,
    event_details: EventDetails,
    logger: &impl InfraLogger,
) -> Result<(), Box<EngineError>> {
    let vault_conn = QVaultClient::new(event_details.clone());
    if let Ok(vault_conn) = vault_conn {
        let mount = secret_manager::vault::get_vault_mount_name(cluster.context().is_test_cluster());

        // ignore on failure
        if let Err(e) = vault_conn.delete_secret(mount.as_str(), cluster.long_id().to_string().as_str()) {
            logger.warn(EventMessage::new(
                "Cannot delete cluster config from Vault".to_string(),
                Some(e.to_string()),
            ));
        }
    }

    Ok(())
}
