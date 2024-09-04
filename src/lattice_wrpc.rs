use crate::bindings::lattice_generator::exports::wasmcloud::control::lattice_lifecycle::{
    Context as LatticeContext, CreateLatticeResponse,
};
use crate::bindings::lattice_generator::LatticeGeneratorPre;
use crate::bindings::lattice_server::exports::wasmcloud::control::lattice_service::{
    self, Health, LatticeAddRequest, LatticeAddResponse, LatticeDeleteRequest,
    LatticeDeleteResponse, LatticeGetRequest, LatticeGetResponse, LatticeUpdateRequest,
    LatticeUpdateResponse, LatticeWatchRequest,
};
use crate::bindings::lattice_server::wasmcloud::control::lattice_types::{Lattice, LatticePhase};
use crate::config::{Args, ComponentSource, Config};
use crate::secrets::SecretsClient;
use crate::vm::Ctx;
use crate::{bindings, vm};
use anyhow::{bail, Context};
use async_nats::{
    jetstream::{
        self,
        kv::{Config as KvConfig, Watch},
    },
    Client, HeaderMap, Subject,
};
use bytes::{BufMut, Bytes, BytesMut};
use futures::{stream::select_all, SinkExt, Stream};
use futures::{StreamExt, TryStreamExt};
use oci_distribution::{secrets::RegistryAuth, Reference};
use std::convert::From;
use std::pin::Pin;
use std::str::FromStr;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, error, info, warn};
use ulid::Ulid;
use uuid::Uuid;
use wadm_client::{Client as WadmClient, ClientConnectOptions};
use wasmtime::{
    component::{Component, Linker, ResourceTable},
    Engine, Store as WasmStore,
};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiView};

// TODO come up with a better MIME type for this
//const WRPC_MIME: &str = "application/vnd.bytecodealliance.wit.wrpc";

pub const LATTICE_SUBJECT: &str = "wasmcloud.lattice.v1alpha1";
pub const LATTICE_BUCKET: &str = "WASMCLOUD_LATTICES";
pub const OBJ_BUCKET: &str = "WASMCLOUD_OBJECTS";

#[derive(Clone)]
pub struct LatticeWrpcApi {
    client: Client,
    nats_sys_client: Option<Client>,
    nats_url: String,
    engine: Engine,
    component: Option<LatticeGeneratorPre<Ctx>>,
    secrets_client: SecretsClient,
    operator_key: Option<String>,
    system_account: Option<String>,
}

impl LatticeWrpcApi {
    pub async fn new(
        config: Config,
        args: Args,
        client: Client,
        engine: Engine,
        secrets_client: SecretsClient,
        sys_client: Option<Client>,
    ) -> anyhow::Result<Self> {
        let mut server = LatticeWrpcApi {
            client,
            nats_sys_client: sys_client,
            component: None,
            nats_url: args.nats_url.clone(),
            engine,
            secrets_client,
            operator_key: None,
            system_account: None,
        };

        //if let Some(_lattice) = &config.lattices {
        //    if let Some(credsfile) = args.nats_sys_credsfile {
        //        let sys_client = ConnectOptions::new()
        //            .credentials_file(credsfile)
        //            .await?
        //            .connect(args.nats_url)
        //            .await?;
        //        server.nats_sys_client = Some(sys_client);
        //    } else {
        //        bail!("No system credentials file provided for lattice");
        //    }
        //};

        // TODO this is all duplicate code. Unduplicate it.
        if let Some(lattice) = &config.lattices {
            let comp = &lattice.component;
            let c = Self::load_component(comp.source.clone(), server.client.clone()).await?;
            let compiled = Component::from_binary(&server.engine, &c)?;
            let mut linker: Linker<Ctx> = Linker::new(&server.engine);
            wasmtime_wasi::add_to_linker_async(&mut linker)
                .context("failed to link to core WASI")?;

            vm::wasmtime_bindings::wasmcloud::secrets::store::add_to_linker(&mut linker, |ctx| ctx)
                .context("failed to link to secrets")?;
            vm::wasmtime_bindings::wasmcloud::secrets::reveal::add_to_linker(&mut linker, |ctx| {
                ctx
            })
            .context("failed to link to secrets")?;
            vm::wasmtime_bindings::wasmcloud::control::lattice_account_update::add_to_linker(
                &mut linker,
                |ctx| ctx,
            )
            .context("failed to link to lattice update interface")?;
            let linked = linker.instantiate_pre(&compiled)?;
            let bound = LatticeGeneratorPre::<Ctx>::new(linked)?;
            server.component = Some(bound);

            server.operator_key = Some(lattice.operator_public_key.clone());
            server.system_account = Some(lattice.wasmcloud_system_account.clone());
        };

        Ok(server)
    }

    async fn load_component(component: ComponentSource, client: Client) -> anyhow::Result<Bytes> {
        let js = jetstream::new(client);
        match &component {
            ComponentSource::Nats(n) => {
                info!(store = n.bucket, component = n.key, "Loading component");
                let store = js.get_object_store(&n.bucket).await?;
                let mut obj = store.get(&n.key).await.unwrap();
                let mut buf = BytesMut::new();
                loop {
                    if let Ok(n) = obj.read_buf(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                    }
                }
                Ok(buf.freeze())
            }
            ComponentSource::Oci(s) => {
                let mut cfg = oci_distribution::client::ClientConfig::default();
                if s.insecure {
                    cfg.protocol = oci_distribution::client::ClientProtocol::Http;
                    cfg.accept_invalid_certificates = true;
                }
                let client = oci_distribution::Client::new(cfg);
                let auth = match (&s.username, &s.password) {
                    (Some(u), Some(p)) => {
                        oci_distribution::secrets::RegistryAuth::Basic(u.clone(), p.clone())
                    }
                    _ => RegistryAuth::Anonymous,
                };

                let reference = Reference::from_str(&s.image).unwrap();
                let img = client
                    .pull(
                        &reference,
                        &auth,
                        vec![crate::auth::WASM_MEDIA_TYPE, crate::auth::OCI_MEDIA_TYPE],
                    )
                    .await
                    .unwrap();
                Ok(img
                    .layers
                    .iter()
                    .flat_map(|l| l.data.clone())
                    .collect::<Bytes>())
            }
        }
    }

    async fn invoke_component_create(
        &self,
        lattice: Lattice,
    ) -> anyhow::Result<CreateLatticeResponse> {
        if self.component.is_none() {
            return Ok(CreateLatticeResponse {
                lattice: lattice.into(),
                account: None,
            });
        }
        let cmp = self.component.as_ref().unwrap();
        let duration = Duration::from_secs(1);
        let wasi = WasiCtxBuilder::new()
            .args(&["lattice.wasm"])
            .inherit_stderr()
            .build();
        let mut store = WasmStore::new(
            &self.engine,
            Ctx::new(
                wasi,
                duration,
                self.secrets_client.clone(),
                self.nats_sys_client.clone(),
            ),
        );

        let create = LatticeContext {
            lattice: lattice.into(),
            additional_metadata: None,
            operator_key: self.operator_key.clone(),
            system_account: self.system_account.clone(),
        };

        store.set_epoch_deadline(duration.as_secs());
        let l = cmp.instantiate_async(&mut store).await?;
        let res = l
            .wasmcloud_control_lattice_lifecycle()
            .call_create(&mut store, &create)
            .await?;

        match res {
            Err(e) => {
                error!("Failed to create lattice: {}", e);
                bail!("Failed to create lattice: {}", e)
            }
            Ok(result) => {
                info!("Lattice created: {}", result.lattice.metadata.name);
                Ok(result)
            }
        }
    }

    async fn watch_all_lattices(&self) -> anyhow::Result<Vec<Watch>> {
        let js = jetstream::new(self.client.clone());
        let bucket = js.get_key_value(LATTICE_BUCKET).await?;
        // This is because the async nats client defaults to only relaying updates and does not
        // show the latest update for every key, which is the behavior we're after here.
        // Using the ">" means we get the latest update for every key since there isn't currently a
        // better way to do that.
        let watch = bucket.watch_with_history(">").await?;
        Ok(vec![watch])
    }

    async fn watch_lattices(&self, lattices: Vec<String>) -> anyhow::Result<Vec<Watch>> {
        let js = jetstream::new(self.client.clone());
        let bucket = js.get_key_value(LATTICE_BUCKET).await?;
        let mut watches = Vec::new();
        for lattice in lattices {
            let watch = bucket.watch_with_history(&lattice).await?;
            watches.push(watch);
        }
        Ok(watches)
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let wrpc_client = wrpc_transport_nats::Client::new(
            self.client.clone(),
            LATTICE_SUBJECT,
            Some("lattices".into()),
        );

        // lol fix this clone
        let mut invocations = crate::bindings::lattice_server::serve(&wrpc_client, self.clone())
            .await
            .unwrap();

        // TODO: is there a way to make this unbounded or make the bound based on something sane?
        let mut invocations = select_all(invocations.into_iter().map(
            |(instance, name, invocations)| {
                invocations
                    .try_buffer_unordered(16) // handle up to 16 invocations concurrently
                    .map(move |res| (instance, name, res))
            },
        ));

        while let Some((instance, name, res)) = invocations.next().await {
            match res {
                Ok(()) => {
                    info!(instance, name, "invocation successfully handled");
                }
                Err(err) => {
                    warn!(?err, instance, name, "failed to accept invocation");
                }
            }
        }

        Ok(())
    }

    fn lattice_from_subject(&self, subject: Subject) -> anyhow::Result<String> {
        let split: Vec<&str> = subject.split('.').collect();
        if split.len() < 4 {
            bail!("Invalid subject")
        }
        Ok(split[3].to_string())
    }
}

impl lattice_service::Handler<Option<HeaderMap>> for LatticeWrpcApi {
    async fn health_check(&self, _cx: Option<async_nats::HeaderMap>) -> anyhow::Result<Health> {
        Ok(Health {
            ready: true,
            message: Some("OK".to_string()),
        })
    }

    async fn add_lattice(
        &self,
        _cx: Option<async_nats::HeaderMap>,
        req: LatticeAddRequest,
    ) -> anyhow::Result<Result<LatticeAddResponse, String>> {
        let mut lattice = req.lattice;

        if lattice.metadata.name.is_empty() {
            bail!("Lattice name is required")
        }
        let name = lattice.metadata.name.clone();

        let version = Ulid::new().to_string();
        if lattice.metadata.version.is_empty() {
            lattice.metadata.version = version.clone();
        }
        let version = lattice.metadata.version.clone();
        lattice.metadata.uid = Uuid::new_v4().to_string();
        lattice.status.phase = LatticePhase::Provisioning;

        // TODO create LATTICEDATA and CONFIGDATA buckets
        // Make the replicas and whatnot configurable
        // Should probably be some sort of reconciliation loop to create/update these buckets.
        let js = jetstream::new(self.client.clone());
        js.create_key_value(KvConfig {
            bucket: format!("{}_{}", "LATTICEDATA", name),
            num_replicas: 1,
            ..Default::default()
        })
        .await?;
        js.create_key_value(KvConfig {
            bucket: format!("{}_{}", "CONFIGDATA", name),
            num_replicas: 1,
            ..Default::default()
        })
        .await?;

        let bucket = js.get_key_value(LATTICE_BUCKET).await?;
        let mut buf = BytesMut::new();
        wrpc_pack::pack(&lattice, &mut buf)?;
        let revision = bucket.put(name.clone(), buf.freeze()).await?;

        // TODO this should really be invoked by a task queue for robustness.
        // TODO this should be calling common code instead of replicating the logic previously
        // written for auth.
        if self.component.is_some() {
            let l = match self.invoke_component_create(lattice.clone()).await {
                Ok(l) => l,
                Err(e) => {
                    error!("Error invoking component: {}", e);
                    return Ok(Err(format!("Error invoking component: {}", e)));
                }
            };

            lattice = l.lattice.into();
            if let Some(account) = l.account {
                println!("{}", account.encoded);
            }
        }

        // TODO this should actually be updated after we run the lattice provisioning component if
        // specified.
        lattice.status.phase = LatticePhase::Created;
        let mut buf = BytesMut::new();
        wrpc_pack::pack(&lattice, &mut buf)?;
        bucket.update(name.clone(), buf.freeze(), revision).await?;

        let response = LatticeAddResponse { version };
        Ok(Ok(response))
    }

    async fn update_lattice(
        &self,
        _cx: Option<async_nats::HeaderMap>,
        req: LatticeUpdateRequest,
    ) -> anyhow::Result<LatticeUpdateResponse> {
        if req.update_mask.paths.is_empty() {
            bail!("No paths provided for update")
        }

        let js = jetstream::new(self.client.clone());
        let bucket = js.get_key_value(LATTICE_BUCKET).await?;
        let mut lattice: Lattice = match bucket.get(&req.update.lattice).await {
            Ok(l) => {
                if l.is_none() {
                    bail!("Lattice not found")
                }
                let mut buf = BytesMut::from(l.unwrap());
                wrpc_pack::unpack(&mut buf)?
            }
            Err(e) => bail!("Failed to get lattice"),
        };

        let mut modified = false;
        for field in req.update_mask.paths {
            match field.as_str() {
                "account" => match &req.update.account {
                    Some(account) => {
                        lattice.account = Some(account.clone());
                        modified = true;
                    }
                    None => {
                        lattice.account = None;
                        modified = true;
                    }
                },
                "description" => match &req.update.description {
                    Some(description) => {
                        lattice.description = Some(description.clone());
                        modified = true;
                    }
                    None => {
                        lattice.description = None;
                        modified = true;
                    }
                },
                "signing_keys" => match &req.update.signing_keys {
                    Some(signing_keys) => {
                        lattice.signing_keys = Some(signing_keys.clone());
                        modified = true;
                    }
                    None => {
                        lattice.signing_keys = None;
                        modified = true;
                    }
                },
                "deletable" => match &req.update.deletable {
                    Some(deletable) => {
                        lattice.deletable = *deletable;
                        modified = true;
                    }
                    None => {
                        lattice.deletable = false;
                        modified = true;
                    }
                },
                _ => {
                    error!("Invalid field for update");
                }
            }
        }
        if modified {
            lattice.metadata.version = Ulid::new().to_string();
            let mut buf = BytesMut::new();
            wrpc_pack::pack(&lattice, &mut buf)?;
            // TODO use update not put here, which means getting the latest revision
            bucket
                .put(lattice.metadata.name.clone(), buf.freeze())
                .await?;
        }
        Ok(LatticeUpdateResponse {
            version: lattice.metadata.version,
        })
    }

    async fn list_lattices(
        &self,
        _cx: Option<async_nats::HeaderMap>,
    ) -> anyhow::Result<Vec<String>> {
        let js = jetstream::new(self.client.clone());
        let bucket = js.get_key_value(LATTICE_BUCKET).await?;
        let mut keys = bucket.keys().await?;
        let mut lattices: Vec<String> = Vec::new();
        while let Some(key) = keys.next().await {
            match key {
                Ok(k) => {
                    lattices.push(k);
                }
                Err(e) => {
                    error!("Error getting key");
                    return Err(anyhow::anyhow!("Error getting key: {e}"));
                }
            }
        }
        Ok(lattices)
    }

    async fn get_lattices(
        &self,
        _cx: Option<async_nats::HeaderMap>,
        req: LatticeGetRequest,
    ) -> anyhow::Result<LatticeGetResponse> {
        let js = jetstream::new(self.client.clone());
        let bucket = js.get_key_value(LATTICE_BUCKET).await?;
        let mut lattices: Vec<Lattice> = Vec::new();
        for lattice in req.lattices {
            let l = match bucket.get(&lattice).await {
                Ok(l) => {
                    if l.is_none() {
                        bail!("Lattice not found")
                    }
                    let mut buf = BytesMut::from(l.unwrap());
                    wrpc_pack::unpack(&mut buf)?
                }
                Err(e) => bail!("Failed to get lattice {}, {e}", lattice),
            };
            lattices.push(l);
        }
        Ok(LatticeGetResponse { lattices: lattices })
    }

    async fn delete_lattice(
        &self,
        _cx: Option<async_nats::HeaderMap>,
        req: LatticeDeleteRequest,
    ) -> anyhow::Result<LatticeDeleteResponse> {
        let js = jetstream::new(self.client.clone());
        let bucket = js.get_key_value(LATTICE_BUCKET).await?;
        let mut lattice: Lattice = match bucket.get(&req.lattice).await {
            Ok(l) => {
                if l.is_none() {
                    bail!("Lattice not found")
                }
                let mut buf = BytesMut::from(l.unwrap());
                wrpc_pack::unpack(&mut buf)?
            }
            Err(e) => bail!("Failed to get lattice"),
        };

        // check for running applications
        // TODO once we figure out how to call the `cleanup` function, this should actually just be
        // a check to see if there are any workloads running.
        //let mut opts = ClientConnectOptions {
        //    url: Some(self.nats_url.clone()),
        //    ..Default::default()
        //};
        //let wadm =
        //    WadmClient::from_nats_client(req.lattice.as_str(), None, self.client.clone()).await?;

        //let applications = wadm.list_manifests().await?;
        //if !applications.is_empty() {
        //    bail!("Lattice has applications. Delete them first before deleting the lattice")
        //}
        let msg = self
            .client
            .request(format!("wadm.api.{}.model.list", req.lattice), "".into())
            .await?;
        if msg.payload != "[]" {
            bail!("Lattice has applications. Delete them first before deleting the lattice")
        }

        lattice.status.phase = LatticePhase::Deleting;
        let mut buf = BytesMut::new();
        wrpc_pack::pack(&lattice, &mut buf)?;
        bucket
            .put(lattice.metadata.name.clone(), buf.freeze())
            .await?;
        bucket.delete(lattice.metadata.name.clone()).await?;
        Ok(LatticeDeleteResponse { lattice })
    }

    async fn watch_lattices(
        &self,
        _cx: Option<async_nats::HeaderMap>,
        req: LatticeWatchRequest,
    ) -> anyhow::Result<Result<Pin<Box<dyn Stream<Item = Vec<Lattice>> + Send>>, String>> {
        let watch = match req.lattices {
            None => self.watch_all_lattices().await?,
            Some(l) => self.watch_lattices(l).await?,
        };

        let mut streams = select_all(watch.into_iter().map(|w| w.into_stream()));
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        tokio::spawn(async move {
            while let Some(item) = streams.next().await {
                if let Ok(item) = item {
                    let mut buf = BytesMut::from(item.value);
                    if let Ok(l) = wrpc_pack::unpack::<Lattice>(&mut buf) {
                        if tx.send(vec![l]).await.is_err() {
                            debug!("stream receiver closed");
                            return;
                        }
                    }
                } else {
                    continue;
                };
            }
        });
        Ok(Ok(Box::pin(ReceiverStream::new(rx))))
    }
}

#[cfg(test)]
mod test {
    use super::{LatticeWrpcApi, LATTICE_BUCKET, LATTICE_SUBJECT};
    use crate::bindings::imports::wasmcloud::control::{lattice_service, lattice_types::*};
    use crate::{secrets::SecretsClient, vm};
    use async_nats::{jetstream, Client};
    use bytes::BytesMut;
    use crossbeam_utils::Backoff;
    use nkeys::XKey;
    use testcontainers::{
        core::{ImageExt, IntoContainerPort, WaitFor},
        runners::AsyncRunner,
        GenericImage,
    };
    use tokio_stream::StreamExt;

    #[tokio::test]
    async fn create_lattice() {
        let container = GenericImage::new("nats", "2.10")
            .with_exposed_port(4222.tcp())
            .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
            .with_cmd(vec!["-js"])
            .start()
            .await
            .expect("Started NATS");

        let url = format!(
            "nats://localhost:{}",
            container.get_host_port_ipv4(4222).await.unwrap()
        );
        let client = async_nats::connect(&url)
            .await
            .expect("Failed to connect to NATS");
        let secrets_client = new_secrets_client(client.clone());
        let engine = vm::configure_wasmtime().unwrap();

        let api = LatticeWrpcApi {
            client: client.clone(),
            nats_sys_client: None,
            component: None,
            nats_url: url,
            secrets_client,
            engine,
            operator_key: None,
            system_account: None,
        };

        tokio::spawn(async move {
            if let Err(e) = api.run().await {
                println!("Error running lattice server: {}", e);
            }
        });

        let wrpc = wrpc_transport_nats::Client::new(
            client.clone(),
            LATTICE_SUBJECT,
            Some("lattices".into()),
        );

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

        let js = jetstream::new(client.clone());
        js.create_key_value(jetstream::kv::Config {
            bucket: LATTICE_BUCKET.to_string(),
            num_replicas: 1,
            ..Default::default()
        })
        .await
        .unwrap();

        let resp = lattice_service::add_lattice(
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
        .unwrap();
        assert_ne!(resp.version, "");

        js.get_key_value("LATTICEDATA_test").await.unwrap();
        js.get_key_value("CONFIGDATA_test").await.unwrap();
        let store = js.get_key_value(LATTICE_BUCKET).await.unwrap();
        let value = store.get("test").await.unwrap();
        assert!(value.is_some());
        let v = value.unwrap();
        let mut v2 = BytesMut::from(v);
        let l: Lattice = wrpc_pack::unpack(&mut v2).unwrap();
        assert_eq!(l.metadata.name, "test");
        assert_eq!(l.metadata.version, resp.version);
        assert_eq!(l.status.phase, LatticePhase::Created);
        assert_ne!(l.metadata.uid, "");

        // HACK: we should have a better way to mock this, though we might be able to do something
        // internally when we embed wadm.
        let delete_client = client.clone();
        tokio::spawn(async move {
            let mut sub = delete_client
                .subscribe("wadm.api.test.model.list")
                .await
                .unwrap();
            while let Some(req) = sub.next().await {
                let msg = req;
                let reply = msg.reply.unwrap();
                let _ = delete_client.publish(reply, "[]".into()).await;
            }
        });

        let _resp = lattice_service::delete_lattice(
            &wrpc,
            None,
            &LatticeDeleteRequest {
                lattice: "test".to_string(),
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn update_lattice_test() {
        let container = GenericImage::new("nats", "2.10")
            .with_exposed_port(4222.tcp())
            .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
            .with_cmd(vec!["-js"])
            .start()
            .await
            .expect("Started NATS");

        let client = async_nats::connect(&format!(
            "nats://localhost:{}",
            container.get_host_port_ipv4(4222).await.unwrap()
        ))
        .await
        .expect("Connected to NATS");

        let secrets_client = new_secrets_client(client.clone());
        let engine = vm::configure_wasmtime().unwrap();
        let api = LatticeWrpcApi {
            client: client.clone(),
            nats_sys_client: None,
            component: None,
            nats_url: "".to_string(),
            engine,
            secrets_client,
            operator_key: None,
            system_account: None,
        };

        tokio::spawn(async move {
            api.run().await.unwrap();
        });

        // TODO the server should have a health check we could ping here to make sure it's actually
        // ready.
        //tokio::time::sleep(std::time::Duration::from_millis(250)).await;

        let js = jetstream::new(client.clone());
        js.create_key_value(jetstream::kv::Config {
            bucket: LATTICE_BUCKET.to_string(),
            num_replicas: 1,
            ..Default::default()
        })
        .await
        .unwrap();

        let wrpc = wrpc_transport_nats::Client::new(
            client.clone(),
            LATTICE_SUBJECT,
            Some("lattices".into()),
        );

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
                panic!("Error adding lattice");
            }
        };

        let update = LatticeUpdateRequest {
            update: LatticeUpdate {
                lattice: "test".to_string(),
                account: Some("ABC123".to_string()),
                description: Some("Test".to_string()),
                signing_keys: None,
                deletable: None,
            },
            update_mask: FieldMask {
                paths: vec!["account".to_string(), "description".to_string()],
            },
        };

        let result = lattice_service::update_lattice(&wrpc, None, &update)
            .await
            .unwrap();
        assert_ne!(result.version, "");
        assert_ne!(result.version, resp.version);

        // TODO read it back
        let get = lattice_service::get_lattices(
            &wrpc,
            None,
            &LatticeGetRequest {
                lattices: vec!["test".to_string()],
            },
        )
        .await
        .unwrap();
        assert_eq!(get.lattices.len(), 1);
        // TODO we can probably derive PartialEq for this
        assert_eq!(get.lattices[0].account.as_ref().unwrap(), "ABC123");
        assert_eq!(get.lattices[0].description.as_ref().unwrap(), "Test");
        assert_eq!(get.lattices[0].signing_keys, None);
        assert!(!get.lattices[0].deletable);
    }

    #[tokio::test]
    async fn watch_test() {
        let container = GenericImage::new("nats", "2.10")
            .with_exposed_port(4222.tcp())
            .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
            .with_cmd(vec!["-js"])
            .start()
            .await
            .expect("Started NATS");

        let client = async_nats::connect(&format!(
            "nats://localhost:{}",
            container.get_host_port_ipv4(4222).await.unwrap()
        ))
        .await
        .expect("Connected to NATS");

        let secrets_client = new_secrets_client(client.clone());
        let engine = vm::configure_wasmtime().unwrap();
        let api = LatticeWrpcApi {
            client: client.clone(),
            nats_sys_client: None,
            component: None,
            nats_url: "".to_string(),
            engine,
            secrets_client,
            operator_key: None,
            system_account: None,
        };

        tokio::spawn(async move {
            api.run().await.unwrap();
        });

        // TODO the server should have a health check we could ping here to make sure it's actually
        // ready.
        //tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let js = jetstream::new(client.clone());
        js.create_key_value(jetstream::kv::Config {
            bucket: LATTICE_BUCKET.to_string(),
            num_replicas: 1,
            ..Default::default()
        })
        .await
        .unwrap();

        let wrpc = wrpc_transport_nats::Client::new(
            client.clone(),
            LATTICE_SUBJECT,
            Some("lattices".into()),
        );

        let resp = lattice_service::add_lattice(
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
        .unwrap();

        //let req = LatticeWatchRequest { lattices: None };
        //let watch = watch_lattices(&wrpc, None, &req).await.unwrap();
        //while let (_, _, _, _) = watch.next().await {
        //    //assert_eq!(lattices.len(), 1);
        //    //assert_eq!(lattices[0].metadata.name, "test");
        //}
    }

    fn new_secrets_client(client: Client) -> SecretsClient {
        let enc_key = XKey::new();
        SecretsClient::new(client, enc_key)
    }
}
