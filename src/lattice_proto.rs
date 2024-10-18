use crate::bindings::lattice_generator::wasmcloud::control::lattice_types::Lattice as ComponentLattice;
use crate::bindings::lattice_generator::LatticeGeneratorPre;
use crate::config::{Args, ComponentSource, Config};
use crate::secrets::SecretsClient;
use crate::vm::Ctx;
use crate::{bindings, vm};
use anyhow::{anyhow, bail, Context};
use async_nats::{
    jetstream::{
        self,
        kv::{Config as KvConfig, Watch},
    },
    Client, HeaderMap, HeaderName, HeaderValue, Subject,
};
use async_nats::{Message, RequestErrorKind};
use bytes::{BufMut, Bytes, BytesMut};
use futures::{stream::select_all, SinkExt, Stream};
use futures::{StreamExt, TryStreamExt};
use oci_distribution::{secrets::RegistryAuth, Reference};
use prost::Message as ProtobufMessage;
use std::cell::LazyCell;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tracing::{debug, error, info, warn};
use ulid::Ulid;
use uuid::Uuid;
use wasmcloud_api_types::*;
use wasmtime::{
    component::{Component, Linker, ResourceTable},
    Engine, Store as WasmStore,
};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiView};

use crate::bindings::lattice_generator::exports::wasmcloud::control::lattice_lifecycle::Context as LatticeContext;

pub const MIME_TYPE: &str = "application/x-protobuf";
pub const LATTICE_SUBJECT: &str = "wasmcloud.lattice.v1alpha1_proto";
pub const LATTICE_BUCKET: &str = "WASMCLOUD_LATTICES_proto";
pub const OBJ_BUCKET: &str = "WASMCLOUD_OBJECTS";
pub const CONTENT_TYPE_HEADERS: LazyCell<HeaderMap> = LazyCell::new(|| {
    let mut headers = HeaderMap::new();
    headers.insert("Content-Type", MIME_TYPE);
    headers
});

pub struct LatticeProtoApi {
    client: Client,
    nats_sys_client: Option<Client>,
    nats_url: String,
    engine: Engine,
    component: Option<LatticeGeneratorPre<Ctx>>,
    secrets_client: SecretsClient,
    operator_key: Option<String>,
    system_account: Option<String>,
}

impl LatticeProtoApi {
    pub async fn new(
        config: Config,
        args: Args,
        client: Client,
        engine: Engine,
        secrets_client: SecretsClient,
        sys_client: Option<Client>,
    ) -> anyhow::Result<Self> {
        let mut server = LatticeProtoApi {
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
            info!("loading lattice component");
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

    fn lattice_from_subject(&self, subject: &Subject) -> Option<String> {
        let split: Vec<&str> = subject.split('.').collect();
        if split.len() < 5 {
            return None;
        }
        Some(split[4].to_string())
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let mut sub = self
            .client
            .queue_subscribe(format!("{LATTICE_SUBJECT}.>"), "lattice_proto".into())
            .await?;
        let handler = Arc::new(self);
        info!("Lattice proto server started");

        while let Some(msg) = sub.next().await {
            match handler.handle_msg(msg).await {
                Ok(_) => {}
                Err(e) => {
                    error!("Error handling message: {:?}", e);
                }
            };
        }

        Ok(())
    }

    async fn handle_msg(self: &Arc<Self>, msg: Message) -> anyhow::Result<()> {
        let subject = msg.subject.clone();
        let lattice = self.lattice_from_subject(&subject);
        let headers = match &msg.headers {
            Some(h) => h,
            None => {
                return Err(anyhow::anyhow!(
                    "No headers -- content-type header is missing"
                ))
            }
        };
        if let Some(content_type) = headers.get("Content-Type") {
            if content_type.as_str() != MIME_TYPE {
                return Err(anyhow::anyhow!("Invalid content type: {}", content_type));
            }
        } else {
            return Err(anyhow::anyhow!("No content type header"));
        }
        let operation = subject.split('.').collect::<Vec<_>>()[3].to_string();
        if operation != "watch" && lattice.is_none() {
            self.client
                .publish(msg.reply.unwrap(), "No lattice specified".into())
                .await?;
            return Ok(());
        }

        let handle = Arc::clone(self);
        tokio::spawn(async move {
            info!("Handling operation: {}", operation);

            let resp = match operation.as_str() {
                "add" => handle.add_lattice(&lattice.unwrap(), &msg).await,
                "update" => handle.update_lattice(&lattice.unwrap(), &msg).await,
                "delete" => handle.delete_lattice(&lattice.unwrap(), &msg).await,
                "get" => handle.get_lattices(&msg).await,
                "watch" => handle.watch_lattices(&msg).await,
                _ => Err(anyhow::anyhow!("Invalid operation: {}", operation)),
            };

            match resp {
                Ok(resp) => {
                    if let Some(r) = resp {
                        let mut headers = HeaderMap::new();
                        headers.insert("Content-Type", MIME_TYPE);
                        if let Err(e) = handle
                            .client
                            .publish_with_headers(msg.reply.unwrap(), headers, r)
                            .await
                        {
                            error!("Error sending response: {:?}", e);
                        }
                    }
                }
                Err(e) => {
                    error!("Error handling request: {:?}", e);
                    if let Err(e) = handle
                        .client
                        .publish(msg.reply.unwrap(), e.to_string().into())
                        .await
                    {
                        error!("Error sending error response: {:?}", e);
                    }
                }
            };
        });

        Ok(())
    }

    async fn add_lattice(&self, name: &str, msg: &Message) -> anyhow::Result<Option<Bytes>> {
        info!("Adding lattice: {}", name);
        let req = LatticeAddRequest::decode(msg.payload.clone())?;
        let version = Ulid::new().to_string();
        let mut lattice = match req.lattice {
            Some(l) => l,
            None => return Err(anyhow::anyhow!("No lattice in request")),
        };
        lattice.meta = Some(Metadata {
            name: name.to_string(),
            version: version.clone(),
            uid: Uuid::new_v4().to_string(),
            ..Default::default()
        });
        if lattice.spec.is_none() {
            lattice.spec = Some(LatticeSpec {
                description: "".to_string(),
                deletable: true,
                ..Default::default()
            });
        }
        lattice.spec.as_mut().unwrap().status = Some(LatticeStatus {
            phase: LatticePhase::LatticeProvisioning.into(),
        });

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
        lattice.encode(&mut buf)?;
        let revision = bucket.put(name, buf.freeze()).await?;

        // TODO this should really be invoked by a task queue for robustness.
        // TODO this should be calling common code instead of replicating the logic previously
        // written for auth.
        if self.component.is_some() {
            let l = match self.invoke_component_create(lattice.clone()).await {
                Ok(l) => l,
                Err(e) => {
                    error!("Error invoking component: {}", e);
                    bail!("Error invoking component: {}", e);
                }
            };

            lattice = l;
        }
        lattice
            .spec
            .as_mut()
            .unwrap()
            .status
            .as_mut()
            .unwrap()
            .phase = LatticePhase::LatticeCreated.into();
        let bucket = js.get_key_value(LATTICE_BUCKET).await?;
        let mut buf = BytesMut::new();
        lattice.meta.as_mut().unwrap().version = Ulid::new().to_string();
        lattice.encode(&mut buf)?;
        bucket.update(name, buf.freeze(), revision).await?;
        let resp = LatticeAddResponse { version };
        let buf = resp.encode_to_vec();
        Ok(Some(buf.into()))
    }

    async fn invoke_component_create(&self, lattice: Lattice) -> anyhow::Result<Lattice> {
        if self.component.is_none() {
            return Ok(lattice);
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
            lattice: ComponentLattice::from(lattice),
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
                let r = result.lattice.clone();
                Ok(r.into())
            }
        }
    }

    async fn update_lattice(&self, name: &str, msg: &Message) -> anyhow::Result<Option<Bytes>> {
        let req = LatticeUpdateRequest::decode(msg.payload.clone())?;
        let mask = match req.update_mask {
            Some(m) => m,
            None => return Err(anyhow::anyhow!("No update mask in request")),
        };
        let update = match req.update {
            Some(l) => l,
            None => return Err(anyhow::anyhow!("No update in request")),
        };

        let js = jetstream::new(self.client.clone());
        let bucket = js.get_key_value(LATTICE_BUCKET).await?;
        let mut lattice = match bucket.get(name).await {
            Ok(l) => {
                if l.is_none() {
                    bail!("Lattice not found");
                }
                Lattice::decode(l.unwrap())?
            }
            Err(e) => {
                error!("Error getting lattice: {:?}", e);
                bail!("Error getting lattice: {:?}", e);
            }
        };
        if mask.paths.contains(&"description".to_string()) {
            lattice.spec.as_mut().unwrap().description = update.description;
        }
        if mask.paths.contains(&"deletable".to_string()) {
            lattice.spec.as_mut().unwrap().deletable = update.deletable;
        }
        if mask.paths.contains(&"account".to_string()) {
            lattice.spec.as_mut().unwrap().account = update.account;
        }
        if mask.paths.contains(&"signing_keys".to_string()) {
            lattice.spec.as_mut().unwrap().signing_keys = update.signing_keys;
        }
        lattice.meta.as_mut().unwrap().version = Ulid::new().to_string();

        let mut buf = BytesMut::new();
        lattice.encode(&mut buf)?;
        bucket.put(name, buf.freeze()).await?;
        let resp = LatticeUpdateResponse {
            version: lattice.meta.as_ref().unwrap().version.clone(),
        };
        Ok(Some(resp.encode_to_vec().into()))
    }

    async fn delete_lattice(&self, name: &str, msg: &Message) -> anyhow::Result<Option<Bytes>> {
        let _req = LatticeDeleteRequest::decode(msg.payload.clone())?;
        let js = jetstream::new(self.client.clone());
        let bucket = js.get_key_value(LATTICE_BUCKET).await?;
        let mut lattice: Lattice = match bucket.get(name).await {
            Ok(l) => {
                if l.is_none() {
                    bail!("Lattice not found");
                }
                Lattice::decode(l.unwrap())?
            }
            Err(e) => {
                error!("Error getting lattice: {:?}", e);
                bail!("Error getting lattice: {:?}", e);
            }
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
            .request(format!("wadm.api.{}.model.list", name), "".into())
            .await?;
        if msg.payload != "[]" {
            bail!("Lattice has applications. Delete them first before deleting the lattice")
        }

        lattice
            .spec
            .as_mut()
            .unwrap()
            .status
            .as_mut()
            .unwrap()
            .phase = LatticePhase::LatticeDeleting.into();
        lattice.meta.as_mut().unwrap().version = Ulid::new().to_string();

        let buf = BytesMut::new();
        bucket.put(name, buf.freeze()).await?;
        bucket.delete(name).await?;
        let resp = LatticeDeleteResponse {
            lattice: Some(lattice),
        };
        Ok(Some(resp.encode_to_vec().into()))
    }

    async fn get_lattices(&self, msg: &Message) -> anyhow::Result<Option<Bytes>> {
        let req = LatticeGetRequest::decode(msg.payload.clone())?;
        let js = jetstream::new(self.client.clone());
        let bucket = js.get_key_value(LATTICE_BUCKET).await?;
        let mut lattices: Vec<Lattice> = Vec::new();
        for lattice in req.lattice {
            let l = match bucket.get(&lattice).await {
                Ok(l) => {
                    if l.is_none() {
                        bail!("Lattice not found")
                    }
                    Lattice::decode(l.unwrap())?
                }
                Err(e) => bail!("Failed to get lattice {}, {e}", lattice),
            };
            lattices.push(l);
        }
        Ok(Some(
            LatticeGetResponse { lattice: lattices }
                .encode_to_vec()
                .into(),
        ))
    }

    async fn watch_lattices(&self, msg: &Message) -> anyhow::Result<Option<Bytes>> {
        let reply_to = match &msg.reply {
            Some(r) => r,
            None => return Err(anyhow::anyhow!("No reply-to subject")),
        };
        let req = LatticeWatchRequest::decode(msg.payload.clone())?;
        let js = jetstream::new(self.client.clone());
        let bucket = js.get_key_value(LATTICE_BUCKET).await?;

        let watch = if req.lattices.is_empty() {
            let watch = bucket.watch_with_history(">").await?;
            vec![watch]
        } else {
            let mut watches = Vec::new();
            for lattice in req.lattices {
                let watch = bucket.watch_with_history(&lattice).await?;
                watches.push(watch);
            }
            watches
        };

        let mut streams = select_all(watch.into_iter().map(|w| w.into_stream()));
        while let Some(item) = streams.next().await {
            let entry = match item {
                Ok(e) => e,
                Err(e) => {
                    error!("Error reading from stream: {:?}", e);
                    continue;
                }
            };
            let lattice = match Lattice::decode(entry.value) {
                Ok(l) => l,
                Err(e) => {
                    error!("Error decoding lattice: {:?}", e);
                    continue;
                }
            };
            let resp = LatticeWatchResponse {
                lattice: Some(lattice),
            };
            let mut headers = HeaderMap::new();
            headers.insert("Content-Type", MIME_TYPE);
            if let Err(err) = self
                .client
                .request_with_headers(reply_to.clone(), headers, resp.encode_to_vec().into())
                .await
            {
                let k = err.kind();
                match k {
                    RequestErrorKind::NoResponders => {
                        debug!("No responders for watch request");
                        return Ok(None);
                    }
                    RequestErrorKind::TimedOut => {
                        debug!("Request timed out");
                        return Ok(None);
                    }
                    _ => {
                        error!("Error sending response: {:?}", err);
                        return Err(err.into());
                    }
                }
            };
        }
        Ok(None)
    }
}
