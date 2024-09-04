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
    wasmtime::component::bindgen!({async: true, world: "lattice-generator"});
    // TODO define these somewhere sane, probably in another crate
    //impl From<lattice_server::exports::wasmcloud::control::lattice_service::Lattice>
    //    for wasmcloud::control::lattice_types::Lattice
    //{
    //    fn from(
    //        lattice: lattice_server::exports::wasmcloud::control::lattice_service::Lattice,
    //    ) -> wasmcloud::control::lattice_types::Lattice {
    //        wasmcloud::control::lattice_types::Lattice {
    //            account: lattice.account,
    //            signing_keys: lattice.signing_keys,
    //            description: lattice.description,
    //            deletable: lattice.deletable,
    //            metadata: lattice.metadata.into(),
    //            status: lattice.status.into(),
    //        }
    //    }
    //}

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
}
