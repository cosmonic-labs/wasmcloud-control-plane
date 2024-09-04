use crate::secrets::SecretsClient;
use async_nats::Client as NatsClient;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use wasmtime::{
    component::{Resource, ResourceTable},
    PoolingAllocationConfig,
};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiView};

// TODO we probably want some parameters here
pub fn configure_wasmtime() -> anyhow::Result<wasmtime::Engine> {
    let mut engine_config = wasmtime::Config::default();
    engine_config.wasm_component_model(true);
    engine_config.async_support(true);
    engine_config.epoch_interruption(true);
    engine_config.memory_init_cow(false);
    //engine_config.wasm_memory64(true);
    let pooling_config = PoolingAllocationConfig::default();
    engine_config.allocation_strategy(wasmtime::InstanceAllocationStrategy::Pooling(
        pooling_config,
    ));
    wasmtime::Engine::new(&engine_config)
}

pub struct Ctx {
    pub wasi: WasiCtx,
    //http: WasiHttpCtx,
    pub table: ResourceTable,
    pub timeout: Duration,
    // TODO make this optional, since if the point of this Ctx is to be universal then it shouldn't
    // always expose an internal secrets client.
    pub secrets: SecretsClient,
    pub sys_client: Option<NatsClient>,
}

impl Ctx {
    pub fn new(
        wasi: WasiCtx,
        timeout: Duration,
        secrets: SecretsClient,
        sys_client: Option<NatsClient>,
    ) -> Self {
        Self {
            wasi,
            table: ResourceTable::new(),
            timeout,
            secrets,
            sys_client,
        }
    }
}

impl WasiView for Ctx {
    fn table(&mut self) -> &mut ResourceTable {
        &mut self.table
    }

    fn ctx(&mut self) -> &mut WasiCtx {
        &mut self.wasi
    }
}

// From the wasmcloud host crate
pub mod wasmtime_bindings {
    mod secrets {
        use super::wasmcloud::secrets::store::SecretValue;

        pub type Secret = std::sync::Arc<String>;

        impl secrecy::Zeroize for SecretValue {
            fn zeroize(&mut self) {
                match self {
                    SecretValue::String(s) => s.zeroize(),
                    SecretValue::Bytes(b) => b.zeroize(),
                }
            }
        }

        /// Permits cloning
        impl secrecy::CloneableSecret for SecretValue {}
        /// Provides a `Debug` impl (by default `[[REDACTED]]`)
        impl secrecy::DebugSecret for SecretValue {}
    }

    wasmtime::component::bindgen!({
        world: "interfaces",
        async: true,
        with: {
           "wasmcloud:secrets/store/secret": secrets::Secret,
        },
    });
}

use wasmtime_bindings::wasmcloud::{
    control::lattice_account_update,
    secrets::{
        reveal,
        store::{self, *},
    },
};

#[async_trait]
impl store::Host for Ctx {
    async fn get(&mut self, key: String) -> anyhow::Result<Resource<Secret>, SecretsError> {
        match self.secrets.get(&key).await {
            Ok(_secret) => {
                let secret_resource = self
                    .table
                    .push(Arc::new(key))
                    .map_err(|e| SecretsError::Io(e.to_string()))?;
                Ok(secret_resource)
            }
            Err(_e) => Err(SecretsError::NotFound),
        }
    }
}

#[async_trait]
impl reveal::Host for Ctx {
    // TODO fix this because it should be infallable. Consider declaring a Secret trait and binding
    // the secret wit to that instead like we do in the host
    async fn reveal(&mut self, secret: Resource<Secret>) -> SecretValue {
        let key = self.table.get(&secret).unwrap();
        match self.secrets.get(key).await {
            Ok(secret) => SecretValue::String(secret),
            // TODO this definitely doesn't work
            Err(_) => SecretValue::String("".to_string()),
        }
    }
}

impl HostSecret for Ctx {
    fn drop(&mut self, secret: Resource<Secret>) -> std::result::Result<(), anyhow::Error> {
        self.table.delete(secret)?;
        Ok(())
    }
}

#[async_trait]
impl lattice_account_update::Host for Ctx {
    async fn update(&mut self, account: String) -> Result<(), String> {
        if self.sys_client.is_none() {
            return Err("NATS SYS client is not enabled".to_string());
        }
        let sys_client = self.sys_client.as_ref().unwrap();
        sys_client
            .request("$SYS.REQ.CLAIMS.UPDATE", account.into())
            .await
            .map_err(|e| e.to_string())?;

        Ok(())
    }

    async fn store_key(&mut self, key: String, value: String) -> Result<(), String> {
        self.secrets
            .set(&key, &value)
            .await
            .map_err(|e| e.to_string())?;
        //if self.sys_client.is_none() {
        //    return Err("NATS SYS client is not enabled".to_string());
        //}
        //let sys_client = self.sys_client.as_ref().unwrap();
        //sys_client
        //    .request("$SYS.REQ.KEYS.STORE", format!("{}:{}", key, value).into())
        //    .await
        //    .map_err(|e| e.to_string())?;

        Ok(())
    }

    async fn delete_key(&mut self, key: String) -> Result<(), String> {
        if self.sys_client.is_none() {
            return Err("NATS SYS client is not enabled".to_string());
        }

        let sys_client = self.sys_client.as_ref().unwrap();
        sys_client
            .request("$SYS.REQ.KEYS.DELETE", key.into())
            .await
            .map_err(|e| e.to_string())?;

        Ok(())
    }
}
