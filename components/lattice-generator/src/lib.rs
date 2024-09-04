wit_bindgen::generate!({ world: "lattice-generator", generate_all});

mod bindings {
    wit_bindgen::generate!({world: "interfaces", with: {"wasmcloud:secrets/store@0.1.0-draft": generate, "wasmcloud:secrets/reveal@0.1.0-draft": generate}});
}

use bindings::wasmcloud::control::lattice_account_update;
use exports::wasmcloud::control::lattice_lifecycle::*;
use nats_jwt::{
    account::{Account, AccountLimits, JetStreamLimits, OperatorLimits},
    activation::Activation,
    types::{
        ExportType, Import, NatsLimits, Permission, Permissions, ResponsePermission, SigningKey,
        UserScope,
    },
    user::UserPermissionLimits,
    Claims,
};
use nkeys::KeyPair;
use wasi::random::random::get_random_bytes;
use wasmcloud::control::lattice_types::LifecycleErrorKind;

struct Generator {}

fn get_secret(key: &str) -> Result<String, LatticeLifecycleError> {
    let secret =
        bindings::wasmcloud::secrets::store::get(key).map_err(|e| LatticeLifecycleError {
            kind: LifecycleErrorKind::Internal,
            message: Some(format!("Failed to get operator key: {}", e)),
        })?;
    let revealed = match bindings::wasmcloud::secrets::reveal::reveal(&secret) {
        bindings::wasmcloud::secrets::store::SecretValue::String(s) => s,
        bindings::wasmcloud::secrets::store::SecretValue::Bytes(_) => {
            return Err(LatticeLifecycleError {
                kind: LifecycleErrorKind::Internal,
                message: Some("Operator key is not a string".to_string()),
            })
        }
    };
    if revealed.is_empty() {
        return Err(LatticeLifecycleError {
            kind: LifecycleErrorKind::Internal,
            message: Some("Operator key is empty".to_string()),
        });
    }
    Ok(revealed)
}

impl Guest for Generator {
    fn create(context: Context) -> Result<CreateLatticeResponse, LatticeLifecycleError> {
        // Assert that all of the required fields are present
        if context.lattice.account.is_some() {
            return Ok(CreateLatticeResponse {
                lattice: context.lattice,
                account: None,
            });
        }
        if context.system_account.is_none() {
            return Err(LatticeLifecycleError {
                kind: LifecycleErrorKind::InvalidArgument,
                message: Some("System account not provided".to_string()),
            });
        }
        if context.operator_key.is_none() {
            return Err(LatticeLifecycleError {
                kind: LifecycleErrorKind::InvalidArgument,
                message: Some("Operator key not provided".to_string()),
            });
        }

        let mut l2 = context.lattice.clone();

        // Generate account keypair, signing key
        let random_bytes = get_random_bytes(32);
        let kp = KeyPair::new_from_raw(
            nkeys::KeyPairType::Account,
            random_bytes.as_slice().try_into().unwrap(),
        )
        .map_err(|e| LatticeLifecycleError {
            kind: LifecycleErrorKind::Internal,
            message: Some(format!("Failed to create account keypair: {}", e)),
        })?;
        let random_bytes = get_random_bytes(32);
        let signing_key = KeyPair::new_from_raw(
            nkeys::KeyPairType::Account,
            random_bytes.as_slice().try_into().unwrap(),
        )
        .map_err(|e| LatticeLifecycleError {
            kind: LifecycleErrorKind::Internal,
            message: Some(format!("Failed to create signing keypair: {}", e)),
        })?;
        let signing_key_seed = signing_key.seed().map_err(|e| LatticeLifecycleError {
            kind: LifecycleErrorKind::Internal,
            message: Some(format!("Failed to generate signing key seed: {}", e)),
        })?;

        let account_public = kp.public_key();
        let mut account = Account::new_claims(
            context.lattice.metadata.name.clone(),
            account_public.clone(),
        );
        l2.account = Some(account_public.clone());

        // Get signing keys for the operator account and the WASMCLOUD_SYSTEM account.
        // We need the operator signing key in order to sign the account JWT, and the system
        // account to sign the import activation JWTs.
        let operator_key = get_secret(context.operator_key.as_ref().unwrap())?;
        let operator_keypair =
            KeyPair::from_seed(&operator_key).map_err(|e| LatticeLifecycleError {
                kind: LifecycleErrorKind::Internal,
                message: Some(format!("Failed to create operator keypair: {}", e)),
            })?;

        let system_account = context.system_account.as_ref().unwrap();
        let system_account_seed = get_secret(system_account)?;
        let system_account_keypair =
            KeyPair::from_seed(&system_account_seed).map_err(|e| LatticeLifecycleError {
                kind: LifecycleErrorKind::Internal,
                message: Some(format!("Failed to create system account keypair: {}", e)),
            })?;

        if context.additional_metadata.is_some() {
            account.nats.generic_fields.tags = Some(
                context
                    .additional_metadata
                    .unwrap()
                    .iter()
                    .map(|(k, v)| format!("{}:{}", k, v))
                    .collect::<Vec<String>>(),
            );
        }

        account.nats.signing_keys.insert(SigningKey {
            key: account_public.clone(),
            scope: None,
        });

        // TODO the go JWT lib automatically sets all of these to be unlimited. Not sure if we want
        // to replicate that behavior
        account.nats.limits = Some(OperatorLimits {
            account: Some(AccountLimits {
                disallow_bearer: Some(true),
                imports: Some(-1),
                exports: Some(-1),
                leaf: Some(-1),
                wildcard_exports: Some(true),
                conn: Some(-1),
            }),
            nats: Some(NatsLimits {
                subs: Some(-1),
                data: Some(-1),
                payload: Some(-1),
            }),
            jetstream: Some(JetStreamLimits {
                consumer: Some(-1),
                disk_storage: Some(-1),
                memory_storage: Some(-1),
                streams: Some(-1),
                max_ack_pending: Some(-1),
                mem_max_stream_bytes: Some(-1),
                disk_max_stream_bytes: Some(-1),
                max_bytes_required: Some(false),
            }),
            ..Default::default()
        });

        if context.system_account.is_none() {
            return Ok(CreateLatticeResponse {
                lattice: l2,
                account: None,
            });
        }

        let mut imports = Vec::new();
        let system_account = context.system_account.unwrap();
        imports.push(Import {
            account: system_account.clone(),
            subject: format!(
                "{}.wasmbus.cfg.{}.>",
                account_public.clone(),
                context.lattice.metadata.name
            ),
            local_subject: format!("wasmbus.cfg.{}.>", context.lattice.metadata.name),
            export_type: Some(ExportType::Service),
            ..Default::default()
        });
        imports.push(Import {
            account: system_account.clone(),
            subject: format!(
                "{}.wasmbus.evt.{}.>",
                account_public.clone(),
                context.lattice.metadata.name
            ),
            local_subject: format!("wasmbus.evt.{}.>", context.lattice.metadata.name),
            export_type: Some(ExportType::Service),
            ..Default::default()
        });
        imports.push(Import {
            account: system_account.clone(),
            subject: format!(
                "{}.wasmbus.ctl.{}.>",
                account_public.clone(),
                context.lattice.metadata.name
            ),
            local_subject: format!("wasmbus.ctl.{}.>", context.lattice.metadata.name),
            export_type: Some(ExportType::Service),
            ..Default::default()
        });
        imports.push(Import {
            account: system_account.clone(),
            subject: format!(
                "{}.wasmcloud.secrets.{}.>",
                account_public.clone(),
                context.lattice.metadata.name
            ),
            local_subject: "wasmcloud.secrets.>".to_string(),
            export_type: Some(ExportType::Service),
            ..Default::default()
        });
        imports.push(Import {
            account: system_account.clone(),
            subject: "wasmcloud.lattice.list".to_string(),
            export_type: Some(ExportType::Service),
            ..Default::default()
        });
        imports.push(Import {
            account: system_account.clone(),
            subject: "wasmcloud.lattice.watch".to_string(),
            export_type: Some(ExportType::Stream),
            ..Default::default()
        });

        for mut import in imports {
            let mut activation = Activation::new_claims(import.subject.clone(), kp.public_key());
            activation.nats.issuer_account = system_account.clone();
            activation.nats.import_subject = import.subject.clone();

            let token =
                activation
                    .encode(&system_account_keypair)
                    .map_err(|e| LatticeLifecycleError {
                        kind: LifecycleErrorKind::Internal,
                        message: Some(format!("Failed to encode activation: {}", e)),
                    })?;
            import.token = token;
        }

        let mut signing_keys =
            Generator::scoped_signers(&mut account, context.lattice.metadata.name.as_str())
                .map_err(|e| LatticeLifecycleError {
                    kind: LifecycleErrorKind::Internal,
                    message: Some(format!("Failed to generate signing keys: {}", e)),
                })?;
        signing_keys.push(signing_key_seed);
        l2.signing_keys = Some(
            signing_keys
                .iter()
                .map(|k| KeyPair::from_seed(k).unwrap().public_key())
                .collect(),
        );

        // TODO validate

        let encoded = account
            .encode(&operator_keypair)
            .map_err(|e| LatticeLifecycleError {
                kind: LifecycleErrorKind::Internal,
                message: Some(format!("Failed to encode account: {}", e)),
            })?;

        // Push the new JWT to the cluster
        lattice_account_update::update(encoded.as_str()).map_err(|e| LatticeLifecycleError {
            kind: LifecycleErrorKind::Internal,
            message: Some(format!("Failed to update account: {}", e)),
        })?;

        let account_seed = kp.seed().map_err(|e| LatticeLifecycleError {
            kind: LifecycleErrorKind::Internal,
            message: Some(format!("Failed to generate account seed: {}", e)),
        })?;

        Ok(CreateLatticeResponse {
            lattice: l2,
            account: Some(GeneratedAccount {
                signing_keys,
                seed: account_seed,
                encoded,
            }),
        })
    }

    // No-op
    fn update(context: Context) -> Result<Lattice, LatticeLifecycleError> {
        Ok(context.lattice)
    }

    // TODO nothing to do for now, but we should delete the account jwt and keys from the backing
    // KV store. That would require binding to a capability to do that in here.
    fn delete(lattice: Lattice) -> Result<Lattice, LatticeLifecycleError> {
        Ok(lattice)
    }
}

impl Generator {
    // This is really side ffecty which isn't great
    fn scoped_signers(account: &mut Claims<Account>, name: &str) -> anyhow::Result<Vec<String>> {
        let random_bytes = get_random_bytes(32);
        let user_key = KeyPair::new_from_raw(
            nkeys::KeyPairType::Account,
            random_bytes.as_slice().try_into().unwrap(),
        )
        .map_err(|e| LatticeLifecycleError {
            kind: LifecycleErrorKind::Internal,
            message: Some(format!("Failed to create account keypair: {}", e)),
        })?;
        let random_bytes = get_random_bytes(32);
        let rpc_key = KeyPair::new_from_raw(
            nkeys::KeyPairType::Account,
            random_bytes.as_slice().try_into().unwrap(),
        )
        .map_err(|e| LatticeLifecycleError {
            kind: LifecycleErrorKind::Internal,
            message: Some(format!("Failed to create account keypair: {}", e)),
        })?;

        // TODO builder pattern would be great for these sorts of things
        let mut user_scope = UserScope {
            key: user_key.public_key(),
            role: Some("user".to_string()),
            ..Default::default()
        };
        let template = UserPermissionLimits {
            permissions: Permissions {
                publish: Permission {
                    allow: vec![
                        "wasmbus.cfg.{{account-name()}}.>".to_string(),
                        "wasmbus.evt.{{account-name()}}.>".to_string(),
                        // TODO does this work? It should probably match the others
                        format!("wasmbus.ctl.*.{}.>", name),
                        "wadm.api.{{account-name()}}.>".to_string(),
                        "wasmcloud.secrets.>".to_string(),
                        "wasmcloud.lattice.list".to_string(),
                        "_INBOX.>".to_string(),
                    ],
                    ..Default::default()
                },
                subscribe: Permission {
                    allow: vec![
                        "wasmbus.cfg.{{account-name()}}.>".to_string(),
                        "wasmbus.evt.{{account-name()}}.>".to_string(),
                        "wasmbus.ctl.*.{{account-name()}}.>".to_string(),
                        "wadm.api.{{account-name()}}.>".to_string(),
                        "{{account-name()}}.*.wrpc.>".to_string(),
                        "wasmcloud.lattice.watch".to_string(),
                        "_INBOX.>".to_string(),
                    ],
                    ..Default::default()
                },
                resp: Some(ResponsePermission {
                    max_messages: -1,
                    ..Default::default()
                }),
            },
            ..Default::default()
        };
        user_scope.template = Some(template);

        let rpc_scope = UserScope {
            key: rpc_key.public_key(),
            role: Some("rpc".to_string()),
            template: Some(UserPermissionLimits {
                permissions: Permissions {
                    publish: Permission {
                        allow: vec![
                            "{{account-name()}}.*.wrpc.>".to_string(),
                            "_INBOX.>".to_string(),
                            "$JS.>".to_string(),
                        ],
                        ..Default::default()
                    },
                    subscribe: Permission {
                        allow: vec![
                            "{{account-name()}}.*.wrpc.>".to_string(),
                            "_INBOX.>".to_string(),
                            "$JS.>".to_string(),
                        ],
                        ..Default::default()
                    },
                    resp: Some(ResponsePermission {
                        max_messages: -1,
                        ..Default::default()
                    }),
                },
                ..Default::default()
            }),
            ..Default::default()
        };

        account.nats.signing_keys.insert(SigningKey {
            key: user_key.public_key(),
            scope: Some(user_scope),
        });
        account.nats.signing_keys.insert(SigningKey {
            key: rpc_key.public_key(),
            scope: Some(rpc_scope),
        });
        Ok(vec![user_key.seed()?, rpc_key.seed()?])
    }
}

export!(Generator);
