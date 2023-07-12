use std::sync::Arc;

use crate::cloud_provider::helm::{
    ChartInfo, ChartInstallationChecker, ChartSetValue, CommonChart, HelmChartNamespaces,
};
use crate::cloud_provider::helm_charts::{
    HelmChartDirectoryLocation, HelmChartPath, HelmChartValuesFilePath, ToCommonHelmChart,
};
use crate::cloud_provider::models::{
    CustomerHelmChartsOverride, KubernetesCpuResourceUnit, KubernetesMemoryResourceUnit,
};
use crate::errors::CommandError;
use kube::Client;

use super::{HelmChartResources, HelmChartResourcesConstraintType};

pub struct NginxIngressChart {
    chart_path: HelmChartPath,
    chart_values_path: HelmChartValuesFilePath,
    controller_resources: HelmChartResources,
    default_backend_resources: HelmChartResources,
    ff_metrics_history_enabled: bool,
    customer_helm_chart_override: Option<CustomerHelmChartsOverride>,
}

impl NginxIngressChart {
    pub fn new(
        chart_prefix_path: Option<&str>,
        controller_resources: HelmChartResourcesConstraintType,
        default_backend_resources: HelmChartResourcesConstraintType,
        ff_metrics_history_enabled: bool,
        customer_helm_chart_fn: Arc<dyn Fn(String) -> Option<CustomerHelmChartsOverride>>,
    ) -> Self {
        NginxIngressChart {
            chart_path: HelmChartPath::new(
                chart_prefix_path,
                HelmChartDirectoryLocation::CommonFolder,
                NginxIngressChart::chart_name(),
            ),
            chart_values_path: HelmChartValuesFilePath::new(
                chart_prefix_path,
                HelmChartDirectoryLocation::CloudProviderFolder,
                NginxIngressChart::chart_old_name(),
            ),
            controller_resources: match controller_resources {
                HelmChartResourcesConstraintType::ChartDefault => HelmChartResources {
                    request_cpu: KubernetesCpuResourceUnit::MilliCpu(100),
                    request_memory: KubernetesMemoryResourceUnit::MebiByte(768),
                    limit_cpu: KubernetesCpuResourceUnit::MilliCpu(500),
                    limit_memory: KubernetesMemoryResourceUnit::MebiByte(768),
                },
                HelmChartResourcesConstraintType::Constrained(r) => r,
            },
            default_backend_resources: match default_backend_resources {
                HelmChartResourcesConstraintType::ChartDefault => HelmChartResources {
                    request_cpu: KubernetesCpuResourceUnit::MilliCpu(10),
                    request_memory: KubernetesMemoryResourceUnit::MebiByte(32),
                    limit_cpu: KubernetesCpuResourceUnit::MilliCpu(20),
                    limit_memory: KubernetesMemoryResourceUnit::MebiByte(32),
                },
                HelmChartResourcesConstraintType::Constrained(r) => r,
            },
            ff_metrics_history_enabled,
            customer_helm_chart_override: customer_helm_chart_fn(Self::chart_name()),
        }
    }

    pub fn chart_name() -> String {
        "ingress-nginx".to_string()
    }

    // for history reasons where nginx-ingress has changed to ingress-nginx
    pub fn chart_old_name() -> String {
        "nginx-ingress".to_string()
    }
}

impl ToCommonHelmChart for NginxIngressChart {
    fn to_common_helm_chart(&self) -> CommonChart {
        CommonChart {
            chart_info: ChartInfo {
                name: NginxIngressChart::chart_old_name(),
                path: self.chart_path.to_string(),
                namespace: HelmChartNamespaces::NginxIngress,
                // Because of NLB, svc can take some time to start
                timeout_in_seconds: 300,
                values_files: vec![self.chart_values_path.to_string()],
                values: vec![
                    ChartSetValue {
                        key: "controller.admissionWebhooks.enabled".to_string(),
                        value: "false".to_string(),
                    },
                    // metrics
                    ChartSetValue {
                        key: "controller.metrics.enabled".to_string(),
                        value: self.ff_metrics_history_enabled.to_string(),
                    },
                    ChartSetValue {
                        key: "controller.metrics.serviceMonitor.enabled".to_string(),
                        value: self.ff_metrics_history_enabled.to_string(),
                    },
                    // Controller resources limits
                    ChartSetValue {
                        key: "controller.resources.limits.cpu".to_string(),
                        value: self.controller_resources.limit_cpu.to_string(),
                    },
                    ChartSetValue {
                        key: "controller.resources.requests.cpu".to_string(),
                        value: self.controller_resources.request_cpu.to_string(),
                    },
                    ChartSetValue {
                        key: "controller.resources.limits.memory".to_string(),
                        value: self.controller_resources.limit_memory.to_string(),
                    },
                    ChartSetValue {
                        key: "controller.resources.requests.memory".to_string(),
                        value: self.controller_resources.request_memory.to_string(),
                    },
                    // Default backend resources limits
                    ChartSetValue {
                        key: "defaultBackend.resources.limits.cpu".to_string(),
                        value: self.default_backend_resources.limit_cpu.to_string(),
                    },
                    ChartSetValue {
                        key: "defaultBackend.resources.requests.cpu".to_string(),
                        value: self.default_backend_resources.request_cpu.to_string(),
                    },
                    ChartSetValue {
                        key: "defaultBackend.resources.limits.memory".to_string(),
                        value: self.default_backend_resources.limit_memory.to_string(),
                    },
                    ChartSetValue {
                        key: "defaultBackend.resources.requests.memory".to_string(),
                        value: self.default_backend_resources.request_memory.to_string(),
                    },
                ],
                yaml_files_content: match self.customer_helm_chart_override.clone() {
                    Some(x) => vec![x.to_chart_values_generated()],
                    None => vec![],
                },
                ..Default::default()
            },
            chart_installation_checker: Some(Box::new(NginxIngressChartChecker::new())),
        }
    }
}

#[derive(Clone)]
pub struct NginxIngressChartChecker {}

impl NginxIngressChartChecker {
    pub fn new() -> NginxIngressChartChecker {
        NginxIngressChartChecker {}
    }
}

impl Default for NginxIngressChartChecker {
    fn default() -> Self {
        NginxIngressChartChecker::new()
    }
}

impl ChartInstallationChecker for NginxIngressChartChecker {
    fn verify_installation(&self, _kube_client: &Client) -> Result<(), CommandError> {
        // TODO(ENG-1370): Implement chart install verification
        Ok(())
    }

    fn clone_dyn(&self) -> Box<dyn ChartInstallationChecker> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use crate::cloud_provider::helm_charts::get_helm_path_kubernetes_provider_sub_folder_name;
    use crate::cloud_provider::helm_charts::nginx_ingress_chart::NginxIngressChart;
    use crate::cloud_provider::helm_charts::HelmChartResourcesConstraintType;
    use crate::cloud_provider::helm_charts::HelmChartType;
    use crate::cloud_provider::models::CustomerHelmChartsOverride;
    use std::env;
    use std::sync::Arc;

    fn get_nginx_ingress_chart_override() -> Arc<dyn Fn(String) -> Option<CustomerHelmChartsOverride>> {
        Arc::new(|_chart_name: String| -> Option<CustomerHelmChartsOverride> {
            Some(CustomerHelmChartsOverride {
                chart_name: NginxIngressChart::chart_name(),
                chart_values: "".to_string(),
            })
        })
    }

    /// Makes sure chart directory containing all YAML files exists.
    #[test]
    fn nginx_ingress_chart_directory_exists_test() {
        // setup:
        let chart = NginxIngressChart::new(
            None,
            HelmChartResourcesConstraintType::ChartDefault,
            HelmChartResourcesConstraintType::ChartDefault,
            true,
            get_nginx_ingress_chart_override(),
        );

        let current_directory = env::current_dir().expect("Impossible to get current directory");
        let chart_path = format!(
            "{}/lib/{}/bootstrap/charts/{}/Chart.yaml",
            current_directory
                .to_str()
                .expect("Impossible to convert current directory to string"),
            get_helm_path_kubernetes_provider_sub_folder_name(chart.chart_path.helm_path(), HelmChartType::Shared),
            NginxIngressChart::chart_name(),
        );

        // execute
        let values_file = std::fs::File::open(&chart_path);

        // verify:
        assert!(values_file.is_ok(), "Chart directory should exist: `{chart_path}`");
    }

    // Makes sure chart values file exists.
    // todo:(pmavro): fix it
    // #[test]
    // fn nginx_ingress_chart_values_file_exists_test() {
    //     // setup:
    //     let chart = NginxIngressChart::new(
    //         None,
    //         HelmChartResourcesConstraintType::ChartDefault,
    //         HelmChartResourcesConstraintType::ChartDefault,
    //         true,
    //         get_nginx_ingress_chart_override(),
    //     );

    //     let current_directory = env::current_dir().expect("Impossible to get current directory");
    //     let chart_values_path = format!(
    //         "{}/lib/{}/bootstrap/chart_values/{}.yaml",
    //         current_directory
    //             .to_str()
    //             .expect("Impossible to convert current directory to string"),
    //         get_helm_path_kubernetes_provider_sub_folder_name(
    //             chart.chart_values_path.helm_path(),
    //             HelmChartType::Shared
    //         ),
    //         NginxIngressChart::chart_name(),
    //     );

    //     // execute
    //     let values_file = std::fs::File::open(&chart_values_path);

    //     // verify:
    //     assert!(values_file.is_ok(), "Chart values file should exist: `{chart_values_path}`");
    // }

    // Make sure rust code doesn't set a value not declared inside values file.
    // All values should be declared / set in values file unless it needs to be injected via rust code.
    // todo(pmavro): fix it
    // #[test]
    // fn nginx_ingress_chart_rust_overridden_values_exists_in_values_yaml_test() {
    //     // setup:
    //     let chart = NginxIngressChart::new(
    //         None,
    //         HelmChartResourcesConstraintType::ChartDefault,
    //         HelmChartResourcesConstraintType::ChartDefault,
    //         true,
    //         get_nginx_ingress_chart_override(),
    //     );
    //     let chart_values_file_path = chart.chart_values_path.helm_path().clone();

    //     let missing_fields = get_helm_values_set_in_code_but_absent_in_values_file(
    //         CommonChart {
    //             // just fake to mimic common chart for test
    //             chart_info: chart.to_common_helm_chart().chart_info,
    //             ..Default::default()
    //         },
    //         format!(
    //             "/lib/{}/bootstrap/chart_values/{}.j2.yaml",
    //             get_helm_path_kubernetes_provider_sub_folder_name(
    //                 &chart_values_file_path,
    //                 HelmChartType::CloudProviderSpecific(KubernetesKind::Eks),
    //             ),
    //             NginxIngressChart::chart_old_name(),
    //         ),
    //     );

    //     // verify:
    //     assert!(missing_fields.is_none(), "Some fields are missing in values file, add those (make sure they still exist in chart values), fields: {}", missing_fields.unwrap_or_default().join(","));
    //}
}