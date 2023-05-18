use crate::cloud_provider::service::DatabaseType;
use crate::cloud_provider::Kind;
use crate::models::database::{DatabaseError, DatabaseInstanceType};
use std::fmt::{Display, Formatter};
use std::str::FromStr;
use strum_macros::EnumIter;

/// DO NOT MANUALLY EDIT THIS FILE. IT IS AUTO-GENERATED BY INSTANCES FETCHER APP
/// https://gitlab.com/qovery/backend/rust-backend/instances-fetcher/src/lib/rust_generator.rs
#[derive(Debug, Clone, PartialEq, Eq, EnumIter)]
#[allow(non_camel_case_types)]
pub enum ScwDatabaseInstanceType {
    DB_DEV_S,
    DB_DEV_M,
    DB_GP_XS,
    DB_GP_S,
    DB_GP_M,
    RED1_MICRO,
}

impl DatabaseInstanceType for ScwDatabaseInstanceType {
    fn cloud_provider(&self) -> Kind {
        Kind::Scw
    }

    fn to_cloud_provider_format(&self) -> String {
        match self {
            ScwDatabaseInstanceType::DB_DEV_S => "db-dev-s",
            ScwDatabaseInstanceType::DB_DEV_M => "db-dev-m",
            ScwDatabaseInstanceType::DB_GP_XS => "db-gp-xs",
            ScwDatabaseInstanceType::DB_GP_S => "db-gp-s",
            ScwDatabaseInstanceType::DB_GP_M => "db-gp-m",
            ScwDatabaseInstanceType::RED1_MICRO => "red1-micro",
        }
        .to_string()
    }

    fn is_instance_allowed(&self) -> bool {
        match self {
            ScwDatabaseInstanceType::DB_DEV_S => true,
            ScwDatabaseInstanceType::DB_DEV_M => true,
            ScwDatabaseInstanceType::DB_GP_XS => true,
            ScwDatabaseInstanceType::DB_GP_S => true,
            ScwDatabaseInstanceType::DB_GP_M => true,
            ScwDatabaseInstanceType::RED1_MICRO => true,
        }
    }

    fn is_instance_compatible_with(&self, database_type: DatabaseType) -> bool {
        match self {
            ScwDatabaseInstanceType::DB_DEV_S
            | ScwDatabaseInstanceType::DB_DEV_M
            | ScwDatabaseInstanceType::DB_GP_XS
            | ScwDatabaseInstanceType::DB_GP_S
            | ScwDatabaseInstanceType::DB_GP_M => match database_type {
                DatabaseType::PostgreSQL => true,
                DatabaseType::MongoDB => true,
                DatabaseType::MySQL => true,
                DatabaseType::Redis => false,
            },
            ScwDatabaseInstanceType::RED1_MICRO => match database_type {
                DatabaseType::PostgreSQL => false,
                DatabaseType::MongoDB => false,
                DatabaseType::MySQL => false,
                DatabaseType::Redis => true,
            },
        }
    }
}

impl Display for ScwDatabaseInstanceType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_cloud_provider_format())
    }
}

impl FromStr for ScwDatabaseInstanceType {
    type Err = DatabaseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "db-dev-s" => Ok(ScwDatabaseInstanceType::DB_DEV_S),
            "db-dev-m" => Ok(ScwDatabaseInstanceType::DB_DEV_M),
            "db-gp-xs" => Ok(ScwDatabaseInstanceType::DB_GP_XS),
            "db-gp-s" => Ok(ScwDatabaseInstanceType::DB_GP_S),
            "db-gp-m" => Ok(ScwDatabaseInstanceType::DB_GP_M),
            "red1-micro" => Ok(ScwDatabaseInstanceType::RED1_MICRO),
            _ => Err(DatabaseError::InvalidDatabaseInstance {
                database_cloud_provider: Kind::Aws,
                requested_database_instance_type: s.to_string(),
            }),
        }
    }
}

#[cfg(test)]
#[rustfmt::skip]
mod tests {
    use crate::cloud_provider::scaleway::database_instance_type::ScwDatabaseInstanceType;
    use crate::cloud_provider::Kind;
    use crate::models::database::DatabaseInstanceType;
    use std::str::FromStr;
    use strum::IntoEnumIterator;
    use crate::cloud_provider::service::DatabaseType;

    #[test]
    fn test_scaleway_database_instance_type_cloud_provider_kind() {
        for instance_type in ScwDatabaseInstanceType::iter() {
            // execute & verify:
            assert_eq!(Kind::Scw, instance_type.cloud_provider())
        }
    }

    #[test]
    fn test_scaleway_database_instance_type_to_cloud_provider_format() {
        for instance_type in ScwDatabaseInstanceType::iter() {
            // execute & verify:
            assert_eq!(
                match instance_type {
                    ScwDatabaseInstanceType::DB_DEV_S => "db-dev-s",
                    ScwDatabaseInstanceType::DB_DEV_M => "db-dev-m",
                    ScwDatabaseInstanceType::DB_GP_XS => "db-gp-xs",
                    ScwDatabaseInstanceType::DB_GP_S => "db-gp-s",
                    ScwDatabaseInstanceType::DB_GP_M => "db-gp-m",
                    ScwDatabaseInstanceType::RED1_MICRO => "red1-micro",
                }
                    .to_string(),
                instance_type.to_cloud_provider_format()
            )
        }
    }

    #[test]
    fn test_scaleway_database_instance_type_to_string() {
        for instance_type in ScwDatabaseInstanceType::iter() {
            // execute & verify:
            assert_eq!(
                match instance_type {
                    ScwDatabaseInstanceType::DB_DEV_S => "db-dev-s",
                    ScwDatabaseInstanceType::DB_DEV_M => "db-dev-m",
                    ScwDatabaseInstanceType::DB_GP_XS => "db-gp-xs",
                    ScwDatabaseInstanceType::DB_GP_S => "db-gp-s",
                    ScwDatabaseInstanceType::DB_GP_M => "db-gp-m",
                    ScwDatabaseInstanceType::RED1_MICRO => "red1-micro",
                }
                    .to_string(),
                instance_type.to_string()
            )
        }
    }

    #[test]
    fn test_scaleway_database_instance_type_from_str() {
        for instance_type in ScwDatabaseInstanceType::iter() {
            // execute & verify:
            // proper string: e.q `db-dev-s`
            assert_eq!(
                Ok(instance_type.clone()),
                ScwDatabaseInstanceType::from_str(&instance_type.to_cloud_provider_format())
            );
            // string with several casing: e.q `DB-DEV-S`
            assert_eq!(
                Ok(instance_type.clone()),
                ScwDatabaseInstanceType::from_str(instance_type.to_cloud_provider_format().to_uppercase().as_str())
            );
            // string with leading and trailing spaces: e.q ` db-dev-s   `
            assert_eq!(
                Ok(instance_type.clone()),
                ScwDatabaseInstanceType::from_str(
                    format!("  {}   ", &instance_type.to_cloud_provider_format()).as_str()
                )
            );
        }
    }

    #[test]
    fn test_scaleway_database_instance_type_is_instance_allowed() {
        for instance_type in ScwDatabaseInstanceType::iter() {
            // execute & verify:
            assert_eq!(
                match instance_type {
                    ScwDatabaseInstanceType::DB_DEV_S => true,
                    ScwDatabaseInstanceType::DB_DEV_M => true,
                    ScwDatabaseInstanceType::DB_GP_XS => true,
                    ScwDatabaseInstanceType::DB_GP_S => true,
                    ScwDatabaseInstanceType::DB_GP_M => true,
                    ScwDatabaseInstanceType::RED1_MICRO => true
                },
                instance_type.is_instance_allowed(),
            )
        }
    }

    #[test]
    fn test_scaleway_database_instance_type_is_instance_compatible_with() {
        for db_type in DatabaseType::iter() {
            for instance_type in ScwDatabaseInstanceType::iter() {
                // execute & verify:
                assert_eq!(
                    match instance_type {
                        // DB
                        ScwDatabaseInstanceType::DB_DEV_S => match db_type {
                            DatabaseType::PostgreSQL => true,
                            DatabaseType::MongoDB => true,
                            DatabaseType::MySQL => true,
                            DatabaseType::Redis => false,
                        },
                        ScwDatabaseInstanceType::DB_DEV_M => match db_type {
                            DatabaseType::PostgreSQL => true,
                            DatabaseType::MongoDB => true,
                            DatabaseType::MySQL => true,
                            DatabaseType::Redis => false,
                        },
                        ScwDatabaseInstanceType::DB_GP_XS => match db_type {
                            DatabaseType::PostgreSQL => true,
                            DatabaseType::MongoDB => true,
                            DatabaseType::MySQL => true,
                            DatabaseType::Redis => false,
                        },
                        ScwDatabaseInstanceType::DB_GP_S => match db_type {
                            DatabaseType::PostgreSQL => true,
                            DatabaseType::MongoDB => true,
                            DatabaseType::MySQL => true,
                            DatabaseType::Redis => false,
                        },
                        ScwDatabaseInstanceType::DB_GP_M => match db_type {
                            DatabaseType::PostgreSQL => true,
                            DatabaseType::MongoDB => true,
                            DatabaseType::MySQL => true,
                            DatabaseType::Redis => false,
                        },
                        // CACHE
                        ScwDatabaseInstanceType::RED1_MICRO => match db_type {
                            DatabaseType::PostgreSQL => false,
                            DatabaseType::MongoDB => false,
                            DatabaseType::MySQL => false,
                            DatabaseType::Redis => true,
                        },
                    },
                    instance_type.is_instance_compatible_with(db_type),
                )
            }
        }
    }
}
