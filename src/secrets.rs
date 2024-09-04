use anyhow::bail;
use async_nats::{jetstream, jetstream::kv::Config as KvConfig, Client};
use nkeys::{KeyPair, XKey};

const SECRETS_BUCKET: &str = "WASMCLOUD_ACCOUNT_KEYS";

#[derive(Clone)]
pub struct SecretsClient {
    client: Client,
    xkey: XKey,
}

impl SecretsClient {
    pub fn new(client: Client, xkey: XKey) -> Self {
        Self { client, xkey }
    }

    pub async fn set(&self, key: &str, data: &str) -> anyhow::Result<()> {
        let js = jetstream::new(self.client.clone());

        let bucket = match js.get_key_value(SECRETS_BUCKET).await {
            Ok(b) => b,
            Err(e) => {
                // TODO tune replicas and whatnot if needed
                if e.kind() == jetstream::context::KeyValueErrorKind::GetBucket {
                    js.create_key_value(KvConfig {
                        bucket: SECRETS_BUCKET.to_string(),
                        ..Default::default()
                    })
                    .await?
                } else {
                    bail!("Failed to get bucket: {}", e)
                }
            }
        };

        let encrypted = self
            .xkey
            .seal(data.as_bytes(), &self.xkey)
            .map_err(|e| anyhow::anyhow!("unable to encrypt secret :{}", e))?;
        bucket.put(key, encrypted.into()).await?;
        Ok(())
    }

    pub async fn get(&self, key: &str) -> anyhow::Result<String> {
        let js = jetstream::new(self.client.clone());

        let bucket = match js.get_key_value(SECRETS_BUCKET).await {
            Ok(b) => b,
            Err(e) => {
                // TODO tune replicas and whatnot if needed
                if e.kind() == jetstream::context::KeyValueErrorKind::GetBucket {
                    js.create_key_value(KvConfig {
                        bucket: SECRETS_BUCKET.to_string(),
                        ..Default::default()
                    })
                    .await?
                } else {
                    bail!("Failed to get bucket: {}", e)
                }
            }
        };

        let secret = bucket.get(key).await?;
        if secret.is_none() {
            bail!("Secret not found")
        }
        let secret = secret.unwrap();
        let unencrypted = self
            .xkey
            .open(&secret, &self.xkey)
            .map_err(|e| anyhow::anyhow!("unable to decrypt secret :{}", e))?;
        Ok(std::str::from_utf8(&unencrypted)?.to_string())
    }
}
