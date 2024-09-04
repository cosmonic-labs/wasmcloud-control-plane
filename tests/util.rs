use anyhow::bail;
use async_nats::{
    jetstream::{self, kv::Config as KvConfig, object_store::Config as ObjConfig},
    Client, ConnectOptions,
};
use handlebars::Handlebars;
use nats_jwt::{
    account::{Account, AccountLimits, JetStreamLimits, OperatorLimits},
    operator::Operator,
    types::{
        Export, ExportType, Limits, NatsLimits, Permission, Permissions, ResponsePermission,
        ResponseType, UserLimits,
    },
    user::{User, UserPermissionLimits},
};
use nkeys::KeyPair;
use serde_json::json;
use std::io::Write;
use testcontainers::{
    core::{ImageExt, IntoContainerPort, Mount, WaitFor},
    runners::AsyncRunner,
    ContainerAsync, GenericImage,
};
use wasmcloud_hub::lattice_wrpc;

pub async fn start_nats() -> anyhow::Result<(ContainerAsync<GenericImage>, Client, Client)> {
    let operator_key = KeyPair::new_operator();
    let sys_key = KeyPair::new_account();
    let mut operator = Operator::new_claims("test operator".to_string(), operator_key.public_key());
    operator.nats.system_account = Some(sys_key.public_key());
    let operator_jwt = operator.encode(&operator_key)?;

    let mut sys_account = Account::new_claims("SYS".to_string(), sys_key.public_key());
    sys_account.nats.limits = Some(OperatorLimits {
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
        ..Default::default()
    });

    sys_account.nats.exports = vec![
        Export {
            subject: "$SYS.REQ.ACCOUNT.*.*".to_string(),
            export_type: Some(ExportType::Service),
            response_type: Some(ResponseType::Stream),
            account_token_position: Some(4),
            ..Default::default()
        },
        Export {
            subject: "$SYS.ACCOUNT.*.>".to_string(),
            export_type: Some(ExportType::Stream),
            account_token_position: Some(3),
            ..Default::default()
        },
    ];
    let sys_account_jwt = sys_account.encode(&operator_key)?;

    let user_key = KeyPair::new_user();
    let mut sys_user = User::new_claims("SYS".to_string(), user_key.public_key());
    sys_user.nats.permissions = UserPermissionLimits {
        permissions: Permissions {
            publish: Permission {
                allow: vec![">".to_string()],
                ..Default::default()
            },
            subscribe: Permission {
                allow: vec![">".to_string()],
                ..Default::default()
            },
            resp: Some(ResponsePermission {
                max_messages: -1,
                ..Default::default()
            }),
        },
        limits: Some(Limits {
            nats_limits: Some(NatsLimits {
                subs: Some(-1),
                data: Some(-1),
                payload: Some(-1),
            }),
            user_limits: None,
        }),
        ..Default::default()
    };
    let sys_user_jwt = sys_user.encode(&sys_key)?;

    let template = r#"
operator: {{operator_jwt}}
system_account: {{account_key}}

resolver {
    type: full
    dir: './jwt'
    allow_delete: true
    interval: "2m"
    timeout: "1.9s"
}

resolver_preload: {
  {{account_key}}: {{account_jwt}}
}

jetstream {
    max_mem: 128M
}
"#;
    let tpl = Handlebars::new();
    let resolver = tpl.render_template(template, &json!({ "operator_jwt": operator_jwt, "account_key": sys_key.public_key(), "account_jwt": sys_account_jwt }))
        .map_err(|e| anyhow::anyhow!("Failed to render template: {}", e))?;
    let mut cfg = tempfile::NamedTempFile::new().unwrap();
    cfg.write(resolver.as_bytes())
        .map_err(|e| anyhow::anyhow!("Failed to write config: {}", e))?;

    println!("Config: {}", resolver);

    // A tmpfs mount would be great, but won't work everywhere
    let mount = Mount::bind_mount(cfg.path().to_str().unwrap(), "/nats.cfg");
    let container = GenericImage::new("nats", "2.10")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .with_cmd(vec!["-js", "-c", "/nats.cfg", "-DV"])
        .with_mount(mount)
        .start()
        .await
        .expect("Started NATS");

    let url = format!(
        "nats://localhost:{}",
        container.get_host_port_ipv4(4222).await.unwrap()
    );

    let sys_client = match ConnectOptions::new()
        .jwt(sys_user_jwt, move |nonce| {
            let key_pair = user_key.clone();
            async move { key_pair.sign(&nonce).map_err(async_nats::AuthError::new) }
        })
        .connect(&url)
        .await
    {
        Err(e) => {
            let output = container.stderr_to_vec().await.unwrap();
            println!("NATS output: {}", String::from_utf8_lossy(&output));
            bail!("Failed to connect to NATS: {:?}", e);
        }
        Ok(c) => c,
    };

    let account_key = KeyPair::new_account();
    let mut account = Account::new_claims("test account".to_string(), account_key.public_key());
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
            memory_storage: Some(-1),
            disk_storage: Some(-1),
            streams: Some(-1),
            consumer: Some(-1),
            max_ack_pending: Some(-1),
            mem_max_stream_bytes: Some(-1),
            disk_max_stream_bytes: Some(-1),
            max_bytes_required: Some(false),
        }),
        ..Default::default()
    });
    let account_jwt = account.encode(&operator_key)?;
    //println!("Account JWT: {}", account_jwt);

    let user_key = KeyPair::new_user();
    let mut user = User::new_claims("test user".to_string(), user_key.public_key());
    user.nats.permissions = UserPermissionLimits {
        permissions: Permissions {
            publish: Permission {
                allow: vec![">".to_string()],
                ..Default::default()
            },
            subscribe: Permission {
                allow: vec![">".to_string()],
                ..Default::default()
            },
            resp: Some(ResponsePermission {
                max_messages: -1,
                ..Default::default()
            }),
        },
        limits: Some(Limits {
            nats_limits: Some(NatsLimits {
                subs: Some(-1),
                data: Some(-1),
                payload: Some(-1),
            }),
            user_limits: None,
        }),
        ..Default::default()
    };
    let user_jwt = user.encode(&account_key)?;

    if sys_client
        .request("$SYS.REQ.CLAIMS.UPDATE", account_jwt.into())
        .await
        .is_err()
    {
        let output = container.stderr_to_vec().await.unwrap();
        println!("NATS output: {}", String::from_utf8_lossy(&output));
    }

    let client = ConnectOptions::new()
        .jwt(user_jwt, move |nonce| {
            let key_pair = user_key.clone();
            async move { key_pair.sign(&nonce).map_err(async_nats::AuthError::new) }
        })
        .connect(&url)
        .await?;

    Ok((container, sys_client, client))
}

// TODO add auth to this for completeness
pub async fn bootstrap_nats(client: Client) -> anyhow::Result<()> {
    let js = jetstream::new(client.clone());

    match js.get_key_value(lattice_wrpc::LATTICE_BUCKET).await {
        Ok(b) => b,
        Err(e) => {
            if e.kind() == jetstream::context::KeyValueErrorKind::GetBucket {
                js.create_key_value(KvConfig {
                    bucket: lattice_wrpc::LATTICE_BUCKET.to_string(),
                    storage: jetstream::stream::StorageType::Memory,
                    ..Default::default()
                })
                .await?
            } else {
                bail!("Failed to get bucket: {}", e)
            }
        }
    };

    match js.get_object_store(lattice_wrpc::OBJ_BUCKET).await {
        Ok(b) => b,
        Err(e) => {
            if e.kind() == jetstream::context::ObjectStoreErrorKind::GetStore {
                js.create_object_store(ObjConfig {
                    bucket: lattice_wrpc::OBJ_BUCKET.to_string(),
                    storage: jetstream::stream::StorageType::Memory,
                    ..Default::default()
                })
                .await?
            } else {
                bail!("Failed to get bucket: {}", e)
            }
        }
    };

    Ok(())
}

pub async fn store_component(
    name: &str,
    path: String,
    bucket: String,
    client: Client,
) -> anyhow::Result<()> {
    let mut data = tokio::fs::File::open(path).await?;
    let js = jetstream::new(client.clone());

    let store = js.get_object_store(bucket).await?;
    store.put(name, &mut data).await?;

    Ok(())
}
