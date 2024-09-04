use crate::util::*;
use crossbeam_utils::Backoff;
use nkeys::{KeyPair, XKey};
use wasmcloud_hub::bindings::imports::wasmcloud::control::{lattice_service, lattice_types::*};
use wasmcloud_hub::config::{
    Args, Component, ComponentSource, Config, LatticeGenerator, NatsSource,
};
use wasmcloud_hub::lattice_wrpc::{LatticeWrpcApi, LATTICE_BUCKET, LATTICE_SUBJECT, OBJ_BUCKET};
use wasmcloud_hub::{secrets::SecretsClient, vm};

#[tokio::test]
async fn test_lattice_generate() {
    let (container, sys_client, client) = start_nats().await.unwrap();

    let path = format!(
        "{}/components/lattice-generator/build/lattice_generator.wasm",
        env!("CARGO_MANIFEST_DIR")
    );

    bootstrap_nats(client.clone()).await.unwrap();

    store_component(
        "generator.wasm",
        path.to_string(),
        OBJ_BUCKET.to_string(),
        client.clone(),
    )
    .await
    .unwrap();

    let operator_key = KeyPair::new_operator();
    let system_key = KeyPair::new_account();
    let enc_key = XKey::new();

    let engine = vm::configure_wasmtime().unwrap();
    let secrets_client = SecretsClient::new(client.clone(), enc_key);
    let config = Config {
        lattices: Some(LatticeGenerator {
            component: Component {
                source: ComponentSource::Nats(NatsSource {
                    bucket: OBJ_BUCKET.to_string(),
                    key: "generator.wasm".to_string(),
                }),
            },
            secrets_backend: "".to_string(),
            wasmcloud_system_account: system_key.public_key(),
            operator_public_key: operator_key.public_key(),
        }),
        authorization: None,
    };

    secrets_client
        .set(&operator_key.public_key(), &operator_key.seed().unwrap())
        .await
        .unwrap();

    secrets_client
        .set(&system_key.public_key(), &system_key.seed().unwrap())
        .await
        .unwrap();

    let url = format!(
        "nats://localhost:{}",
        container.get_host_port_ipv4(4222).await.unwrap()
    );

    let args = Args {
        lattice_bucket: LATTICE_BUCKET.to_string(),
        nats_url: url,
        work_stream: "WASMCLOUD_WORKQUEUE".to_string(),
        ..Default::default()
    };

    let api = LatticeWrpcApi::new(
        config,
        args,
        client.clone(),
        engine.clone(),
        secrets_client.clone(),
        Some(sys_client.clone()),
    )
    .await
    .unwrap();

    tokio::spawn(async move {
        api.run().await.unwrap();
    });

    let wrpc =
        wrpc_transport_nats::Client::new(client.clone(), LATTICE_SUBJECT, Some("lattices".into()));

    let backoff = Backoff::new();
    let timeout = std::time::Duration::from_secs(1);
    let start = std::time::Instant::now();
    loop {
        if std::time::Instant::now() - start > timeout {
            panic!("Timed out waiting for lattice server to start");
        }

        if lattice_service::health_check(&wrpc, None).await.is_err() {
            backoff.snooze();
        } else {
            break;
        }
    }

    let resp = match lattice_service::add_lattice(
        &wrpc,
        None,
        &LatticeAddRequest {
            lattice: Lattice {
                metadata: Metadata {
                    name: "test".to_string(),
                    version: "".to_string(),
                    uid: "".to_string(),
                    labels: Default::default(),
                    annotations: Default::default(),
                },
                account: None,
                description: None,
                signing_keys: None,
                deletable: false,
                status: LatticeStatus {
                    phase: LatticePhase::Provisioning,
                },
            },
        },
    )
    .await
    .unwrap()
    {
        Ok(r) => r,
        Err(e) => {
            println!("Error adding lattice: {}", e);
            panic!("Error adding lattice: {}", e);
        }
    };
    let resp = lattice_service::get_lattices(
        &wrpc,
        None,
        &LatticeGetRequest {
            lattices: vec!["test".to_string()],
        },
    )
    .await
    .unwrap();

    assert_eq!(resp.lattices.len(), 1);
    let l = resp.lattices[0].clone();
    // TODO figure out signing keys -- operator key shouldn't be in the jwt and a key without a
    // scope should be a string, not a JSON object
    assert_ne!(l.account, None);
    assert_ne!(l.account.as_ref().unwrap(), "");
    println!("Lattice: {:?}", &l);

    let lookup = sys_client
        .request(
            format!(
                "$SYS.REQ.ACCOUNT.{}.CLAIMS.LOOKUP",
                l.account.as_ref().unwrap()
            ),
            "".into(),
        )
        .await
        .unwrap();

    let msg = std::str::from_utf8(&lookup.payload).unwrap();
    println!("Account retrieved from nats server: {}", msg);
    // TODO come up with a test to verify the account is correct
    //assert_eq!(msg, l.account.unwrap());
    //
    //  Verify all the keys were written to the KV store
}
