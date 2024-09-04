use anyhow::bail;
use async_nats::{
    jetstream::{
        self,
        kv::{Config as KvConfig, Store},
    },
    Client, ConnectOptions,
};
use clap::Parser;
use nkeys::XKey;
use tokio::task::JoinSet;
use tracing::{error, info};
use wasmcloud_hub::auth;
use wasmcloud_hub::config::{load_config, Args};
use wasmcloud_hub::lattice_wrpc::{LatticeWrpcApi, LATTICE_BUCKET};
use wasmcloud_hub::secrets::SecretsClient;
use wasmcloud_hub::vm;

#[tokio::main]
async fn main() {
    configure_tracing();
    let args = Args::parse();

    let cfg = std::fs::read_to_string(args.config.clone()).unwrap();
    let config = load_config(&cfg).unwrap();

    let core_client = ConnectOptions::new()
        .credentials_file(args.nats_credsfile.as_ref().unwrap().clone())
        .await
        .unwrap()
        .connect(&args.nats_url)
        .await
        .unwrap();

    let sys_client = match &args.nats_sys_credsfile {
        Some(creds) => Some(
            ConnectOptions::new()
                .credentials_file(creds)
                .await
                .unwrap()
                .connect(&args.nats_url)
                .await
                .unwrap(),
        ),
        None => None,
    };

    let engine = vm::configure_wasmtime().unwrap();
    let _bucket = get_lattices_bucket(core_client.clone()).await.unwrap();

    let mut set = JoinSet::new();

    // TODO load the component if it exists and pass the bytes in. We need to use the wasmcloud
    // system account creds
    if let Some(authorization) = &config.authorization {
        let host_auth = auth::AuthCallout::new_from_config(
            authorization.host.clone(),
            &args.nats_url,
            core_client.clone(),
            engine.clone(),
        )
        .await
        .unwrap();

        set.spawn(async move { host_auth.run().await });

        let user_auth = auth::AuthCallout::new_from_config(
            authorization.user.clone(),
            &args.nats_url,
            core_client.clone(),
            engine.clone(),
        )
        .await
        .unwrap();

        set.spawn(async move { user_auth.run().await });
    }

    // TODO figure out how to make this optional
    let encryption_key = std::fs::read_to_string(args.encryption_key_file.as_ref().unwrap())
        .expect("Failed to read encryption key file");
    let enc_key = XKey::from_seed(encryption_key.trim()).unwrap();
    let secrets_client = SecretsClient::new(core_client.clone(), enc_key);

    let lattice_server = LatticeWrpcApi::new(
        config.clone(),
        args.clone(),
        core_client.clone(),
        engine.clone(),
        secrets_client,
        sys_client,
    )
    .await
    .unwrap();

    info!("Starting lattice server");
    set.spawn(async move { lattice_server.run().await });

    if let Some(res) = set.join_next().await {
        error!("Error in lattice server: {:?}", res);
        std::process::exit(1);
    }

    info!("Lattice server stopped");
}

fn configure_tracing() {
    //use tracing_subscriber::prelude::*;
    use tracing_subscriber::EnvFilter;

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
}

async fn get_lattices_bucket(client: Client) -> anyhow::Result<Store> {
    let js = jetstream::new(client);
    let bucket = match js.get_key_value(LATTICE_BUCKET).await {
        Ok(b) => b,
        Err(e) => {
            if e.kind() == jetstream::context::KeyValueErrorKind::GetBucket {
                js.create_key_value(KvConfig {
                    bucket: LATTICE_BUCKET.to_string(),
                    ..Default::default()
                })
                .await?
            } else {
                bail!("Failed to get bucket: {}", e)
            }
        }
    };
    Ok(bucket)
}
