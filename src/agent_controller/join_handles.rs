//! A small module for managing and tracking asynchronous task join handles.
//!
//! Provides a thread-safe way to collect and wait for multiple async tasks
//! to complete. It uses a combination of channels and a futures unordered collection
//! to handle both existing and new tasks that may be added while waiting.
//!
//! The alternatives are:
//! With FuturesUnordered alone, or tokio::JoinSet, you have to lock it to wait on the result, but if it's
//! locked, you can't add new handles. tokio_util's TaskTracker does allow insertion without mutable access,
//! however it doesn't actually allow you to handle the result of the tasks.
//!

use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use log::{debug, info};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::{JoinError, JoinHandle};

#[derive(Debug)]
pub struct JoinHandles {
    tx: mpsc::UnboundedSender<(String, JoinHandle<()>)>,
    hdl: Arc<tokio::sync::Mutex<JoinHandle<()>>>,
}
impl Default for JoinHandles {
    fn default() -> Self {
        Self::new()
    }
}

impl JoinHandles {
    pub fn new() -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<(String, JoinHandle<()>)>();

        let hdl = tokio::spawn(async move {
            info!("join handles started");
            use futures::StreamExt as _;
            let mut handles: FuturesUnordered<BoxFuture<'static, (String, Result<(), JoinError>)>> =
                FuturesUnordered::new();

            loop {
                let next_handle = async {
                    if !handles.is_empty() {
                        handles.next().await.unwrap()
                    } else {
                        futures::future::pending().await
                    }
                };

                tokio::select! {
                    result = next_handle => {
                        let (name, hdl_ret) = result;
                        let result = match &hdl_ret {
                            Ok(_) => "completed",
                            Err(_e) => "failed",
                        };
                        debug!("handle '{}' {}", name, result);
                        hdl_ret.unwrap();
                    }
                    handle = rx.recv() => {
                        let (name, handle) = handle.unwrap();
                        debug!("adding new handle '{}'", name);
                        handles.push(wait_handle(name, handle));
                    }
                }
            }
        });

        Self {
            tx,
            hdl: Arc::new(tokio::sync::Mutex::new(hdl)),
        }
    }
    pub fn push(&self, name: &str, handle: JoinHandle<()>) {
        self.tx.send((name.to_string(), handle)).unwrap();
    }
    pub async fn join(&self) {
        let mut hdl = self.hdl.lock().await;
        (&mut *hdl).await.unwrap();
    }
}

fn wait_handle(
    name: String,
    handle: JoinHandle<()>,
) -> BoxFuture<'static, (String, Result<(), JoinError>)> {
    Box::pin(async move { (name, handle.await) })
}
