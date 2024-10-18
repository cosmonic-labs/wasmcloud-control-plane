pub mod lattice_server {
    use super::lattice_generator;

    wit_bindgen_wrpc::generate!("lattice-server");

    impl From<lattice_generator::wasmcloud::control::lattice_types::Lattice>
        for wasmcloud::control::lattice_types::Lattice
    {
        fn from(
            lattice: lattice_generator::wasmcloud::control::lattice_types::Lattice,
        ) -> wasmcloud::control::lattice_types::Lattice {
            wasmcloud::control::lattice_types::Lattice {
                account: lattice.account,
                signing_keys: lattice.signing_keys,
                description: lattice.description,
                deletable: lattice.deletable,
                metadata: lattice.metadata.into(),
                status: lattice.status.into(),
            }
        }
    }

    impl From<lattice_generator::wasmcloud::control::lattice_types::Metadata>
        for wasmcloud::control::lattice_types::Metadata
    {
        fn from(
            metadata: lattice_generator::wasmcloud::control::lattice_types::Metadata,
        ) -> wasmcloud::control::lattice_types::Metadata {
            wasmcloud::control::lattice_types::Metadata {
                name: metadata.name,
                version: metadata.version,
                uid: metadata.uid,
                annotations: metadata.annotations,
                labels: metadata.labels,
            }
        }
    }

    impl From<lattice_generator::wasmcloud::control::lattice_types::LatticeStatus>
        for wasmcloud::control::lattice_types::LatticeStatus
    {
        fn from(
            status: lattice_generator::wasmcloud::control::lattice_types::LatticeStatus,
        ) -> wasmcloud::control::lattice_types::LatticeStatus {
            wasmcloud::control::lattice_types::LatticeStatus {
            phase: match status.phase {
                lattice_generator::wasmcloud::control::lattice_types::LatticePhase::Provisioning => {
                    wasmcloud::control::lattice_types::LatticePhase::Provisioning
                }
                lattice_generator::wasmcloud::control::lattice_types::LatticePhase::Created => {
                    wasmcloud::control::lattice_types::LatticePhase::Created
                }
                lattice_generator::wasmcloud::control::lattice_types::LatticePhase::Deleting => {
                    wasmcloud::control::lattice_types::LatticePhase::Deleting
                }
            },
        }
        }
    }
}

pub mod imports {
    wit_bindgen_wrpc::generate!("imports");
}

pub mod lattice_generator {
    use super::lattice_server;
    use std::convert::{From, Into};
    use wasmcloud_api_types::{Lattice, LatticePhase, LatticeSpec, LatticeStatus, Metadata};

    wasmtime::component::bindgen!({async: true, world: "lattice-generator"});

    impl From<lattice_server::wasmcloud::control::lattice_types::Lattice>
        for wasmcloud::control::lattice_types::Lattice
    {
        fn from(
            lattice: lattice_server::wasmcloud::control::lattice_types::Lattice,
        ) -> wasmcloud::control::lattice_types::Lattice {
            wasmcloud::control::lattice_types::Lattice {
                account: lattice.account,
                signing_keys: lattice.signing_keys,
                description: lattice.description,
                deletable: lattice.deletable,
                metadata: lattice.metadata.into(),
                status: lattice.status.into(),
            }
        }
    }

    impl From<lattice_server::wasmcloud::control::lattice_types::Metadata>
        for wasmcloud::control::lattice_types::Metadata
    {
        fn from(
            metadata: lattice_server::wasmcloud::control::lattice_types::Metadata,
        ) -> wasmcloud::control::lattice_types::Metadata {
            wasmcloud::control::lattice_types::Metadata {
                name: metadata.name,
                version: metadata.version,
                uid: metadata.uid,
                annotations: metadata.annotations,
                labels: metadata.labels,
            }
        }
    }

    impl From<lattice_server::wasmcloud::control::lattice_types::LatticeStatus>
        for wasmcloud::control::lattice_types::LatticeStatus
    {
        fn from(
            status: lattice_server::wasmcloud::control::lattice_types::LatticeStatus,
        ) -> wasmcloud::control::lattice_types::LatticeStatus {
            wasmcloud::control::lattice_types::LatticeStatus {
            phase: match status.phase {
                lattice_server::wasmcloud::control::lattice_types::LatticePhase::Provisioning => {
                    wasmcloud::control::lattice_types::LatticePhase::Provisioning
                }
                lattice_server::wasmcloud::control::lattice_types::LatticePhase::Created => {
                    wasmcloud::control::lattice_types::LatticePhase::Created
                }
                lattice_server::wasmcloud::control::lattice_types::LatticePhase::Deleting => {
                    wasmcloud::control::lattice_types::LatticePhase::Deleting
                }
            },
          }
        }
    }

    impl Into<Lattice> for wasmcloud::control::lattice_types::Lattice {
        fn into(self) -> Lattice {
            Lattice {
                meta: Some(self.metadata.into()),
                spec: Some(LatticeSpec {
                    account: self.account.unwrap_or_default(),
                    signing_keys: self.signing_keys.unwrap_or_default(),
                    description: self.description.unwrap_or_default(),
                    status: Some(self.status.into()),
                    deletable: self.deletable,
                }),
            }
        }
    }

    impl Into<Metadata> for wasmcloud::control::lattice_types::Metadata {
        fn into(self) -> Metadata {
            Metadata {
                name: self.name,
                version: self.version,
                uid: self.uid,
                annotations: self.annotations.into_iter().collect(),
                labels: self.labels.into_iter().collect(),
            }
        }
    }

    impl Into<LatticeStatus> for wasmcloud::control::lattice_types::LatticeStatus {
        fn into(self) -> LatticeStatus {
            let phase = match self.phase {
                wasmcloud::control::lattice_types::LatticePhase::Provisioning => {
                    LatticePhase::LatticeProvisioning
                }
                wasmcloud::control::lattice_types::LatticePhase::Created => {
                    LatticePhase::LatticeCreated
                }
                wasmcloud::control::lattice_types::LatticePhase::Deleting => {
                    LatticePhase::LatticeDeleting
                }
            };
            LatticeStatus {
                phase: phase.into(),
            }
        }
    }

    impl From<Lattice> for wasmcloud::control::lattice_types::Lattice {
        fn from(lattice: Lattice) -> wasmcloud::control::lattice_types::Lattice {
            let metadata = lattice.meta.unwrap_or_default();
            let spec = lattice.spec.unwrap_or_default();
            wasmcloud::control::lattice_types::Lattice {
                metadata: metadata.into(),
                account: {
                    match spec.account.as_str() {
                        "" => None,
                        s => Some(s.to_string()),
                    }
                },
                signing_keys: {
                    match spec.signing_keys.is_empty() {
                        true => None,
                        false => Some(spec.signing_keys),
                    }
                },
                description: {
                    match spec.description.as_str() {
                        "" => None,
                        s => Some(s.to_string()),
                    }
                },
                deletable: spec.deletable,
                status: spec.status.unwrap_or_default().into(),
            }
        }
    }

    impl From<Metadata> for wasmcloud::control::lattice_types::Metadata {
        fn from(metadata: Metadata) -> wasmcloud::control::lattice_types::Metadata {
            wasmcloud::control::lattice_types::Metadata {
                name: metadata.name,
                version: metadata.version,
                uid: metadata.uid,
                annotations: metadata.annotations.into_iter().collect(),
                labels: metadata.labels.into_iter().collect(),
            }
        }
    }

    impl From<LatticeStatus> for wasmcloud::control::lattice_types::LatticeStatus {
        fn from(status: LatticeStatus) -> wasmcloud::control::lattice_types::LatticeStatus {
            let phase =
                LatticePhase::from_i32(status.phase).unwrap_or(LatticePhase::LatticeUnspecified);
            wasmcloud::control::lattice_types::LatticeStatus {
                phase: match phase {
                    LatticePhase::LatticeProvisioning => {
                        wasmcloud::control::lattice_types::LatticePhase::Provisioning
                    }
                    LatticePhase::LatticeCreated => {
                        wasmcloud::control::lattice_types::LatticePhase::Created
                    }
                    LatticePhase::LatticeDeleting => {
                        wasmcloud::control::lattice_types::LatticePhase::Deleting
                    }
                    // TODO this is a bug waiting to happen, fix it
                    LatticePhase::LatticeUnspecified => {
                        wasmcloud::control::lattice_types::LatticePhase::Provisioning
                    }
                },
            }
        }
    }
}
