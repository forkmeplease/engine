use super::utils::delete_cached_image;
use crate::cmd::kubectl::{kubectl_exec_delete_job, kubectl_get_job_pod_output};
use crate::cmd::structs::KubernetesPodStatusPhase;
use crate::environment::action::DeploymentAction;
use crate::environment::action::deploy_helm::HelmDeployment;
use crate::environment::action::utils::{KubeObjectKind, get_last_deployed_image, mirror_image_if_necessary};
use crate::environment::models::job::{ImageSource, Job, JobService};
use crate::environment::models::types::{CloudProvider, ToTeraContext};
use crate::environment::report::job::reporter::JobDeploymentReporter;
use crate::environment::report::logger::{EnvProgressLogger, EnvSuccessLogger};
use crate::environment::report::{DeploymentTaskImpl, execute_long_deployment};
use crate::errors::{CommandError, EngineError, ErrorMessageVerbosity};
use crate::events::EngineEvent;
use crate::events::{EnvironmentStep, EventDetails, EventMessage, Stage};
use crate::helm::{ChartInfo, HelmChartNamespaces};
use crate::infrastructure::models::cloud_provider::DeploymentTarget;
use crate::infrastructure::models::cloud_provider::service::{Action, Service};
use crate::io_models::job::{JobSchedule, LifecycleType};
use crate::runtime::block_on;
use itertools::Itertools;
use k8s_openapi::api::batch::v1::{CronJob, Job as K8sJob};
use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::Api;
use kube::api::{AttachParams, ListParams, PostParams};
use kube::runtime::wait::{Condition, await_condition};
use retry::{Error, OperationResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cmp::min;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

impl<T: CloudProvider> DeploymentAction for Job<T>
where
    Job<T>: ToTeraContext,
{
    fn on_create(&self, target: &DeploymentTarget) -> Result<(), Box<EngineError>> {
        let event_details = self.get_event_details(Stage::Environment(self.action().to_environment_step()));

        // Force job to run, if force trigger is requested
        let default = JobSchedule::OnStart {
            lifecycle_type: self.schedule().lifecycle_type().unwrap_or(LifecycleType::GENERIC),
        };
        let job_schedule = if self.should_force_trigger() {
            &default
        } else {
            self.schedule()
        };

        match job_schedule {
            JobSchedule::OnStart { .. } | JobSchedule::Cron { .. } => {
                let (pre_run, run, post_run) = run_job(self, target, &event_details);
                let task = DeploymentTaskImpl {
                    pre_run: &pre_run,
                    run: &run,
                    post_run_success: &post_run,
                };

                execute_long_deployment(JobDeploymentReporter::new(self, target, Action::Create), task)
            }
            JobSchedule::OnPause { .. } | JobSchedule::OnDelete { .. } => {
                let job_reporter = JobDeploymentReporter::new(self, target, Action::Create);
                execute_long_deployment(job_reporter, |_logger: &EnvProgressLogger| -> Result<(), Box<EngineError>> {
                    Ok(())
                })
            }
        }
    }

    fn on_pause(&self, target: &DeploymentTarget) -> Result<(), Box<EngineError>> {
        let event_details = self.get_event_details(Stage::Environment(self.action().to_environment_step()));
        match self.schedule() {
            JobSchedule::Cron { .. } => {
                let (pre_run, run, post_run) = delete_job(self, target, &event_details);
                let task = DeploymentTaskImpl {
                    pre_run: &pre_run,
                    run: &run,
                    post_run_success: &post_run,
                };
                execute_long_deployment(JobDeploymentReporter::new(self, target, Action::Pause), task)
            }
            JobSchedule::OnPause { .. } => {
                let (pre_run, run, post_run) = run_job(self, target, &event_details);
                let task = DeploymentTaskImpl {
                    pre_run: &pre_run,
                    run: &run,
                    post_run_success: &post_run,
                };

                execute_long_deployment(JobDeploymentReporter::new(self, target, Action::Pause), task)
            }
            JobSchedule::OnStart { .. } | JobSchedule::OnDelete { .. } => {
                let job_reporter = JobDeploymentReporter::new(self, target, Action::Pause);
                execute_long_deployment(job_reporter, |_logger: &EnvProgressLogger| -> Result<(), Box<EngineError>> {
                    Ok(())
                })
            }
        }
    }

    fn on_delete(&self, target: &DeploymentTarget) -> Result<(), Box<EngineError>> {
        let event_details = self.get_event_details(Stage::Environment(self.action().to_environment_step()));
        match self.schedule() {
            JobSchedule::OnDelete { .. } => {
                let (pre_run, run, post_run) = run_job(self, target, &event_details);
                let task = DeploymentTaskImpl {
                    pre_run: &pre_run,
                    run: &run,
                    post_run_success: &post_run,
                };

                // We dont send final status when on_delete is executed because we want to keep the job in the Deleting state
                // while we are sure we have cleaned up all the resources
                execute_long_deployment(
                    JobDeploymentReporter::new_without_final_deleted(self, target, Action::Delete),
                    task,
                )
            }
            JobSchedule::Cron { .. } | JobSchedule::OnStart { .. } | JobSchedule::OnPause { .. } => Ok(()),
        }?;

        let (pre_run, run, post_run) = delete_job(self, target, &event_details);
        let task = DeploymentTaskImpl {
            pre_run: &pre_run,
            run: &run,
            post_run_success: &post_run,
        };
        execute_long_deployment(JobDeploymentReporter::new(self, target, Action::Delete), task)
    }

    fn on_restart(&self, target: &DeploymentTarget) -> Result<(), Box<EngineError>> {
        let command_error = CommandError::new_from_safe_message("Cannot restart Job service".to_string());
        Err(Box::new(EngineError::new_cannot_restart_service(
            self.get_event_details(Stage::Environment(EnvironmentStep::Restart)),
            target.environment.namespace(),
            &self.kube_label_selector(),
            command_error,
        )))
    }
}

struct TaskContext {
    last_deployed_image: Option<String>,
}

#[allow(clippy::type_complexity)]
fn run_job<'a, T: CloudProvider>(
    job: &'a Job<T>,
    target: &'a DeploymentTarget,
    event_details: &'a EventDetails,
) -> (
    impl Fn(&EnvProgressLogger) -> Result<TaskContext, Box<EngineError>> + 'a,
    impl Fn(&EnvProgressLogger, TaskContext) -> Result<TaskContext, Box<EngineError>> + 'a,
    impl Fn(&EnvSuccessLogger, TaskContext) + 'a,
)
where
    Job<T>: JobService,
{
    let metrics_registry = target.metrics_registry.clone();
    let pre_run = move |logger: &EnvProgressLogger| -> Result<TaskContext, Box<EngineError>> {
        match &job.image_source {
            // If image come from a registry, we mirror it to the cluster registry in order to avoid losing access to it due to creds expiration
            ImageSource::Registry { source } => {
                mirror_image_if_necessary(
                    job.long_id(),
                    source,
                    target,
                    logger,
                    event_details.clone(),
                    metrics_registry.clone(),
                )?;
            }
            ImageSource::Build { .. } => {}
        }

        let last_image = block_on(get_last_deployed_image(
            target.kube.client(),
            &job.kube_label_selector(),
            if job.is_cron_job() {
                KubeObjectKind::CronJob
            } else {
                KubeObjectKind::Job
            },
            target.environment.namespace(),
        ));

        Ok(TaskContext {
            last_deployed_image: last_image,
        })
    };

    let task = move |logger: &EnvProgressLogger, state: TaskContext| -> Result<TaskContext, Box<EngineError>> {
        let chart = ChartInfo {
            name: job.helm_release_name(),
            path: job.workspace_directory().to_string(),
            namespace: HelmChartNamespaces::Custom,
            custom_namespace: Some(target.environment.namespace().to_string()),
            timeout_in_seconds: job.startup_timeout().as_secs() as i64,
            k8s_selector: Some(job.kube_label_selector()),
            ..Default::default()
        };

        let helm = HelmDeployment::new(
            event_details.clone(),
            job.to_tera_context(target)?,
            PathBuf::from(job.helm_chart_dir()),
            None,
            chart,
        );

        // Wait for the job to terminate in order to have his status
        // For cronjob we dont care as we don't control when it is executed
        if job.schedule().is_job() {
            // We first need to delete the old job, because job spec cannot be updated (due to be an immutable resources)
            helm.on_delete(target)?;

            // create job
            helm.on_create(target)?;

            // Get kube config file
            let job_pod_selector = format!("job-name={}", job.kube_name());
            let kube_pod_api: Api<Pod> = Api::namespaced(target.kube.client(), target.environment.namespace());

            let job_max_nb_restart = job.max_nb_restart();
            let mut job_creation_iterations = 0;
            let mut set_of_pods_already_processed: HashSet<String> = HashSet::new();

            loop {
                // Wait for the pod to be started to get its name
                let pod_name = get_active_job_pod_by_selector(
                    kube_pod_api.clone(),
                    &job_pod_selector,
                    event_details,
                    &set_of_pods_already_processed,
                    job.max_duration(),
                )?;
                set_of_pods_already_processed.insert(pod_name.clone());

                // FIXME(ENG-1941) correctly handle cancel
                let should_force_cancel = async {
                    while !target.abort.status().should_force_cancel() {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                };

                // Wait for the job container to be terminated
                logger.info(format!("Waiting for the job container {} to be processed...", job.kube_name()));
                block_on(async {
                    tokio::select! {
                        biased;
                        _ = should_force_cancel => {},
                        _ = await_condition(
                            kube_pod_api.clone(),
                            &pod_name,
                            is_job_pod_container_terminated(job.kube_name()),
                        ) => {},
                    }
                });

                let status = target.abort.status();
                // If abort is forced, we delete lifecycle jobs
                if status.should_force_cancel() {
                    // force delete lifecycle jobs
                    for job in target.environment.jobs.iter().filter(|&j| j.job_schedule().is_job()) {
                        info!("Trying to delete lifecycle jobs with label `{}`", job.kube_label_selector());
                        match kubectl_exec_delete_job(
                            &target.kube,
                            job.kube_label_selector().as_str(),
                            Some(target.environment.namespace()),
                        ) {
                            Ok(_) => {
                                info!("Job with selector `{}` has been deleted", job.kube_label_selector());
                                return Err(Box::new(EngineError::new_task_cancellation_requested(
                                    event_details.clone(),
                                )));
                            }
                            Err(_) => warn!("Cannot delete job with selector `{}`", job.kube_label_selector()),
                        }
                    }
                }

                info!("Get JSON output from shared volume");
                // Get JSON output from shared volume
                let result_json_output = kubectl_get_job_pod_output(
                    target.kubernetes.kubeconfig_local_file_path(),
                    target.cloud_provider.credentials_environment_variables(),
                    target.environment.namespace(),
                    &pod_name,
                );
                match result_json_output {
                    Ok(json) => {
                        let result_serde_json: Result<HashMap<String, JobOutputVariable>, serde_json::Error> =
                            serialize_job_output(&json);
                        match result_serde_json {
                            Ok(deserialized_json_hashmap) => {
                                let deserialized_json_hashmap_with_uppercase_keys: HashMap<String, JobOutputVariable> =
                                    deserialized_json_hashmap
                                        .iter()
                                        .map(|(key, value)| (key.to_uppercase(), value.clone()))
                                        .collect();
                                logger.core_configuration_for_job(
                                    "Job output succeeded. Environment variables will be synchronized.".to_string(),
                                    serde_json::to_string(&deserialized_json_hashmap_with_uppercase_keys)
                                        .unwrap_or_else(|_| "{}".to_string()),
                                )
                            }
                            Err(err) => {
                                logger.log(EngineEvent::Warning(
                                    event_details.clone(),
                                    EventMessage::from(EngineError::new_invalid_job_output_cannot_be_serialized(
                                        event_details.clone(),
                                        err,
                                        &json,
                                    )),
                                ));
                            }
                        }
                    }
                    Err(err) => {
                        info!(
                            "Cannot get JSON job output: {}",
                            err.message(ErrorMessageVerbosity::FullDetailsWithoutEnvVars)
                        );
                    }
                };

                info!("Write file in shared volume to let the waiting container terminate");
                // Write file in shared volume to let the waiting container terminate
                block_on(kube_pod_api.clone().exec(
                    &pod_name,
                    vec!["touch", "/qovery-output/terminate"],
                    &AttachParams::default().container("qovery-wait-container-output"),
                ))
                .map_err(|_err| {
                    EngineError::new_job_error(
                        event_details.clone(),
                        format!("Cannot create terminate file inside waiting container for pod {}", &pod_name),
                    )
                })?;

                // wait for job to finish
                let jobs: Api<K8sJob> = Api::namespaced(target.kube.client(), target.environment.namespace());

                // await_condition WILL NOT return an error if the job is not found, hence checking the job existence before
                info!("Get Jobs");
                block_on(jobs.get(job.kube_name())).map_err(|err| {
                    EngineError::new_job_error(
                        event_details.clone(),
                        format!("Cannot get job {}: {}", job.kube_name(), err),
                    )
                })?;
                info!("Wait for job to finish");
                let ret = block_on(await_condition(jobs, job.kube_name(), is_job_terminated())).map_err(|_err| {
                    EngineError::new_job_error(
                        event_details.clone(),
                        format!("Cannot find job for terminated pod {}", &pod_name),
                    )
                })?;

                let job_status_result = match job_status(&ret.as_ref()) {
                    JobStatus::Success => return Ok(state),
                    JobStatus::NotRunning | JobStatus::Running => unreachable!(),
                    JobStatus::Failure { reason, message } => {
                        let msg = format!("Job failed to correctly run due to {reason} {message}");
                        debug!(msg);
                        debug!("Job pod: {:?}", ret);
                        Err(EngineError::new_job_error(event_details.clone(), msg))
                    }
                };

                // If job has restarted the maximum time, then return the result that should be an Err
                if job_creation_iterations == job_max_nb_restart {
                    job_status_result?;
                }
                job_creation_iterations += 1;
            }
        }

        // Cronjob will be installed
        if job.is_cron_job() && !job.is_force_trigger() {
            // create cronjob
            helm.on_create(target)?;
        }

        // Cronjob will be force triggered
        if job.is_cron_job() && job.is_force_trigger() {
            // check if cronjob is installed
            let k8s_cronjob_api: Api<CronJob> = Api::namespaced(target.kube.client(), target.environment.namespace());
            let cronjob_is_already_installed = block_on(k8s_cronjob_api.get(job.kube_name())).is_ok();

            // create cronjob
            helm.on_create(target)?;

            // Cronjob have been installed, in order to trigger it, we need to create a job from the cronjob manually.
            let k8s_job_api: Api<K8sJob> = Api::namespaced(target.kube.client(), target.environment.namespace());
            let cronjob = block_on(k8s_cronjob_api.get(job.kube_name())).map_err(|err| {
                EngineError::new_job_error(
                    event_details.clone(),
                    format!("Cannot get cronjob {}: {}", job.kube_name(), err),
                )
            })?;

            let mut job_template = cronjob.spec.expect("Cronjob should have a job_template").job_template;
            // For kube to automatically cleanup the job for us
            job_template
                .spec
                .as_mut()
                .expect("job_template should be editable")
                .ttl_seconds_after_finished = Some(10);
            // add a suffix in name to avoid conflict with jobs created by cronjob
            // truncate original name to 63 chars minus "-force-trigger" length to avoid k8s name length limit
            let job_name = format!("{}-force-trigger", job.kube_name().chars().take(63 - 14).join("").as_str(),);
            let job_to_start = K8sJob {
                metadata: ObjectMeta {
                    name: Some(job_name.to_string()),
                    ..job_template.metadata.expect("job_template should have metadata")
                },
                spec: job_template.spec,
                status: None,
            };
            block_on(k8s_job_api.create(&PostParams::default(), &job_to_start)).map_err(|err| {
                EngineError::new_job_error(event_details.clone(), format!("Cannot create job from cronjob: {err}"))
            })?;

            // FIXME(ENG-1942) correctly handle cancel
            let fut = async {
                match tokio::time::timeout(
                    // TODO is it the right duration to wait here? shouldn't we take the user-configured timeout value?
                    std::time::Duration::from_secs(3800), // We wait 1h + delta max for the job to be terminated
                    await_condition(k8s_job_api, &job_name, is_job_terminated()),
                )
                .await
                {
                    Ok(Ok(job_st)) => Ok(job_status(&job_st.as_ref())),
                    Ok(Err(err)) => Err(err),
                    Err(_) => Ok(JobStatus::Running), // timeout
                }
            };
            let job_status = block_on(fut).map_err(|_err| {
                EngineError::new_job_error(event_details.clone(), "Cannot find job for terminated pod".to_string())
            })?;

            let cronjob_result = match job_status {
                JobStatus::Success => Ok(()),
                JobStatus::Running => {
                    logger.info("Job is still running after 1h. Stopping waiting for it. Please check live-logs and service status to know its status".to_string());
                    Ok(())
                }
                JobStatus::NotRunning => {
                    let msg = "Job failed to correctly run due to `NotRunning`. This should not happen".to_string();
                    Err(EngineError::new_job_error(event_details.clone(), msg))
                }
                JobStatus::Failure { reason, message } => {
                    let msg = format!("Job failed to correctly run due to {reason} {message}");
                    Err(EngineError::new_job_error(event_details.clone(), msg))
                }
            };

            // uninstall cronjob if it was already present
            if !cronjob_is_already_installed {
                helm.on_delete(target)?;
            }

            // propagate if result is an error
            cronjob_result?;
        }

        Ok(state)
    };

    let post_run = move |logger: &EnvSuccessLogger, state: TaskContext| {
        // Delete previous image from cache to cleanup resources
        match &job.image_source {
            ImageSource::Registry { source } => {
                let mirrored_image_tag = source.tag_for_mirror(job.long_id());

                // In case the job is running due to the hook on on_delete
                // we don't to transition to a final state/deleted state
                // so we override the logger to not send a success message
                let empty_logger = |_: String| {};
                let normal_logger = |msg| logger.send_success(msg);
                let logger = if job.action() == &Action::Delete {
                    &empty_logger as &dyn Fn(String)
                } else {
                    &normal_logger as &dyn Fn(String)
                };
                if let Err(err) = delete_cached_image(
                    job.long_id(),
                    mirrored_image_tag,
                    state.last_deployed_image,
                    false,
                    target,
                    &logger,
                ) {
                    error!("Failed to delete previous image from cache: {}", err);
                }
            }
            ImageSource::Build { .. } => {}
        };
    };

    (pre_run, task, post_run)
}

#[allow(clippy::type_complexity)]
fn delete_job<'a, T: CloudProvider>(
    job: &'a Job<T>,
    target: &'a DeploymentTarget,
    event_details: &'a EventDetails,
) -> (
    impl Fn(&EnvProgressLogger) -> Result<TaskContext, Box<EngineError>> + 'a,
    impl Fn(&EnvProgressLogger, TaskContext) -> Result<TaskContext, Box<EngineError>> + 'a,
    impl Fn(&EnvSuccessLogger, TaskContext) + 'a,
)
where
    Job<T>: JobService,
{
    let pre_run = move |_logger: &EnvProgressLogger| -> Result<TaskContext, Box<EngineError>> {
        let last_image = block_on(get_last_deployed_image(
            target.kube.client(),
            &job.kube_label_selector(),
            if job.is_cron_job() {
                KubeObjectKind::CronJob
            } else {
                KubeObjectKind::Job
            },
            target.environment.namespace(),
        ));

        Ok(TaskContext {
            last_deployed_image: last_image,
        })
    };

    let task = move |_logger: &EnvProgressLogger, state: TaskContext| -> Result<TaskContext, Box<EngineError>> {
        let chart = ChartInfo {
            name: job.helm_release_name(),
            path: job.workspace_directory().to_string(),
            namespace: HelmChartNamespaces::Custom,
            custom_namespace: Some(target.environment.namespace().to_string()),
            timeout_in_seconds: job.startup_timeout().as_secs() as i64,
            k8s_selector: Some(job.kube_label_selector()),
            ..Default::default()
        };

        let helm = HelmDeployment::new(
            event_details.clone(),
            job.to_tera_context(target)?,
            PathBuf::from(job.helm_chart_dir()),
            None,
            chart,
        );

        helm.on_delete(target)?;

        Ok(state)
    };

    let post_run = move |logger: &EnvSuccessLogger, state: TaskContext| {
        match &job.image_source {
            // Delete previous image from cache to cleanup resources
            ImageSource::Registry { source } => {
                let mirrored_image_tag = source.tag_for_mirror(job.long_id());
                if let Err(err) = delete_cached_image(
                    job.long_id(),
                    mirrored_image_tag,
                    state.last_deployed_image,
                    true,
                    target,
                    &|msg| logger.send_success(msg),
                ) {
                    let user_msg = format!("Failed to delete previous image from cache: {err}");
                    logger.send_success(user_msg);
                }
            }

            // Delete shared container repository if needed (depending on flag computed on core)
            ImageSource::Build { source } => {
                if job.should_delete_shared_registry() {
                    logger.send_success("🪓 Terminating shared container registry of the job".to_string());
                    if let Err(err) = target
                        .container_registry
                        .delete_repository(source.image.shared_repository_name())
                    {
                        let user_msg =
                            format!("❌ Failed to delete shared container registry of the application: {err}");
                        logger.send_success(user_msg);
                    }
                }

                // Deleting the repository dedicated to this job
                logger.send_success("🪓 Terminating container registry of the job".to_string());
                if let Err(err) = target
                    .container_registry
                    .delete_repository(source.image.legacy_repository_name())
                {
                    let user_msg = format!("❌ Failed to delete container registry of the application: {err}");
                    logger.send_success(user_msg);
                }
            }
        };
    };

    (pre_run, task, post_run)
}

pub enum JobStatus {
    NotRunning,
    Running,
    Success,
    Failure { reason: String, message: String },
}

pub fn job_status(job: &Option<&K8sJob>) -> JobStatus {
    if let Some(pod) = job {
        if let Some(status) = &pod.status {
            if status.succeeded.is_some() {
                return JobStatus::Success;
            }

            if status.failed.is_some() {
                let condition = status
                    .conditions
                    .as_ref()
                    .and_then(|conds| conds.iter().find(|c| c.type_ == "Failed").cloned())
                    .unwrap_or_default();
                return JobStatus::Failure {
                    reason: condition.reason.unwrap_or_default(),
                    message: condition.message.unwrap_or_default(),
                };
            }
        }
        // TODO (mzo) deadline exceeded ?
        return JobStatus::Running;
    }
    JobStatus::NotRunning
}

pub fn is_job_terminated() -> impl Condition<K8sJob> {
    |job: Option<&K8sJob>| match job_status(&job) {
        JobStatus::NotRunning => false,
        JobStatus::Running => false,
        JobStatus::Success => true,
        JobStatus::Failure { .. } => true,
    }
}

pub fn get_active_job_pod_by_selector(
    kube_pod_api: Api<Pod>,
    job_pod_selector: &str,
    event_details: &EventDetails,
    set_of_pods_already_processed: &HashSet<String>,
    job_max_duration: &Duration,
) -> Result<String, Box<EngineError>> {
    // We wait at max 30 times 10 seconds (5min) for the pod to be running.
    // If the job max duration is lower than 5min, we reduce the number of retries to clamp it to its maximum duration
    let retry_fixed_delay =
        retry::delay::Fixed::from_millis(10_000).take(min(30, job_max_duration.as_secs() as usize / 10));

    // Trying to get the pod name, letting it up to 5 minutes to be scheduled
    let list_job_pods_result = retry::retry(retry_fixed_delay, || {
        // List pods according to job label selector
        let pods = match block_on(kube_pod_api.list(&ListParams::default().labels(job_pod_selector))) {
            Ok(pods_list) => {
                if pods_list.items.is_empty() {
                    return OperationResult::Retry(EngineError::new_job_error(
                        event_details.clone(),
                        format!("No pod found when listing pods having label {}", &job_pod_selector),
                    ));
                } else {
                    pods_list
                }
            }
            Err(_) => {
                return OperationResult::Retry(EngineError::new_job_error(
                    event_details.clone(),
                    format!("Error when listing pods having label {} through Kube API", &job_pod_selector),
                ));
            }
        };

        // If pod is pending for some reason (cluster scaling, etc.) let's move on to the next retry.
        if pods.items.iter().any(|pod| {
            if let Some(pod_status) = &pod.status {
                if let Some(phase) = &pod_status.phase {
                    // Pod has been scheduled but is Pending (e.q. in case of cluster node scale-up required)
                    return phase.to_lowercase() == KubernetesPodStatusPhase::Pending.to_string().to_lowercase();
                }
            }
            false
        }) {
            return OperationResult::Retry(EngineError::new_job_error(
                event_details.clone(),
                format!(
                    "Error pods having label {} are still pending to be scheduled",
                    &job_pod_selector
                ),
            ));
        }

        // Retrieve active pods
        let active_job_pods: Vec<String> = pods
            .items
            .iter()
            .filter_map(|pod| {
                if let Some(pod_status) = &pod.status {
                    if let Some(pod_container_statuses) = &pod_status.container_statuses {
                        // Pod is running, checking container statuses
                        let job_container_is_active = &pod_container_statuses
                            .iter()
                            .filter_map(|container_status| container_status.clone().state)
                            .any(|status| status.running.is_some());
                        if *job_container_is_active {
                            return Some(pod.metadata.name.as_ref().unwrap().clone());
                        }
                    }
                }
                None
            })
            .collect();

        // There should never be more than 1 pod in 'Running' status
        let active_selected_pod_name = match active_job_pods.len() {
            1 => active_job_pods.first().unwrap().to_string(),
            _ => {
                return OperationResult::Retry(EngineError::new_job_error(
                    event_details.clone(),
                    format!("Cannot find active pod having label {}", &job_pod_selector),
                ));
            }
        };

        // Check that the selected running pod has not already been processed
        if set_of_pods_already_processed.contains(&active_selected_pod_name) {
            return OperationResult::Retry(EngineError::new_job_error(
                event_details.clone(),
                format!(
                    "Selected pod has already been processed. Waiting for the next pod to be created having label {}",
                    &job_pod_selector
                ),
            ));
        }
        OperationResult::Ok(active_selected_pod_name)
    });
    match list_job_pods_result {
        Ok(active_pod_name) => Ok(active_pod_name),
        Err(Error { error, .. }) => Err(Box::new(error)),
    }
}

pub fn job_pod_container_status_is_terminated(job_pod: &Option<&Pod>, job_container_name: &str) -> bool {
    if let Some(pod) = job_pod {
        if let Some(pod_status) = &pod.status {
            info!("{}", format!("Job pod: {}  status {:?}", job_container_name, pod_status));
            if let Some(pod_container_statuses) = &pod_status.container_statuses {
                info!("{}", format!("Job pod {} container status", job_container_name));
                let job_container_terminated = &pod_container_statuses
                    .iter()
                    .filter(|container_status| container_status.name == job_container_name)
                    .filter_map(|container_status| container_status.clone().state)
                    .any(|status| status.terminated.is_some());
                return *job_container_terminated;
            }
        }
    }
    info!("{}", format!("Job pod container: {} is not terminated", job_container_name));
    false
}

pub fn is_job_pod_container_terminated(job_container_name: &str) -> impl Condition<Pod> + '_ {
    move |job_pod: Option<&Pod>| job_pod_container_status_is_terminated(&job_pod, job_container_name)
}

// Used to validate the job json output format with serde
#[derive(Serialize, Deserialize, Debug, Clone, Eq, PartialEq, Hash)]
#[serde(default)]
pub struct JobOutputVariable {
    pub value: String,
    pub sensitive: bool,
    pub description: String,
}

impl Default for JobOutputVariable {
    fn default() -> Self {
        JobOutputVariable {
            value: String::new(),
            sensitive: true,
            description: String::new(),
        }
    }
}

pub fn serialize_job_output(json: &str) -> Result<HashMap<String, JobOutputVariable>, serde_json::Error> {
    let serde_hash_map: HashMap<&str, Value> = serde_json::from_str(json)?;
    let mut job_output_variables: HashMap<String, JobOutputVariable> = HashMap::new();

    for (key, value) in serde_hash_map {
        let job_output_variable_object = value.as_object();
        let job_output_variable_hashmap = match job_output_variable_object {
            None => continue,
            Some(hashmap) => hashmap,
        };

        let serde_value_default = &Value::default();
        let value = job_output_variable_hashmap.get("value").unwrap_or(serde_value_default);

        // Get job output 'value' as string or any other type
        let job_output_value = if value.is_string() {
            value.as_str().unwrap_or_default().to_string()
        } else {
            value.to_string()
        };
        let job_output_description = job_output_variable_hashmap
            .get("description")
            .unwrap_or(serde_value_default)
            .as_str()
            .unwrap_or_default()
            .to_string();

        job_output_variables.insert(
            key.to_string(),
            JobOutputVariable {
                value: job_output_value,
                sensitive: job_output_variable_hashmap
                    .get("sensitive")
                    .unwrap_or(serde_value_default)
                    .as_bool()
                    .unwrap_or(false),
                description: job_output_description,
            },
        );
    }
    Ok(job_output_variables)
}

#[cfg(test)]
mod test {
    use crate::environment::action::deploy_job::{JobOutputVariable, serialize_job_output};

    #[test]
    fn should_serialize_json_to_job_output_variable_with_string_value() {
        // given
        let json_output_with_string_values = r#"
        {"foo": { "value": "bar", "sensitive": true }, "foo_2": {"value": "bar_2"} }
        "#;

        // when
        let hashmap = serialize_job_output(json_output_with_string_values).unwrap();

        // then
        assert_eq!(
            hashmap.get("foo").unwrap(),
            &JobOutputVariable {
                value: "bar".to_string(),
                sensitive: true,
                description: "".to_string(),
            }
        );
        assert_eq!(
            hashmap.get("foo_2").unwrap(),
            &JobOutputVariable {
                value: "bar_2".to_string(),
                sensitive: false,
                description: "".to_string(),
            }
        );
    }

    #[test]
    fn should_serialize_json_to_job_output_variable_with_numeric_value() {
        // given
        let json_output_with_numeric_values = r#"
        {"foo": { "value": 123, "sensitive": true }, "foo_2": {"value": 123.456} }
        "#;

        // when
        let hashmap = serialize_job_output(json_output_with_numeric_values).unwrap();

        // then
        assert_eq!(
            hashmap.get("foo").unwrap(),
            &JobOutputVariable {
                value: "123".to_string(),
                sensitive: true,
                description: "".to_string(),
            }
        );
        assert_eq!(
            hashmap.get("foo_2").unwrap(),
            &JobOutputVariable {
                value: "123.456".to_string(),
                sensitive: false,
                description: "".to_string(),
            }
        );
        let json_final = serde_json::to_string(&hashmap).unwrap();
        println!("{json_final}");
    }

    #[test]
    fn should_serialize_json_to_job_output_variable_with_description() {
        // given
        let json_output_with_numeric_values = r#"
        {"foo": { "value": 123, "description": "a description" }}
        "#;

        // when
        let hashmap = serialize_job_output(json_output_with_numeric_values).unwrap();

        // then
        assert_eq!(
            hashmap.get("foo").unwrap(),
            &JobOutputVariable {
                value: "123".to_string(),
                sensitive: false,
                description: "a description".to_string(),
            }
        );
        let json_final = serde_json::to_string(&hashmap).unwrap();
        println!("{json_final}");
    }
}
