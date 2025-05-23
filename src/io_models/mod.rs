use crate::engine_task::qovery_api::QoveryApi;
use crate::infrastructure::models::build_platform::{Credentials, SshKey};
use crate::infrastructure::models::cloud_provider::service;
use crate::infrastructure::models::cloud_provider::service::ServiceType;
use crate::io_models::variable_utils::VariableInfo;
use crate::utilities::{to_qovery_name, to_short_id};
use base64::Engine;
use base64::engine::general_purpose;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use uuid::Uuid;

pub mod annotations_group;
pub mod application;
mod azure;
pub mod container;
pub mod context;
pub mod database;
pub mod engine_location;
pub mod engine_request;
pub mod environment;
mod gke;
pub mod helm_chart;
pub mod job;
pub mod labels_group;
pub mod metrics;
pub mod models;
pub mod probe;
pub mod router;
pub mod terraform_service;
mod types;
pub mod variable_utils;

#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize, Hash, Debug, Default)]
pub enum UpdateStrategy {
    #[default]
    RollingUpdate,
    Recreate,
}

#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, Default)]
pub enum PodAntiAffinity {
    #[default]
    Preferred,
    Required,
}

#[derive(thiserror::Error, Clone, Debug, PartialEq)]
pub enum QoveryIdentifierError {
    #[error("Error while parsing Qovery identifier: {raw_error_message}")]
    ParsingError { raw_error_message: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QoveryIdentifier {
    long_id: Uuid,
    short: String,
    name: String,
}

impl QoveryIdentifier {
    pub fn new(long_id: Uuid) -> Self {
        QoveryIdentifier {
            long_id,
            short: to_short_id(&long_id),
            name: to_qovery_name(&long_id),
        }
    }

    pub fn new_random() -> Self {
        Self::new(Uuid::new_v4())
    }

    pub fn short(&self) -> &str {
        &self.short
    }

    pub fn qovery_resource_name(&self) -> &str {
        &self.name
    }

    pub fn to_uuid(&self) -> Uuid {
        self.long_id
    }
}

impl FromStr for QoveryIdentifier {
    type Err = QoveryIdentifierError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(QoveryIdentifier::new(Uuid::parse_str(s).map_err(|e| {
            QoveryIdentifierError::ParsingError {
                raw_error_message: e.to_string(),
            }
        })?))
    }
}

impl Default for QoveryIdentifier {
    fn default() -> Self {
        QoveryIdentifier::new_random()
    }
}

impl Display for QoveryIdentifier {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.long_id.to_string().as_str())
    }
}

#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Action {
    Create,
    Pause,
    Delete,
    Restart,
}

impl Action {
    pub fn to_service_action(&self) -> service::Action {
        match self {
            Action::Create => service::Action::Create,
            Action::Pause => service::Action::Pause,
            Action::Delete => service::Action::Delete,
            Action::Restart => service::Action::Restart,
        }
    }
}
impl Display for Action {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Action::Create => write!(f, "create"),
            Action::Pause => write!(f, "pause"),
            Action::Delete => write!(f, "delete"),
            Action::Restart => write!(f, "restart"),
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, Eq, PartialEq, Hash)]
pub struct MountedFile {
    pub id: String,
    pub long_id: Uuid,
    pub mount_path: String,
    pub file_content_b64: String,
}

impl MountedFile {
    pub fn to_domain(&self) -> models::MountedFile {
        models::MountedFile {
            id: self.id.to_string(),
            long_id: self.long_id,
            mount_path: self.mount_path.to_string(),
            file_content_b64: self.file_content_b64.to_string(),
        }
    }
}

// TODO(bchastanier): this should probably be structured better than a string in the future
#[derive(Clone)]
pub struct NginxConfigurationServerSnippet(String);

impl NginxConfigurationServerSnippet {
    pub fn new(snippet: String) -> Self {
        NginxConfigurationServerSnippet(snippet)
    }

    pub fn get_snippet_value(&self) -> &str {
        &self.0
    }
}

pub enum RateLimiting {
    Enabled {
        max_requests_per_minute: u32,
        burst_multiplier: u32,
    },
    Disabled,
}

// Retrieve ssh keys from env variables, values are base64 encoded
pub fn ssh_keys_from_env_vars(environment_vars: &BTreeMap<String, VariableInfo>) -> Vec<SshKey> {
    // Retrieve ssh keys from env variables
    const ENV_GIT_PREFIX: &str = "GIT_SSH_KEY";
    let env_ssh_keys: Vec<(String, String)> = environment_vars
        .iter()
        .filter_map(|(name, variable_infos)| {
            if name.starts_with(ENV_GIT_PREFIX) {
                Some((name.clone(), variable_infos.value.clone()))
            } else {
                None
            }
        })
        .collect();

    // Get passphrase and public key if provided by the user
    let mut ssh_keys: Vec<SshKey> = Vec::with_capacity(env_ssh_keys.len());
    for (ssh_key_name, private_key) in env_ssh_keys {
        let private_key =
            if let Ok(Ok(private_key)) = general_purpose::STANDARD.decode(private_key).map(String::from_utf8) {
                private_key
            } else {
                error!("Invalid base64 environment variable for {}", ssh_key_name);
                continue;
            };

        let passphrase = environment_vars
            .get(&ssh_key_name.replace(ENV_GIT_PREFIX, "GIT_SSH_PASSPHRASE"))
            .and_then(|variable_infos| general_purpose::STANDARD.decode(variable_infos.value.clone()).ok())
            .and_then(|str| String::from_utf8(str).ok());

        let public_key = environment_vars
            .get(&ssh_key_name.replace(ENV_GIT_PREFIX, "GIT_SSH_PUBLIC_KEY"))
            .and_then(|variable_infos| general_purpose::STANDARD.decode(variable_infos.value.clone()).ok())
            .and_then(|str| String::from_utf8(str).ok());

        ssh_keys.push(SshKey {
            private_key,
            passphrase,
            public_key,
        });
    }

    ssh_keys
}

// Convert our root path to an relative path to be able to append them correctly
pub fn normalize_root_and_dockerfile_path(
    root_path: &str,
    dockerfile_path: &Option<String>,
) -> (PathBuf, Option<PathBuf>) {
    let root_path = if Path::new(&root_path).is_absolute() {
        PathBuf::from(root_path.trim_start_matches('/'))
    } else {
        PathBuf::from(&root_path)
    };
    assert!(root_path.is_relative(), "root path is not a relative path");

    let dockerfile_path = dockerfile_path.as_ref().map(|path| {
        if Path::new(&path).is_absolute() {
            root_path.join(path.trim_start_matches('/'))
        } else {
            root_path.join(path)
        }
    });

    (root_path, dockerfile_path)
}

pub fn fetch_git_token(
    qovery_api: &dyn QoveryApi,
    service_type: ServiceType,
    service_id: &Uuid,
) -> anyhow::Result<Credentials> {
    let creds = match qovery_api.git_token(service_type, service_id) {
        Ok(creds) => creds,
        Err(err) => {
            error!("Unable to get git credentials for {:?}({}): {}", service_type, service_id, err);
            return Err(err);
        }
    };

    Ok(Credentials {
        login: creds.login,
        password: creds.access_token,
    })
}

pub fn sanitized_git_url(git_url: &str) -> String {
    let sanitized_git_url = git_url
        .to_ascii_lowercase()
        .replace(|c: char| !c.is_ascii_alphanumeric(), "-");
    Regex::new(r"-+")
        .unwrap()
        .replace_all(&sanitized_git_url, "-")
        .to_string()
}
