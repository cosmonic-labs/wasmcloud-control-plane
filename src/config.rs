use clap::Parser;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser, Clone, Default)]
// TODO merge all this with the config file
pub struct Args {
    #[clap(short, long, default_value = "WASMCLOUD_LATTICES")]
    pub lattice_bucket: String,

    #[clap(short, long, default_value = "WASMCLOUD_WORKQUEUE")]
    pub work_stream: String,

    #[clap(short = 's', long, default_value = "127.0.0.1:4222")]
    pub nats_url: String,

    #[clap(short, long)]
    pub nats_credsfile: Option<String>,

    #[clap(long)]
    pub nats_sys_credsfile: Option<String>,

    #[clap(short, long, required = true)]
    pub config: String,

    #[clap(long)]
    pub encryption_key_file: Option<String>,
}

#[derive(Deserialize, Clone)]
pub struct Config {
    pub authorization: Option<Authorization>,
    pub lattices: Option<LatticeGenerator>,
}

#[derive(Deserialize, Clone)]
pub struct Authorization {
    pub user: AuthCallout,
    pub host: AuthCallout,
}

#[derive(Deserialize, Clone)]
pub struct Webhook {
    pub url: String,
    // TODO add TLS
}

#[derive(Deserialize, Clone)]
pub struct AuthCallout {
    pub account: String,
    pub signing_key_seed: SecretString,
    pub encryption_key_seed: SecretString,
    pub component: Option<Component>,
    pub webhook: Option<Webhook>,
    pub credsfile: String,
}

#[derive(Deserialize, Clone)]
pub enum ComponentSource {
    #[serde(rename = "nats")]
    Nats(NatsSource),
    #[serde(rename = "oci")]
    Oci(OciSource),
}

#[derive(Deserialize, Clone)]
pub struct OciSource {
    pub image: String,
    pub username: Option<String>,
    // TODO pull from secret backend
    pub password: Option<String>,
    pub insecure: bool,
}

#[derive(Deserialize, Clone)]
pub struct Component {
    pub source: ComponentSource,
}

#[derive(Deserialize, Clone)]
pub struct NatsSource {
    pub bucket: String,
    pub key: String,
}

#[derive(Deserialize, Clone)]
pub struct LatticeGenerator {
    pub component: Component,
    // TODO: need to add sys creds here, archiving
    pub secrets_backend: String,
    pub wasmcloud_system_account: String,
    pub operator_public_key: String,
}

#[derive(Deserialize, Clone)]
pub struct SecretBackend {
    pub name: String,
    pub subject: String,
    #[serde(flatten)]
    pub component: Component,
}

pub fn load_config(cfg: &str) -> anyhow::Result<Config> {
    let t: Config = toml::from_str(cfg)?;
    Ok(t)
}
