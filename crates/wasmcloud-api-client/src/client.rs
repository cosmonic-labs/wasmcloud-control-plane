use async_nats::{Client as NatsClient, HeaderMap, Request, Subscriber};
use futures::{Stream, StreamExt};
use prost::Message;
use std::cell::LazyCell;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::error;

use wasmcloud_api_types::*;

pub const MIME_TYPE: &str = "application/x-protobuf";
pub const LATTICE_SUBJECT: &str = "wasmcloud.lattice.v1alpha1_proto";

pub const CONTENT_TYPE_HEADERS: LazyCell<HeaderMap> = LazyCell::new(|| {
    let mut headers = HeaderMap::new();
    headers.insert("Content-Type", MIME_TYPE);
    headers
});

pub struct LatticeClient {
    nats: NatsClient,
}

struct LatticeWatcher {
    recv: mpsc::Receiver<LatticeWatchResponse>,
    send: mpsc::Sender<LatticeWatchResponse>,
    client: NatsClient,
    token: CancellationToken,
}

impl LatticeWatcher {
    pub fn new(client: NatsClient) -> Self {
        // Arbitrary buffer size here, should decide on a real one
        let (send, recv) = mpsc::channel(100);
        let token = CancellationToken::new();
        Self {
            send,
            recv,
            client,
            token,
        }
    }

    async fn subscribe(&self) -> anyhow::Result<String> {
        let inbox = self.client.new_inbox();
        let mut sub = self.client.subscribe(inbox.clone()).await.unwrap();
        let jh = tokio::spawn({
            let client = self.client.clone();
            let send = self.send.clone();
            async move {
                while let Some(msg) = sub.next().await {
                    let r = match LatticeWatchResponse::decode(msg.payload) {
                        Ok(r) => {
                            client.publish(msg.reply.unwrap(), "".into()).await.unwrap();
                            r
                        }
                        Err(e) => {
                            error!("Error decoding LatticeWatchResponse: {}", e);
                            continue;
                        }
                    };
                    send.send(r).await.unwrap();
                }
            }
        });

        // If the token is cancelled, abort the join handle which stops the subscription
        tokio::spawn({
            let token = self.token.clone();
            async move {
                token.cancelled().await;
                jh.abort();
            }
        });

        Ok(inbox)
    }
}

impl Drop for LatticeWatcher {
    fn drop(&mut self) {
        self.recv.close();
        self.token.cancel();
    }
}

impl Stream for LatticeWatcher {
    type Item = LatticeWatchResponse;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.recv.poll_recv(cx)
    }
}

impl LatticeClient {
    pub async fn new(client: NatsClient) -> Self {
        Self { nats: client }
    }

    pub async fn add(
        &self,
        name: &str,
        req: LatticeAddRequest,
    ) -> anyhow::Result<LatticeAddResponse> {
        let headers = CONTENT_TYPE_HEADERS.clone();
        let resp = self
            .nats
            .request_with_headers(
                format!("{LATTICE_SUBJECT}.add.{}", name),
                headers,
                req.encode_to_vec().into(),
            )
            .await?;

        Ok(LatticeAddResponse::decode(resp.payload)?)
    }

    pub async fn watch(
        &self,
        req: LatticeWatchRequest,
    ) -> anyhow::Result<impl Stream<Item = LatticeWatchResponse>> {
        let headers = CONTENT_TYPE_HEADERS.clone();

        let watcher = LatticeWatcher::new(self.nats.clone());
        let inbox = watcher.subscribe().await?;

        let req = Request::new()
            .payload(req.encode_to_vec().into())
            .headers(headers)
            .inbox(inbox.clone());
        self.nats
            .send_request(format!("{LATTICE_SUBJECT}.watch"), req)
            .await?;

        Ok(watcher)
    }

    pub async fn get() {}
}
