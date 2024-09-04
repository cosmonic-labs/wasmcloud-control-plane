use anyhow::{bail, Context};
use async_nats::{
    jetstream,
    service::{Request, ServiceExt},
    Client, ConnectOptions,
};
use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use nats_jwt::{
    authorization::{AuthRequest, AuthResponse},
    user::User,
    Claims,
};
use nkeys::{KeyPair, XKey};
use oci_distribution::{secrets::RegistryAuth, Reference};
use secrecy::ExposeSecret;
use std::str::FromStr;
use std::sync::Arc;
use std::{collections::HashMap, time::Duration};
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;
use tracing::{debug, error, info};
use wasmcloud::control::authorizer_types::{Context as AuthorizerContext, Decision};
use wasmtime::{
    component::{Component, Linker, ResourceTable, Val},
    Engine, PoolingAllocationConfig, Store,
};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiView};
use wasmtime_wasi_http::{
    body::HyperOutgoingBody,
    types::{HostFutureIncomingResponse, OutgoingRequestConfig},
    HttpResult, WasiHttpCtx, WasiHttpView,
};

use crate::config::{AuthCallout as AuthCfg, ComponentSource};

pub const WASM_MEDIA_TYPE: &str = "application/vnd.module.wasm.content.layer.v1+wasm";
pub const OCI_MEDIA_TYPE: &str = "application/vnd.oci.image.layer.v1.tar";

wasmtime::component::bindgen!({async: true, world: "authorizer"});

// TODO use the implementation in the vm crate
struct Ctx {
    wasi: WasiCtx,
    //http: WasiHttpCtx,
    table: ResourceTable,
    timeout: Duration,
}

impl WasiView for Ctx {
    fn table(&mut self) -> &mut ResourceTable {
        &mut self.table
    }

    fn ctx(&mut self) -> &mut WasiCtx {
        &mut self.wasi
    }
}

//impl WasiHttpView for Ctx {
//    fn ctx(&mut self) -> &mut WasiHttpCtx {
//        &mut self.http
//    }
//
//    fn table(&mut self) -> &mut ResourceTable {
//        &mut self.table
//    }
//
//    fn send_request(
//        &mut self,
//        request: http::Request<HyperOutgoingBody>,
//        config: OutgoingRequestConfig,
//    ) -> HttpResult<HostFutureIncomingResponse>
//    where
//        Self: Sized,
//    {
//        Ok(HostFutureIncomingResponse::pending(
//            wasmtime_wasi::runtime::spawn(
//                invoke_outgoing_handle(self.handler.clone(), request, config).in_current_span(),
//            ),
//        ))
//    }
//}

pub struct AuthCallout {
    client: Client,
    encryption_key: XKey,
    issuer_keys: Arc<Mutex<HashMap<String, KeyPair>>>,
    account: String,
    component: Option<AuthorizerPre<Ctx>>,
    runtime: Engine,
}

const ACCOUNT_PACK_SUBJECT: &str = "$SYS.REQ.CLAIMS.PACK";

struct State {}

impl AuthCallout {
    pub async fn new_from_config(
        cfg: AuthCfg,
        nats_url: &str,
        system_client: Client,
        engine: wasmtime::Engine,
    ) -> anyhow::Result<Self> {
        let enc_key = XKey::from_seed(cfg.encryption_key_seed.expose_secret())?;
        let signing_key = KeyPair::from_seed(cfg.signing_key_seed.expose_secret())?;
        let client = ConnectOptions::new()
            .credentials_file(cfg.credsfile)
            .await?
            .connect(nats_url)
            .await?;

        let mut callout = Self {
            client: client.clone(),
            encryption_key: enc_key,
            issuer_keys: Arc::new(Mutex::new(HashMap::from([(
                "default".to_string(),
                signing_key,
            )]))),
            account: cfg.account,
            component: None,
            runtime: engine,
        };

        if let Some(comp) = cfg.component {
            let c = Self::load_component(comp.source, system_client).await?;
            let compiled = Component::from_binary(&callout.runtime, &c)?;
            let mut linker: Linker<Ctx> = Linker::new(&callout.runtime);
            wasmtime_wasi::add_to_linker_async(&mut linker).context("failed to link core WASI")?;
            // TODO grant the component access to wasi:http
            //wasmtime_wasi_http::add_to_linker_async(&mut linker).context("failed to link wasi:http")?;
            //AuthorizerClient::instantiate((), component, linker)

            let linked = linker
                .instantiate_pre(&compiled)
                .context("failed to pre instantiate component")?;
            let bound = AuthorizerPre::<Ctx>::new(linked)?;
            callout.component = Some(bound);
        }
        Ok(callout)
    }

    async fn load_component(component: ComponentSource, client: Client) -> anyhow::Result<Bytes> {
        let js = jetstream::new(client);
        match &component {
            ComponentSource::Nats(n) => {
                info!(store = n.bucket, component = n.key, "Loading component");
                let store = js.get_object_store(&n.bucket).await.unwrap();
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
                    .pull(&reference, &auth, vec![WASM_MEDIA_TYPE, OCI_MEDIA_TYPE])
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

    async fn invoke_component(&self) -> anyhow::Result<Option<Decision>> {
        if self.component.is_none() {
            return Ok(None);
        }
        info!("invoking component");

        let cmp = self.component.as_ref().unwrap();
        // TODO this needs to be less than the auth callout timeout. Not sure how to configure that
        let duration = Duration::from_secs(1);

        let table = ResourceTable::new();
        // TODO do we need to do something different here?
        let wasi = WasiCtxBuilder::new()
            .args(&["main.wasm"])
            .inherit_stderr()
            .build();

        let mut store = Store::new(
            &self.runtime,
            Ctx {
                wasi,
                table,
                timeout: duration,
            },
        );
        store.set_epoch_deadline(duration.as_secs());
        let ctx = AuthorizerContext {
            lattice_id: "".into(),
            token: None,
            username: None,
            password: None,
            tls: None,
        };

        let auth = cmp.instantiate_async(&mut store).await?;
        let res = auth
            .wasmcloud_control_callout_authorizer()
            .call_authorized(&mut store, &ctx)
            .await
            .context("failed to call authorized")?;

        match res {
            Err(e) => {
                error!("error in callout: {}", e);
                Err(anyhow::anyhow!("error in callout: {}", e))
            }
            Ok(decision) => Ok(Some(decision)),
        }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let svc = self
            .client
            .service_builder()
            // TODO take the name from configuration in the struct
            .start("wasmcloud-auth-callout", "0.0.1")
            .await
            .unwrap();
        let group = svc.group("$SYS.REQ.USER");
        let mut endpoint = group
            .endpoint("AUTH")
            .await
            .map_err(|e| anyhow::anyhow!("failed to create endpoint: {e}"))?;

        info!("listening for requests");
        // TODO parallelize this
        while let Some(req) = endpoint.next().await {
            if let Err(e) = self.handle_message(req).await {
                error!("failed to handle message: {}", e);
                continue;
            };
        }
        Ok(())
    }

    async fn handle_message(&self, req: Request) -> anyhow::Result<()> {
        info!("received request");
        let mut token = vec![];
        if let Some(headers) = &req.message.headers {
            if let Some(xkey) = headers.get("Nats-Server-Xkey") {
                let key = XKey::from_public_key(xkey.as_str())?;
                token = self.encryption_key.open(&req.message.payload, &key)?;
            } else {
                token = req.message.payload.to_vec();
            }
        }

        let str = String::from_utf8(token).unwrap();
        debug!("received token: {}", str);
        let auth_req: AuthRequest = match Claims::<AuthRequest>::decode(&str) {
            Ok(j) => j.payload().clone(),
            Err(e) => {
                error!("failed to decode JWT: {}", e);
                let guard = self.issuer_keys.lock().await;
                let key = guard.keys().last().unwrap();
                let key = guard.get(key).unwrap();
                if let Err(e) = self
                    .respond(
                        &req,
                        "".into(),
                        "".into(),
                        "".into(),
                        Some(format!("failed to decode JWT: {}", e)),
                        key,
                    )
                    .await
                {
                    bail!("failed to respond: {}", e);
                };
                return Ok(());
            }
        };
        let user_nkey = auth_req.user_nkey.clone();
        let server_id = auth_req.server.clone();
        let user_key = auth_req.user_nkey;
        debug!("user context: {:?}", auth_req.connect_opts);
        let mut user_claims = User::new_claims(user_key, user_nkey.clone());

        match self.invoke_component().await {
            Ok(None) => {
                // This should only trigger if there is no component configured
                info!("no decision from component");
            }
            Err(e) => {
                error!("error in component: {}", e);
                let guard = self.issuer_keys.lock().await;
                let key = guard.keys().last().unwrap();
                let key = guard.get(key).unwrap();
                if let Err(e) = self
                    .respond(
                        &req,
                        user_nkey,
                        server_id.id,
                        "".into(),
                        Some(format!("error in component: {}", e)),
                        key,
                    )
                    .await
                {
                    bail!("failed to respond: {}", e);
                };
                return Ok(());
            }
            Ok(Some(decision)) => {
                if decision.allowed {
                    info!("allowed");
                } else {
                    info!("denied");
                    let guard = self.issuer_keys.lock().await;
                    let key = guard.keys().last().unwrap();
                    let key = guard.get(key).unwrap();
                    if let Err(e) = self
                        .respond(
                            &req,
                            user_nkey,
                            server_id.id,
                            "".into(),
                            Some("denied".into()),
                            key,
                        )
                        .await
                    {
                        bail!("disallowed {}", e);
                    };
                    return Ok(());
                }
            }
        }

        // Sign with account signing key
        let key = self.issuer_keys.lock().await;
        // TODO based on what the auth callout component tells us, actually select the right key by
        // role name.
        let k = key.keys().last().unwrap();
        let key = key.get(k).unwrap();
        user_claims.nats.issuer_account = Some(self.account.clone());
        let enc = match user_claims.encode(key) {
            Ok(e) => e,
            Err(e) => {
                if let Err(e) = self
                    .respond(
                        &req,
                        user_nkey,
                        server_id.id,
                        "".into(),
                        Some(e.to_string()),
                        key,
                    )
                    .await
                {
                    bail!("failed to respond: {}", e);
                };
                return Ok(());
            }
        };
        if let Err(e) = self
            .respond(&req, user_nkey, server_id.id, enc, None, key)
            .await
        {
            bail!("failed to respond: {}", e);
        };
        Ok(())
    }

    async fn respond(
        &self,
        req: &Request,
        user_nkey: String,
        server_id: String,
        user_jwt: String,
        err: Option<String>,
        signing_key: &KeyPair,
    ) -> anyhow::Result<()> {
        let mut claim = AuthResponse::generic_claim(user_nkey);
        claim.aud = Some(server_id);
        claim.nats.jwt = user_jwt;
        if let Some(e) = err {
            claim.nats.error = e;
        }

        let jwt = claim.encode(signing_key).unwrap();
        if let Some(headers) = &req.message.headers {
            if let Some(xkey) = headers.get("Nats-Server-Xkey") {
                let key = XKey::from_public_key(xkey.as_str()).unwrap();
                let data = match self.encryption_key.seal(jwt.as_bytes(), &key) {
                    Ok(d) => d,
                    Err(e) => {
                        // TODO this needs to change the response to an error by setting it in the
                        // response jwt
                        req.respond(Ok(e.to_string().into())).await.unwrap();
                        return Err(e.into());
                    }
                };
                req.respond(Ok(data.into())).await?;
                return Ok(());
            }
        }

        req.respond(Ok(jwt.into())).await?;
        Ok(())
    }
}
