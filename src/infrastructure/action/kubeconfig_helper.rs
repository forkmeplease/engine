use crate::errors::{CommandError, EngineError};
use crate::events::EventDetails;
use std::fs;
use std::fs::File;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub fn write_kubeconfig_on_disk(
    kubeconfig_path: &Path,
    kubeconfig: &str,
    event_details: EventDetails,
) -> Result<(), Box<EngineError>> {
    fs::create_dir_all(
        kubeconfig_path
            .parent()
            .expect("Couldn't create kubeconfig folder parent path"),
    )
    .map_err(|err| EngineError::new_cannot_create_file(event_details.clone(), err.into()))?;

    let mut file = File::create(kubeconfig_path)
        .map_err(|err| EngineError::new_cannot_create_file(event_details.clone(), err.into()))?;
    file.write_all(kubeconfig.as_bytes())
        .map_err(|err| EngineError::new_cannot_write_file(event_details.clone(), err.into()))?;

    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(err) => {
            let error = EngineError::new_cannot_retrieve_cluster_config_file(
                event_details.clone(),
                CommandError::new("Error getting file metadata.".to_string(), Some(err.to_string()), None),
            );
            return Err(Box::new(error));
        }
    };

    let max_size = 16 * 1024;
    if metadata.len() > max_size {
        return Err(Box::new(EngineError::new_kubeconfig_size_security_check_error(
            event_details.clone(),
            metadata.len(),
            max_size,
        )));
    };

    let mut permissions = metadata.permissions();
    permissions.set_mode(0o600);
    if let Err(err) = file.set_permissions(permissions) {
        let error = EngineError::new_cannot_retrieve_cluster_config_file(
            event_details.clone(),
            CommandError::new("Error getting file permissions.".to_string(), Some(err.to_string()), None),
        );
        return Err(Box::new(error));
    }

    Ok(())
}
