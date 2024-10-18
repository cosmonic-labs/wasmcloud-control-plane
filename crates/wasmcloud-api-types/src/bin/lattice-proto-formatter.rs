use std::collections::HashMap;

use bytes::BytesMut;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use prost::Message;
use prost_types::FieldMask;
use wasmcloud_api_types::{
    Lattice, LatticeAddRequest, LatticeSpec, LatticeUpdate, LatticeUpdateRequest, Metadata,
};

#[derive(Subcommand)]
enum Subcommands {
    Add(AddCmd),
    Delete,
    Update(UpdateCmd),
}

#[derive(Parser)]
struct Cli {
    #[clap(short = 'f', long)]
    file_output: Option<String>,
    #[clap(subcommand)]
    command: Subcommands,
}

#[derive(Parser)]
struct AddCmd {
    #[clap(short, long)]
    name: String,

    #[clap(short, long, default_value = "")]
    description: String,

    #[clap(long)]
    deletable: bool,
}

#[derive(Parser, Clone)]
struct UpdateCmd {
    #[clap(short, long, required = true)]
    name: String,

    #[clap(short, long)]
    description: Option<String>,

    #[clap(long)]
    deletable: Option<bool>,

    //#[clap(short, long)]
    //signing_keys: Option<HashMap<String, String>>,
    #[clap(short, long)]
    account: Option<String>,
}

fn main() {
    let mut app = Cli::command();
    app.build();

    let matches = app.get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap();

    match cli.command {
        Subcommands::Add(add) => {
            let lattice = Lattice {
                meta: Some(Metadata {
                    name: add.name,
                    ..Default::default()
                }),
                spec: Some(LatticeSpec {
                    description: add.description,
                    deletable: add.deletable,
                    ..Default::default()
                }),
            };

            let req = LatticeAddRequest {
                lattice: Some(lattice),
            };

            if let Some(file) = cli.file_output {
                println!("Writing to file: {}", file);
                let mut buf = BytesMut::new();
                req.encode(&mut buf).unwrap();
                std::fs::write(file, &buf).unwrap();
            }
        }
        Subcommands::Delete => {
            println!("Delete command");
        }
        Subcommands::Update(update) => {
            let desc = update
                .description
                .as_ref()
                .unwrap_or(&"".to_string())
                .clone();

            //let signing_keys = update.signing_keys.clone().unwrap_or_default();
            let account = update.account.clone().unwrap_or_default();

            let lattice = LatticeUpdate {
                description: desc.clone(),
                deletable: update.deletable.unwrap_or_default(),
                account: account.clone(),
                //signing_keys: signing_keys.clone(),
                ..Default::default()
            };

            let mut paths: Vec<String> = Vec::new();
            if update.description.is_some() {
                paths.push("spec.description".to_string());
            }
            if update.deletable.is_some() {
                paths.push("spec.deletable".to_string());
            }
            if update.account.is_some() {
                paths.push("spec.account".to_string());
            }
            //if update.signing_keys.is_some() {
            //    paths.push("spec.signing_keys".to_string());
            //}

            let lattice_update = FieldMask { paths };

            let req = LatticeUpdateRequest {
                update: Some(lattice),
                update_mask: Some(lattice_update),
            };

            if let Some(file) = cli.file_output {
                println!("Writing to file: {}", file);
                let mut buf = BytesMut::new();
                req.encode(&mut buf).unwrap();
                std::fs::write(file, &buf).unwrap();
            }
        }
    }
}
