use log::*;
use std::{
    collections::{BTreeMap, VecDeque},
    pin::Pin,
    sync::Arc,
};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::models::WaypointSymbol;

#[derive(Debug)]
enum Message {
    ReceiveCargo(String, WaypointSymbol, i64, oneshot::Sender<()>),
    TransferCargo(
        String,
        WaypointSymbol,
        Vec<(String, i64)>,
        oneshot::Sender<()>,
    ),
    Terminate,
}

pub trait TransferActor {
    fn _transfer_cargo(
        &self,
        src_ship_symbol: String,
        dest_ship_symbol: String,
        good: String,
        units: i64,
    ) -> Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
}

pub struct CargoBroker {
    tx: mpsc::Sender<Message>,
    inner: Arc<Mutex<CargoBrokerInner>>,
}

struct CargoBrokerInner {
    rx: mpsc::Receiver<Message>,
    receivers: BTreeMap<WaypointSymbol, VecDeque<(String, i64, oneshot::Sender<()>)>>,
    senders: BTreeMap<WaypointSymbol, VecDeque<(String, Vec<(String, i64)>, oneshot::Sender<()>)>>,
}

impl Default for CargoBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl CargoBroker {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<Message>(32);
        let inner = CargoBrokerInner {
            rx,
            receivers: BTreeMap::new(),
            senders: BTreeMap::new(),
        };
        Self {
            tx,
            inner: Arc::new(Mutex::new(inner)),
        }
    }

    pub async fn receive_cargo(&self, ship_symbol: &str, waypoint: &WaypointSymbol, capacity: i64) {
        let (tx, rx) = oneshot::channel::<()>();
        self.tx
            .send(Message::ReceiveCargo(
                ship_symbol.to_string(),
                waypoint.clone(),
                capacity,
                tx,
            ))
            .await
            .unwrap();
        rx.await.unwrap()
    }

    pub async fn transfer_cargo(
        &self,
        ship_symbol: &str,
        waypoint: &WaypointSymbol,
        goods: Vec<(String, i64)>,
    ) {
        let (tx, rx) = oneshot::channel::<()>();
        self.tx
            .send(Message::TransferCargo(
                ship_symbol.to_string(),
                waypoint.clone(),
                goods,
                tx,
            ))
            .await
            .unwrap();
        rx.await.unwrap()
    }

    pub async fn terminate(&self) {
        self.tx.send(Message::Terminate).await.unwrap();
    }

    pub async fn run(&self, agent_controller: Box<dyn TransferActor + Sync + Send>) {
        let mut inner = self.inner.lock().await;
        inner.run(&agent_controller).await;
    }
}

impl CargoBrokerInner {
    async fn run(&mut self, actor: &Box<dyn TransferActor + Sync + Send>) {
        while let Some(cmd) = self.rx.recv().await {
            // debug!("cargo_broker rcv: {:?}", cmd);
            match cmd {
                Message::ReceiveCargo(ship_symbol, waypoint, capacity, rx) => {
                    let e = self.receivers.entry(waypoint.clone()).or_default();
                    e.push_back((ship_symbol, capacity, rx));
                    self.try_transfer(actor, &waypoint).await;
                }
                Message::TransferCargo(ship_symbol, waypoint, goods, rx) => {
                    let e = self.senders.entry(waypoint.clone()).or_default();
                    e.push_back((ship_symbol, goods, rx));
                    self.try_transfer(actor, &waypoint).await;
                }
                Message::Terminate => {
                    // Could do some cleanup: cancel all pending transfers, with Error responses
                    break;
                }
            }
        }
    }

    async fn try_transfer(
        &mut self,
        actor: &Box<dyn TransferActor + Send + Sync>,
        waypoint: &WaypointSymbol,
    ) {
        // we could improve the algorithm here to do fancy balancing stuff, or early release for senders
        // but for now we go simple queue based
        let receivers = self.receivers.entry(waypoint.clone()).or_default();
        let senders = self.senders.entry(waypoint.clone()).or_default();
        loop {
            debug!("try_transfer loop");
            let (ship_recv, capacity, _) = match receivers.front_mut() {
                Some(rcv) => rcv,
                None => break,
            };
            let (ship_snd, goods, _) = match senders.front_mut() {
                Some(snd) => snd,
                None => break,
            };

            let good = goods.first_mut().unwrap();
            let units = std::cmp::min(*capacity, good.1);
            actor
                ._transfer_cargo(ship_snd.clone(), ship_recv.clone(), good.0.clone(), units)
                .await;

            *capacity -= units;
            good.1 -= units;

            if *capacity == 0 {
                let (_, _, done1) = receivers.pop_front().unwrap();
                done1.send(()).unwrap();
            }
            goods.retain(|(_, units)| *units != 0);
            if goods.is_empty() {
                let (_, _, done2) = senders.pop_front().unwrap();
                done2.send(()).unwrap();
                continue;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone)]
    struct MockTransferActor {
        transfers: Arc<Mutex<Vec<(String, String, String, i64)>>>,
    }
    impl MockTransferActor {
        fn new() -> Self {
            Self {
                transfers: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }
    impl TransferActor for MockTransferActor {
        fn _transfer_cargo(
            &self,
            src_ship_symbol: String,
            dest_ship_symbol: String,
            good: String,
            units: i64,
        ) -> Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
            let mut transfers = self.transfers.lock().unwrap();
            debug!(
                "transfer_cargo: {} -> {} {} {}",
                src_ship_symbol, dest_ship_symbol, good, units
            );
            transfers.push((
                src_ship_symbol.to_string(),
                dest_ship_symbol.to_string(),
                good.to_string(),
                units,
            ));
            Box::pin(async move {})
        }
    }

    // !! We could test CargoBrokerInner separately, and then we could queue up messages more easily and in a repeatable way

    #[tokio::test]
    async fn test_cargo_broker() {
        pretty_env_logger::formatted_timed_builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Debug)
            .try_init()
            .ok();
        debug!("test_cargo_broker");

        let mock = MockTransferActor::new();
        let broker = Arc::new(CargoBroker::new());
        let waypoint = WaypointSymbol::new("X1-S1-W1");
        let broker_handle = {
            let broker = broker.clone();
            tokio::task::spawn(async move { broker.run(Box::new(mock)).await })
        };
        let ship1_handle = {
            let broker = broker.clone();
            let waypoint = waypoint.clone();
            tokio::task::spawn(async move {
                broker.receive_cargo("ship1", &waypoint, 100).await;
                debug!("ship1 free to go");
            })
        };
        let ship2_handle = {
            let broker = broker.clone();
            let waypoint = waypoint.clone();
            tokio::task::spawn(async move {
                broker
                    .transfer_cargo("ship2", &waypoint, vec![("good1".to_string(), 50)])
                    .await;
                debug!("ship2 free to go");
            })
        };
        let ship3_handle = {
            let broker = broker.clone();
            let waypoint = waypoint.clone();
            tokio::task::spawn(async move {
                broker
                    .transfer_cargo("ship3", &waypoint, vec![("good2".to_string(), 50)])
                    .await;
                debug!("ship3 free to go");
            })
        };
        ship1_handle.await.unwrap();
        ship2_handle.await.unwrap();
        ship3_handle.await.unwrap();

        broker.terminate().await;
        broker_handle.await.unwrap();
    }
}
