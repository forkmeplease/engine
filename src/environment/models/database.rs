use crate::environment::action::DeploymentAction;
use crate::environment::models::annotations_group::AnnotationsGroupTeraContext;
use crate::environment::models::database_utils::{
    is_allowed_containered_mongodb_version, is_allowed_containered_mysql_version,
    is_allowed_containered_postgres_version, is_allowed_containered_redis_version,
};
use crate::environment::models::labels_group::LabelsGroupTeraContext;
use crate::environment::models::types::{CloudProvider, ToTeraContext, VersionsNumber};
use crate::environment::models::utils;
use crate::errors::{CommandError, EngineError};
use crate::events::{EnvironmentStep, EventDetails, Stage, Transmitter};
use crate::infrastructure::models::build_platform::Build;
use crate::infrastructure::models::cloud_provider::service::{
    Action, Service, ServiceType, ServiceVersionCheckResult, check_service_version, default_tera_context,
    get_service_statefulset_name_and_volumes,
};
use crate::infrastructure::models::cloud_provider::{DeploymentTarget, Kind, service};
use crate::infrastructure::models::kubernetes;
use crate::io_models::annotations_group::{Annotation, AnnotationsGroup};
use crate::io_models::context::Context;
use crate::io_models::database::DatabaseOptions;
use crate::io_models::labels_group::LabelsGroup;
use crate::io_models::models::{
    EnvironmentVariable, InvalidPVCStorage, InvalidStatefulsetStorage, KubernetesCpuResourceUnit,
    KubernetesMemoryResourceUnit,
};
use crate::kubers_utils::kube_get_resources_by_selector;
use crate::runtime::block_on;
use crate::unit_conversion::extract_volume_size;
use crate::utilities::to_short_id;
use chrono::{DateTime, Utc};
use k8s_openapi::api::core::v1::PersistentVolumeClaim;
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::Arc;
use tera::Context as TeraContext;
use uuid::Uuid;

/////////////////////////////////////////////////////////////////
// Database mode
pub trait DatabaseInstanceType: Send + Sync {
    fn cloud_provider(&self) -> Kind;
    fn to_cloud_provider_format(&self) -> String;
    fn is_instance_allowed(&self) -> bool;
    fn is_instance_compatible_with(&self, database_type: service::DatabaseType) -> bool;
}

pub struct Managed {}

pub struct Container {}

pub trait DatabaseMode: Send {
    fn is_managed() -> bool;
    fn is_container() -> bool {
        !Self::is_managed()
    }
}

impl DatabaseMode for Managed {
    fn is_managed() -> bool {
        true
    }
}

impl DatabaseMode for Container {
    fn is_managed() -> bool {
        false
    }
}

/////////////////////////////////////////////////////////////////
// Database types, will be only used as a marker
pub struct PostgresSQL {}

pub struct MySQL {}

pub struct MongoDB {}

pub struct Redis {}

pub trait DatabaseType<T: CloudProvider, M: DatabaseMode>: Send + Sync {
    type DatabaseOptions: Send + Sync;

    fn short_name() -> &'static str;
    fn lib_directory_name() -> &'static str;
    fn db_type() -> service::DatabaseType;

    // autocorrect resources if needed
    fn cpu_validate(desired_cpu: String) -> String {
        // TODO: cpu to use KubernetesCpuResourceUnit
        desired_cpu
    }
    fn cpu_burst_value(desired_cpu: String) -> String {
        // TODO: cpu to use KubernetesCpuResourceUnit
        desired_cpu
    }
    fn memory_validate(desired_memory: u32) -> u32 {
        // TODO: cpu to use KubernetesMemoryResourceUnit
        desired_memory
    }
}

#[derive(thiserror::Error, Debug, PartialEq)]
pub enum DatabaseError {
    #[error("Database invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("Managed database for {0:?} is not supported (yet) by provider {1}")]
    UnsupportedManagedMode(service::DatabaseType, String),

    #[error("Database not found error for `{database_type:?}/{database_id}`")]
    DatabaseNotFound {
        database_type: service::DatabaseType,
        database_id: String,
    },

    #[error("Version `{database_version}` for database for {database_type:?} is unknown")]
    UnknownDatabaseVersion {
        database_type: service::DatabaseType,
        database_version: Arc<str>,
    },

    #[error("Version `{database_version}` for database for {database_type:?} is not supported")]
    UnsupportedDatabaseVersion {
        database_type: service::DatabaseType,
        database_version: Arc<str>,
    },

    #[error(
        "Database instance type `{requested_database_instance_type}` is invalid for cloud provider `{database_cloud_provider}`."
    )]
    InvalidDatabaseInstance {
        requested_database_instance_type: String,
        database_cloud_provider: Kind,
    },

    #[error(
        "Database instance type `{database_instance_type_str}` doesn't belong to the database cloud provider `{database_cloud_provider:?}`"
    )]
    DatabaseInstanceTypeMismatchCloudProvider {
        database_instance_type_str: String,
        database_cloud_provider: Kind,
    },

    #[error(
        "Database instance type `{database_instance_type_str}` is not compatible with database type `{database_type:?}`"
    )]
    DatabaseInstanceTypeMismatchDatabaseType {
        database_instance_type_str: String,
        database_type: service::DatabaseType,
    },

    #[error("Unknown Database error: {0}")]
    UnknownError(String),
}

pub struct Database<C: CloudProvider, M: DatabaseMode, T: DatabaseType<C, M>> {
    _marker: PhantomData<(C, M, T)>,
    pub(crate) mk_event_details: Box<dyn Fn(Stage) -> EventDetails + Send + Sync>,
    pub(crate) id: String,
    pub(crate) long_id: Uuid,
    pub(crate) action: Action,
    pub(crate) name: String,
    pub(crate) kube_name: String,
    pub(crate) version: VersionsNumber,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) fqdn: String,
    pub(crate) fqdn_id: String,
    pub(crate) cpu_request_in_milli: KubernetesCpuResourceUnit,
    pub(crate) cpu_limit_in_milli: KubernetesCpuResourceUnit,
    pub(crate) ram_request_in_mib: KubernetesMemoryResourceUnit,
    pub(crate) ram_limit_in_mib: KubernetesMemoryResourceUnit,
    pub(crate) total_disk_size_in_gb: u32,
    pub(crate) database_instance_type: Option<Box<dyn DatabaseInstanceType>>,
    pub(crate) publicly_accessible: bool,
    pub(crate) private_port: u16,
    pub(crate) options: T::DatabaseOptions,
    pub(crate) workspace_directory: PathBuf,
    pub(crate) lib_root_directory: String,
    pub(crate) annotations_group: AnnotationsGroupTeraContext,
    pub(crate) additionnal_annotations: Vec<Annotation>,
    pub(crate) labels_group: LabelsGroupTeraContext,
}

impl<C: CloudProvider, M: DatabaseMode, T: DatabaseType<C, M>> Database<C, M, T> {
    pub fn new(
        context: &Context,
        long_id: Uuid,
        action: Action,
        name: &str,
        kube_name: String,
        version: VersionsNumber,
        created_at: DateTime<Utc>,
        fqdn: &str,
        fqdn_id: &str,
        cpu_request_in_milli: u32,
        cpu_limit_in_milli: u32,
        ram_request_in_mib: u32,
        ram_limit_in_mib: u32,
        total_disk_size_in_gb: u32,
        database_instance_type: Option<Box<dyn DatabaseInstanceType>>,
        publicly_accessible: bool,
        private_port: u16,
        options: T::DatabaseOptions,
        mk_event_details: impl Fn(Transmitter) -> EventDetails,
        annotations_groups: Vec<AnnotationsGroup>,
        additionnal_annotations: Vec<Annotation>,
        labels_groups: Vec<LabelsGroup>,
    ) -> Result<Self, DatabaseError> {
        // TODO: Implement domain constraint logic

        // check instance type is matching database cloud provider
        database_instance_type
            .as_ref()
            .map_or(Ok(()), |i| Self::check_instance_type_validity(i.as_ref(), C::cloud_provider()))?;

        let workspace_directory = crate::fs::workspace_directory(
            context.workspace_root_dir(),
            context.execution_id(),
            format!("databases/{long_id}"),
        )
        .map_err(|_| DatabaseError::InvalidConfig("Can't create workspace directory".to_string()))?;

        // Check memory settings only for container databases, as managed db are using an instance type
        if M::is_container() {
            if cpu_request_in_milli > cpu_limit_in_milli {
                return Err(DatabaseError::InvalidConfig(
                    "cpu_request_in_milli must be less or equal to cpu_limit_in_milli".to_string(),
                ));
            }

            if cpu_request_in_milli == 0 {
                return Err(DatabaseError::InvalidConfig(
                    "cpu_request_in_milli must be greater than 0".to_string(),
                ));
            }

            if ram_request_in_mib > ram_limit_in_mib {
                return Err(DatabaseError::InvalidConfig(
                    "ram_request_in_mib must be less or equal to ram_limit_in_mib".to_string(),
                ));
            }

            if ram_request_in_mib == 0 {
                return Err(DatabaseError::InvalidConfig(
                    "ram_request_in_mib must be greater than 0".to_string(),
                ));
            }
        }

        let event_details = mk_event_details(Transmitter::Database(long_id, name.to_string()));
        let mk_event_details = move |stage: Stage| EventDetails::clone_changing_stage(event_details.clone(), stage);
        Ok(Self {
            _marker: PhantomData,
            mk_event_details: Box::new(mk_event_details),
            action,
            id: to_short_id(&long_id),
            long_id,
            name: name.to_string(),
            kube_name,
            version,
            created_at,
            fqdn: fqdn.to_string(),
            fqdn_id: fqdn_id.to_string(),
            cpu_request_in_milli: KubernetesCpuResourceUnit::MilliCpu(cpu_request_in_milli),
            cpu_limit_in_milli: KubernetesCpuResourceUnit::MilliCpu(cpu_limit_in_milli),
            ram_request_in_mib: KubernetesMemoryResourceUnit::MebiByte(ram_request_in_mib),
            ram_limit_in_mib: KubernetesMemoryResourceUnit::MebiByte(ram_limit_in_mib),
            total_disk_size_in_gb,
            database_instance_type,
            publicly_accessible,
            private_port,
            options,
            workspace_directory,
            lib_root_directory: context.lib_root_dir().to_string(),
            annotations_group: AnnotationsGroupTeraContext::new(annotations_groups),
            additionnal_annotations,
            labels_group: LabelsGroupTeraContext::new(labels_groups),
        })
    }

    pub fn kube_label_selector(&self) -> String {
        format!("qovery.com/service-id={}", self.long_id)
    }

    pub fn workspace_directory(&self) -> &str {
        self.workspace_directory.to_str().unwrap_or("")
    }

    pub(crate) fn fqdn(&self, target: &DeploymentTarget, fqdn: &str) -> String {
        match &self.publicly_accessible {
            true => fqdn.to_string(),
            false => match M::is_managed() {
                true => format!("{}-dns.{}.svc.cluster.local", self.id(), target.environment.namespace()),
                false => format!("{}.{}.svc.cluster.local", self.kube_name(), target.environment.namespace()),
            },
        }
    }

    fn _cloud_provider(&self) -> Kind {
        C::cloud_provider()
    }

    fn check_instance_type_validity(
        database_instance_type: &dyn DatabaseInstanceType,
        database_cloud_provider_kind: Kind,
    ) -> Result<(), DatabaseError> {
        // instance type should belongs to database cloud provider
        if database_instance_type.cloud_provider() != database_cloud_provider_kind {
            // database instance type doesn't belong to database cloud provider
            return Err(DatabaseError::DatabaseInstanceTypeMismatchCloudProvider {
                database_cloud_provider: database_cloud_provider_kind,
                database_instance_type_str: database_instance_type.to_cloud_provider_format(),
            });
        }

        Ok(())
    }
}

impl<C: CloudProvider, M: DatabaseMode, T: DatabaseType<C, M>> Service for Database<C, M, T> {
    fn service_type(&self) -> ServiceType {
        ServiceType::Database(T::db_type())
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn long_id(&self) -> &Uuid {
        &self.long_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn version(&self) -> String {
        self.version.to_string()
    }

    fn kube_name(&self) -> &str {
        &self.kube_name
    }

    fn kube_label_selector(&self) -> String {
        self.kube_label_selector()
    }

    fn get_event_details(&self, stage: Stage) -> EventDetails {
        (self.mk_event_details)(stage)
    }

    fn action(&self) -> &Action {
        &self.action
    }

    fn as_service(&self) -> &dyn Service {
        self
    }

    fn as_service_mut(&mut self) -> &mut dyn Service {
        self
    }

    fn build(&self) -> Option<&Build> {
        None
    }

    fn build_mut(&mut self) -> Option<&mut Build> {
        None
    }

    fn get_environment_variables(&self) -> Vec<EnvironmentVariable> {
        vec![]
    }
}

// Method Only For all container database
impl<C: CloudProvider, T: DatabaseType<C, Container>> Database<C, Container, T> {
    pub fn helm_release_name(&self) -> String {
        format!("{}-{}", T::lib_directory_name(), self.id)
    }

    pub fn helm_chart_dir(&self) -> String {
        format!("{}/common/services/{}", self.lib_root_directory, T::lib_directory_name())
    }

    pub fn helm_chart_values_dir(&self) -> String {
        format!(
            "{}/{}/chart_values/{}",
            self.lib_root_directory,
            C::lib_directory_name(),
            T::lib_directory_name()
        )
    }

    pub(crate) fn to_tera_context_for_container(
        &self,
        target: &DeploymentTarget,
        options: &DatabaseOptions,
    ) -> Result<TeraContext, Box<EngineError>> {
        let event_details = self.get_event_details(Stage::Environment(EnvironmentStep::LoadConfiguration));
        let kubernetes = target.kubernetes;
        let environment = target.environment;
        let mut context = default_tera_context(self, kubernetes, environment);

        // we can't link a security group to an NLB, so we need this to deny any access
        let cluster_denied_any_access = match T::db_type() {
            service::DatabaseType::PostgreSQL => kubernetes.advanced_settings().database_postgresql_deny_any_access,
            service::DatabaseType::MongoDB => kubernetes.advanced_settings().database_mongodb_deny_any_access,
            service::DatabaseType::MySQL => kubernetes.advanced_settings().database_mysql_deny_any_access,
            service::DatabaseType::Redis => kubernetes.advanced_settings().database_redis_deny_any_access,
        };
        let container_database_publicly_accessible = !cluster_denied_any_access && self.publicly_accessible;

        // repository and image location
        let registry_name = "public.ecr.aws";
        let repository_name = format!("r3m4q3r9/pub-mirror-{}", T::db_type().to_string().to_lowercase());
        let repository_name_minideb = "r3m4q3r9/pub-mirror-minideb".to_string();
        let repository_name_bitnami_shell = "r3m4q3r9/pub-mirror-bitnami-shell".to_string();
        context.insert("registry_name", registry_name);
        context.insert("repository_name", repository_name.as_str());
        context.insert("repository_name_minideb", repository_name_minideb.as_str());
        context.insert("repository_name_bitnami_shell", repository_name_bitnami_shell.as_str());
        context.insert(
            "repository_with_registry",
            format!("{registry_name}/{repository_name}").as_str(),
        );

        context.insert("namespace", environment.namespace());

        let version = self.get_version(event_details)?.matched_version();
        context.insert("version", &version.to_string());

        for (k, v) in target.cloud_provider.tera_context_environment_variables() {
            context.insert(k, v);
        }

        context.insert("kubernetes_cluster_id", kubernetes.short_id());
        context.insert("kubernetes_cluster_name", kubernetes.name());

        context.insert("fqdn_id", self.fqdn_id.as_str());
        context.insert("fqdn", self.fqdn(target, &self.fqdn).as_str());
        context.insert("service_name", self.fqdn_id.as_str());
        context.insert("database_db_name", &self.name);
        context.insert("database_login", options.login.as_str());
        context.insert("database_password", options.password.as_str());
        context.insert("database_port", &self.private_port);
        context.insert("database_disk_size_in_gib", &options.disk_size_in_gib);
        if let Some(i) = &self.database_instance_type {
            context.insert("database_instance_type", i.to_cloud_provider_format().as_str());
        }
        context.insert("database_disk_type", &options.database_disk_type);
        context.insert("cpu_request_in_milli", &self.cpu_request_in_milli.to_string());
        context.insert("cpu_limit_in_milli", &self.cpu_limit_in_milli.to_string());
        context.insert("ram_request_in_mib", &self.ram_request_in_mib.to_string());
        context.insert("ram_limit_in_mib", &self.ram_limit_in_mib.to_string());
        context.insert("database_fqdn", &options.host.as_str());
        context.insert("database_id", &self.id());
        context.insert("publicly_accessible", &container_database_publicly_accessible);

        // NLB or ALB controller annotation
        context.insert(
            "aws_load_balancer_type",
            match &kubernetes.advanced_settings().aws_eks_enable_alb_controller {
                true => "external",
                false => "nlb",
            },
        );

        context.insert(
            "resource_expiration_in_seconds",
            &kubernetes.advanced_settings().pleco_resources_ttl,
        );

        let mut node_affinity = BTreeMap::<String, String>::new();
        let mut toleration = BTreeMap::<String, String>::new();

        // some Database/Version do not support arm arch
        let (node_affinity_type, node_affinity_key, node_affinity_values) = match T::db_type() {
            service::DatabaseType::PostgreSQL if version.major == "10" => ("hard", "kubernetes.io/arch", vec!["amd64"]),
            service::DatabaseType::Redis if version.major == "5" => ("hard", "kubernetes.io/arch", vec!["amd64"]),
            service::DatabaseType::MongoDB => ("hard", "kubernetes.io/arch", vec!["amd64"]),
            service::DatabaseType::PostgreSQL => ("", "", vec![]),
            service::DatabaseType::Redis => ("", "", vec![]),
            service::DatabaseType::MySQL => ("", "", vec![]),
        };

        if let Some(value) = node_affinity_values.first() {
            node_affinity.insert(node_affinity_key.to_string(), value.to_string());
        }
        if kubernetes.kind() == kubernetes::Kind::Eks && kubernetes.is_karpenter_enabled() {
            utils::target_stable_node_pool(&mut node_affinity, &mut toleration, true);
        }

        context.insert("toleration", &toleration);
        context.insert("node_affinity", &node_affinity);
        context.insert("node_affinity_type", &node_affinity_type);
        context.insert("node_affinity_key", &node_affinity_key);
        context.insert("node_affinity_values", &node_affinity_values);
        context.insert("annotations_group", &self.annotations_group);
        context.insert("additional_annotations", &self.additionnal_annotations);
        context.insert("labels_group", &self.labels_group);

        Ok(context)
    }

    fn get_version(&self, event_details: EventDetails) -> Result<ServiceVersionCheckResult, Box<EngineError>> {
        let fn_version = match T::db_type() {
            service::DatabaseType::PostgreSQL => is_allowed_containered_postgres_version,
            service::DatabaseType::MongoDB => is_allowed_containered_mongodb_version,
            service::DatabaseType::MySQL => is_allowed_containered_mysql_version,
            service::DatabaseType::Redis => is_allowed_containered_redis_version,
        };

        check_service_version(
            fn_version(&self.version)
                .map(|_| self.version.to_string())
                .map_err(CommandError::from),
            self,
            event_details,
        )
    }
}

// methods for all Managed databases
impl<C: CloudProvider, T: DatabaseType<C, Managed>> Database<C, Managed, T> {
    pub fn helm_chart_external_name_service_dir(&self) -> String {
        format!("{}/common/charts/external-name-svc", self.lib_root_directory)
    }

    pub fn terraform_common_resource_dir_path(&self) -> String {
        format!("{}/{}/services/common", self.lib_root_directory, C::lib_directory_name())
    }

    pub fn terraform_resource_dir_path(&self) -> String {
        format!(
            "{}/{}/services/{}",
            self.lib_root_directory,
            C::lib_directory_name(),
            T::lib_directory_name()
        )
    }
}

pub trait DatabaseService: Service + DeploymentAction + ToTeraContext + Send {
    fn is_managed_service(&self) -> bool;

    fn db_type(&self) -> service::DatabaseType;

    fn db_instance_type(&self) -> Option<&dyn DatabaseInstanceType>;

    fn as_deployment_action(&self) -> &dyn DeploymentAction;

    fn total_disk_size_in_gb(&self) -> u32;
}

impl<C: CloudProvider, M: DatabaseMode, T: DatabaseType<C, M>> DatabaseService for Database<C, M, T>
where
    Database<C, M, T>: Service + DeploymentAction + ToTeraContext,
{
    fn is_managed_service(&self) -> bool {
        M::is_managed()
    }

    fn db_type(&self) -> service::DatabaseType {
        T::db_type()
    }

    fn db_instance_type(&self) -> Option<&dyn DatabaseInstanceType> {
        match &self.database_instance_type {
            None => None,
            Some(t) => Some(t.as_ref()),
        }
    }

    fn as_deployment_action(&self) -> &dyn DeploymentAction {
        self
    }

    fn total_disk_size_in_gb(&self) -> u32 {
        self.total_disk_size_in_gb
    }
}

pub fn get_database_with_invalid_storage_size<C: CloudProvider, M: DatabaseMode, T: DatabaseType<C, M>>(
    database: &Database<C, M, T>,
    kube_client: &kube::Client,
    namespace: &str,
    event_details: &EventDetails,
) -> Result<Option<InvalidStatefulsetStorage>, Box<EngineError>> {
    let selector = database.kube_label_selector();
    let (statefulset_name, statefulset_volumes) =
        get_service_statefulset_name_and_volumes(kube_client, namespace, &selector, event_details)?;
    let storage_err = Box::new(EngineError::new_service_missing_storage(
        event_details.clone(),
        &database.long_id,
    ));
    let volume = match statefulset_volumes {
        None => return Err(storage_err),
        Some(volumes) => {
            // ATM only one volume should be bound to container database
            if volumes.len() > 1 {
                return Err(storage_err);
            }

            match volumes.first() {
                None => return Err(storage_err),
                Some(volume) => volume.clone(),
            }
        }
    };

    if let Some(spec) = &volume.spec {
        if let Some(resources) = &spec.resources {
            if let Some(requests) = &resources.requests {
                // in order to compare volume size from engine request to effective size in kube, we must get the  effective size
                let size = extract_volume_size(requests["storage"].0.to_string()).map_err(|e| {
                    Box::new(EngineError::new_cannot_parse_string(
                        event_details.clone(),
                        &requests["storage"].0,
                        e,
                    ))
                })?;

                if database.total_disk_size_in_gb > size {
                    // if volume size in request is bigger than effective size we get related PVC to get its infos
                    if let Some(pvc) = block_on(kube_get_resources_by_selector::<PersistentVolumeClaim>(
                        kube_client,
                        namespace,
                        &format!("app={}", database.kube_name()),
                    ))
                    .map_err(|e| EngineError::new_k8s_cannot_get_pvcs(event_details.clone(), namespace, e))?
                    .items
                    .first()
                    {
                        if let Some(pvc_name) = &pvc.metadata.name {
                            return Ok(Some(InvalidStatefulsetStorage {
                                service_type: Database::service_type(database),
                                service_id: database.long_id,
                                statefulset_selector: selector,
                                statefulset_name,
                                invalid_pvcs: vec![InvalidPVCStorage {
                                    pvc_name: pvc_name.to_string(),
                                    required_disk_size_in_gib: database.total_disk_size_in_gb,
                                }],
                            }));
                        }
                    };
                }

                if database.total_disk_size_in_gb < size {
                    return Err(Box::new(EngineError::new_invalid_engine_payload(
                        event_details.clone(),
                        format!(
                            "new storage size ({}) should be equal or greater than actual size ({})",
                            database.total_disk_size_in_gb, size
                        )
                        .as_str(),
                        None,
                    )));
                }
            }
        }
    }

    Ok(None)
}
