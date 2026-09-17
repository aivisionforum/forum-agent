use forum_contracts::{EventType, SessionSpec, SessionState, SessionTransition};
use forum_runtime::{DurableProducer, Endpoint, RpcError, RuntimeConfig, UdsServer};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use uuid::Uuid;
struct Fixture {
    root: PathBuf,
    config: RuntimeConfig,
}
impl Fixture {
    fn new() -> Self {
        let root = PathBuf::from("/tmp").join(format!("f03-outbox-{}", Uuid::new_v4()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let config = RuntimeConfig {
            endpoint: Endpoint::new(root.join("core.sock")),
            session: SessionSpec {
                session_id: Uuid::new_v4(),
                event_id: Uuid::new_v4(),
                room_id: Uuid::new_v4(),
                owner_device_id: Uuid::new_v4(),
                title: "outbox transport tests".into(),
            },
            producer_dir: root.join("producer"),
            producer_name: "test".into(),
        };
        Self { root, config }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn payload() -> SessionTransition {
    SessionTransition {
        expected_state: SessionState::Created,
        next_state: SessionState::Preparing,
        reason: "test".into(),
    }
}
#[test]
fn producer_lock_and_previous_process_evidence_survive_reopen() {
    let fixture = Fixture::new();
    let first = DurableProducer::open(fixture.config.clone()).unwrap();
    let run = first.run_id();
    assert!(DurableProducer::open(fixture.config.clone()).is_err());
    drop(first);
    let next = DurableProducer::open(fixture.config.clone()).unwrap();
    let prior = next.prior_run_records().unwrap();
    assert_eq!(prior.len(), 1);
    assert_eq!(prior[0].run_id, run);
    assert_eq!(prior[0].owner_pid, std::process::id());
}
#[test]
fn mismatched_ack_never_deletes_the_stable_event() {
    let fixture = Fixture::new();
    let mut server = UdsServer::bind(fixture.config.endpoint.clone(), |_| {
        Ok(json!({"message_id":Uuid::new_v4(),"store_seq":1,"duplicate":false}))
    })
    .unwrap();
    let mut producer = DurableProducer::open(fixture.config.clone()).unwrap();
    let pending = producer
        .append(EventType::SessionChanged, &payload())
        .unwrap();
    let body = pending.event.clone();
    assert_eq!(
        producer.flush_one(pending.message_id).unwrap_err().code,
        "INVALID_RECEIPT"
    );
    assert_eq!(producer.pending().unwrap()[0].event, body);
    server.stop().unwrap();
}
#[test]
fn failed_batch_stops_at_first_rpc_and_preserves_every_pending_body() {
    let fixture = Fixture::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut server = UdsServer::bind(fixture.config.endpoint.clone(), move |_| {
        observed.fetch_add(1, Ordering::AcqRel);
        Err(RpcError::new("CORE_OFFLINE", true, "unavailable"))
    })
    .unwrap();
    let mut producer = DurableProducer::open(fixture.config.clone()).unwrap();
    producer
        .append(EventType::SessionChanged, &payload())
        .unwrap();
    producer
        .append(EventType::SessionChanged, &payload())
        .unwrap();
    let result = producer.flush_pending();
    assert_eq!(result.len(), 1);
    assert!(result[0].1.is_err());
    assert_eq!(calls.load(Ordering::Acquire), 1);
    assert_eq!(producer.pending().unwrap().len(), 2);
    server.stop().unwrap();
}
#[test]
fn lost_ack_replays_identical_message_after_a_new_run() {
    let fixture = Fixture::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut server = UdsServer::bind(fixture.config.endpoint.clone(), move |req| {
        let count = observed.fetch_add(1, Ordering::AcqRel);
        if count == 0 {
            return Err(RpcError::new(
                "LOST_ACK",
                true,
                "commit happened, response was lost",
            ));
        }
        Ok(json!({"message_id":req.params["event"]["message_id"],"store_seq":7,"duplicate":true}))
    })
    .unwrap();
    let mut first = DurableProducer::open(fixture.config.clone()).unwrap();
    let event = first.append(EventType::SessionChanged, &payload()).unwrap();
    assert!(first.flush_one(event.message_id).is_err());
    drop(first);
    let mut next = DurableProducer::open(fixture.config.clone()).unwrap();
    assert_eq!(next.pending().unwrap()[0].event, event.event);
    let receipt = next.flush_one(event.message_id).unwrap();
    assert!(receipt.duplicate);
    assert!(next.pending().unwrap().is_empty());
    server.stop().unwrap();
}
