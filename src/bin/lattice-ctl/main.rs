use async_nats::{Client, ConnectOptions, Request};
use clap::{Parser, Subcommand};
use futures::{StreamExt, TryStreamExt};
use nkeys::{KeyPair, XKey};
use prost::Message;
use wasmcloud_api_types::{self as ProtoApi, Lattice as ProtoLattice};
use wasmcloud_hub::bindings::imports::wasmcloud::control::{lattice_service, lattice_types::*};
use wasmcloud_hub::lattice_proto::LATTICE_SUBJECT as PROTO_LATTICE_SUBJECT;
use wasmcloud_hub::lattice_wrpc::LATTICE_SUBJECT;
use wasmcloud_hub::secrets::SecretsClient;

#[derive(Parser, Clone)]
#[command(about)]
struct Args {
    #[clap(short, long, required = true)]
    credsfile: String,

    #[clap(short, long, required = true)]
    enc_key: String,

    #[clap(short, long, required = true)]
    key_dir: String,

    #[clap(short, long)]
    proto: bool,

    #[clap(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Clone)]
enum Cmd {
    Bootstrap,
    AddLattice {
        #[clap(short, long, required = true)]
        name: String,
    },
    GetLattice {
        #[clap(short, long, required = true)]
        name: String,
    },
    DeleteLattice {
        #[clap(short, long, required = true)]
        name: String,
    },
    WatchLattices {},
}

async fn bootstrap(key_dir: String, enc_key: XKey, client: Client) {
    let secrets_client = SecretsClient::new(client.clone(), enc_key);

    println!("{key_dir}");
    let system_key =
        std::fs::read_to_string(format!("{}/keys/WASMCLOUD SYSTEM.nk", key_dir)).unwrap();
    let operator_key =
        std::fs::read_to_string(format!("{}/keys/operator-signing.nk", key_dir)).unwrap();
    let system_key = KeyPair::from_seed(system_key.trim()).unwrap();
    let operator_key = KeyPair::from_seed(operator_key.trim()).unwrap();

    if secrets_client
        .get(&operator_key.public_key())
        .await
        .is_err()
    {
        secrets_client
            .set(&operator_key.public_key(), &operator_key.seed().unwrap())
            .await
            .unwrap();
    }

    if secrets_client.get(&system_key.public_key()).await.is_err() {
        secrets_client
            .set(&system_key.public_key(), &system_key.seed().unwrap())
            .await
            .unwrap();
    }
}

async fn get_lattice_proto(name: String, client: Client) {
    let req = ProtoApi::LatticeGetRequest {
        lattices: vec![name.clone()],
    };
    let headers = wasmcloud_hub::lattice_proto::CONTENT_TYPE_HEADERS.clone();
    let resp = client
        .request_with_headers(
            format!("{PROTO_LATTICE_SUBJECT}.get.{name}"),
            headers,
            req.encode_to_vec().into(),
        )
        .await
        .unwrap();
    let r = ProtoApi::LatticeGetResponse::decode(resp.payload).unwrap();
    println!("{:?}", r.lattices);
}

async fn get_lattice(name: String, wrpc: wrpc_transport_nats::Client) {
    let resp = match lattice_service::get_lattices(
        &wrpc,
        None,
        &LatticeGetRequest {
            lattices: vec![name],
        },
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            println!("Error getting lattice: {}", e);
            panic!("Error getting lattice: {}", e);
        }
    }
    .unwrap();

    let lattice = resp.lattices[0].clone();
    println!("{:?}", lattice);
}

async fn delete_lattice_proto(name: String, client: Client) {
    let req = ProtoApi::LatticeDeleteRequest::default();
    let headers = wasmcloud_hub::lattice_proto::CONTENT_TYPE_HEADERS.clone();
    let resp = client
        .request_with_headers(
            format!("{PROTO_LATTICE_SUBJECT}.delete.{name}"),
            headers,
            req.encode_to_vec().into(),
        )
        .await
        .unwrap();
    println!("{:?}", resp);
}

async fn delete_lattice(name: String, wrpc: wrpc_transport_nats::Client) {
    let resp =
        match lattice_service::delete_lattice(&wrpc, None, &LatticeDeleteRequest { lattice: name })
            .await
        {
            Ok(r) => r,
            Err(e) => {
                println!("Error deleting lattice: {}", e);
                panic!("Error deleting lattice: {}", e);
            }
        };

    println!("{:?}", resp);
}

async fn add_lattice(name: String, wrpc: wrpc_transport_nats::Client) {
    let resp = match lattice_service::add_lattice(
        &wrpc,
        None,
        &LatticeAddRequest {
            lattice: Lattice {
                metadata: Metadata {
                    name,
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
    println!("{:?}", resp);
}

async fn add_lattice_proto(name: String, client: Client) {
    let req = ProtoApi::LatticeAddRequest {
        lattice: Some(ProtoLattice::default()),
    };
    let headers = wasmcloud_hub::lattice_proto::CONTENT_TYPE_HEADERS.clone();
    let resp = client
        .request_with_headers(
            format!("{PROTO_LATTICE_SUBJECT}.add.{name}"),
            headers,
            req.encode_to_vec().into(),
        )
        .await
        .unwrap();
    let r = ProtoApi::LatticeAddResponse::decode(resp.payload).unwrap();
    println!("{:?}", r.version);
}

async fn watch_lattices_proto(client: Client) {
    let inbox = client.new_inbox();
    let req = ProtoApi::LatticeWatchRequest::default();
    let headers = wasmcloud_hub::lattice_proto::CONTENT_TYPE_HEADERS.clone();
    let c2 = client.clone();
    let i2 = inbox.clone();

    println!("{:?}", inbox);

    let jh = tokio::spawn(async move {
        let mut sub = c2.subscribe(i2.clone()).await.unwrap();
        while let Some(msg) = sub.next().await {
            let r = ProtoApi::LatticeWatchResponse::decode(msg.payload).unwrap();
            c2.publish(msg.reply.unwrap(), "".into()).await.unwrap();
            println!("{:?}", r);
        }
    });

    let req = Request::new()
        .payload(req.encode_to_vec().into())
        .headers(headers)
        .inbox(inbox.clone());
    client
        .send_request(format!("{PROTO_LATTICE_SUBJECT}.watch"), req)
        .await
        .unwrap();

    let _ = jh.await;
}

async fn watch_lattices(wrpc: wrpc_transport_nats::Client) {
    let (l, _other) =
        lattice_service::watch_lattices(&wrpc, None, &LatticeWatchRequest { lattices: None })
            .await
            .unwrap();
    let mut lattices = l.unwrap();
    while let Some(lattice) = lattices.next().await {
        println!("{:?}", lattice);
    }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let client = ConnectOptions::new()
        .credentials_file(args.credsfile)
        .await
        .unwrap()
        .connect("localhost:4222")
        .await
        .unwrap();
    let wrpc =
        wrpc_transport_nats::Client::new(client.clone(), LATTICE_SUBJECT, Some("lattices".into()));

    match args.cmd {
        Cmd::Bootstrap => {
            let enc = std::fs::read_to_string(args.enc_key).unwrap();
            let enc_key = XKey::from_seed(enc.trim()).unwrap();
            bootstrap(args.key_dir, enc_key, client).await
        }
        Cmd::AddLattice { name } => {
            if args.proto {
                add_lattice_proto(name, client).await;
            } else {
                add_lattice(name, wrpc).await;
            }
        }
        Cmd::GetLattice { name } => {
            if args.proto {
                get_lattice_proto(name, client).await;
            } else {
                get_lattice(name, wrpc).await;
            }
        }
        Cmd::DeleteLattice { name } => {
            if args.proto {
                delete_lattice_proto(name, client).await;
            } else {
                delete_lattice(name, wrpc).await;
            }
        }
        Cmd::WatchLattices {} => {
            if args.proto {
                watch_lattices_proto(client).await;
            } else {
                watch_lattices(wrpc).await;
            }
        }
    }
}
