use anyhow::bail;
use async_nats::{
    jetstream::{
        self,
        consumer::{pull, Consumer, DeliverPolicy},
        AckKind, Message,
    },
    Client,
};
use futures::StreamExt;
use std::sync::Arc;
use tracing::error;

pub(crate) struct Worker {
    pub stream: String,
    consumer: Consumer<pull::Config>,
    parallelism: u32,
    client: Client,
}

const WORKER_SUBJECT: &str = "wasmcloud.internal.control_plane.workqueue";

impl Worker {
    pub async fn new(stream: String, consumer_name: String, client: Client) -> Worker {
        let js = jetstream::new(client.clone());

        let consumer = js
            .create_consumer_on_stream(
                pull::Config {
                    // TODO add additional identifier for this particular instance of the control
                    // plane
                    name: Some(consumer_name.clone()),
                    deliver_policy: DeliverPolicy::All,
                    max_ack_pending: -1,
                    max_deliver: 10,
                    filter_subjects: vec![format!("{}.>", stream.clone())],
                    ..Default::default()
                },
                stream.clone(),
            )
            .await
            .unwrap();
        Worker {
            stream,
            consumer,
            parallelism: 5,
            client: client.clone(),
        }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let mut messages = self.consumer.messages().await?;
        while let Some(msg) = messages.next().await {
            let msg = match msg {
                Ok(msg) => msg,
                Err(e) => {
                    error!("Error receiving message: {:?}", e);
                    continue;
                }
            };

            // TODO will this actually work nicely? We don't want to block the consumer from
            // recieving messages and we want to handle anything we get async. Do we need to
            // actually lock on updates to the same subject though?
            let handler = MessageHandler {};
            tokio::spawn(async move {
                handler.handle_message(msg).await;
            });
        }
        Ok(())
    }
}

struct MessageHandler {}

impl MessageHandler {
    async fn handle_message(&self, msg: Message) {
        let progress_copy = msg.clone();
        let progress = tokio::spawn(async move {
            // TODO this should be based on a fraction of the max wait setting of the consumer
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3));
            loop {
                interval.tick().await;
                _ = progress_copy
                    .ack_with(AckKind::Progress)
                    .await
                    .map_err(|e| error!("Error acking message: {:?}", e));
            }
        });

        tokio::select! {
            _ = progress => {}
            _ = self.handle(msg) => {}
        }
    }
    async fn handle(&self, msg: Message) -> anyhow::Result<()> {
        let subject = msg.subject.clone().into_string();
        let command = subject.replace(format!("{WORKER_SUBJECT}.").as_str(), "");
        let command_parts: Vec<&str> = command.split(".").collect();
        let entity = command_parts[0];
        let action = command_parts[1];

        // TODO do something!

        if let Err(e) = msg.double_ack().await {
            error!("Error double acking message: {:?}", e);
            bail!("Error double acking message: {:?}", e);
        }
        Ok(())
    }
}
