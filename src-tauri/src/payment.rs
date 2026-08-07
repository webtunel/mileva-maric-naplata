use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crossbeam_channel::{unbounded, RecvTimeoutError, Sender};
use parking_lot::Mutex;
use tauri::Emitter;

use crate::{
    models::{KioskError, PaymentEvent, PaymentProgress},
    nv9::{EscrowDecision, Nv9Config},
};

/// How a payment session ended. `inserted` is the cash actually taken in EVERY variant —
/// the caller must persist it (finalize on Paid, mark abandoned on Cancelled/Failed).
pub enum PaymentEnd {
    Paid(i64),
    Cancelled(i64),
    Failed(KioskError, i64),
}

type DoneCallback = Box<dyn FnOnce(PaymentEnd) + Send + 'static>;

/// Abandon a session that has taken no cash for this long (visitor walked off).
const INACTIVITY_TIMEOUT: Duration = Duration::from_secs(120);
/// After a cancel, keep draining events this long to catch a note already travelling to
/// the stacker, so its credit is recorded rather than silently swallowed.
const CANCEL_DRAIN: Duration = Duration::from_secs(3);

pub struct PaymentHandle {
    stop: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    decisions_tx: Sender<EscrowDecision>,
    coordinator: Mutex<Option<JoinHandle<()>>>,
}

pub fn start(
    app: tauri::AppHandle,
    cfg: Nv9Config,
    total_rsd: i64,
    on_credit: impl Fn(i64) + Send + 'static,
    on_done: impl FnOnce(PaymentEnd) + Send + 'static,
) -> PaymentHandle {
    let (events_tx, events_rx) = unbounded();
    let (decisions_tx, decisions_rx) = unbounded();
    let stop = Arc::new(AtomicBool::new(false));
    let cancel = Arc::new(AtomicBool::new(false));
    let on_done: Arc<Mutex<Option<DoneCallback>>> = Arc::new(Mutex::new(Some(Box::new(on_done))));

    // NV9 driver thread. Fatal driver errors are surfaced as PaymentEvent::Error by the
    // driver itself; if it simply exits, dropping events_tx disconnects the event loop.
    let validator_stop = Arc::clone(&stop);
    let validator = thread::spawn(move || {
        #[cfg(feature = "simulate")]
        let result = crate::nv9::run_simulator(cfg, events_tx, decisions_rx, validator_stop);
        #[cfg(not(feature = "simulate"))]
        let result = crate::nv9::run_validator(cfg, events_tx, decisions_rx, validator_stop);
        let _ = result;
    });

    let coordinator_stop = Arc::clone(&stop);
    let coordinator_cancel = Arc::clone(&cancel);
    let coordinator_decisions = decisions_tx.clone();
    let coordinator_on_done = Arc::clone(&on_done);

    let coordinator = thread::spawn(move || {
        let mut inserted = 0_i64;
        let mut last_activity = Instant::now();
        let mut draining_until: Option<Instant> = None;

        let end: PaymentEnd = loop {
            // Enter drain mode on an explicit cancel or an inactivity timeout.
            if draining_until.is_none()
                && (coordinator_cancel.load(Ordering::SeqCst)
                    || last_activity.elapsed() >= INACTIVITY_TIMEOUT)
            {
                coordinator_cancel.store(true, Ordering::SeqCst);
                draining_until = Some(Instant::now() + CANCEL_DRAIN);
                // Reject anything currently in escrow; stop accepting new notes.
                let _ = coordinator_decisions.send(EscrowDecision::Reject);
            }
            if let Some(deadline) = draining_until {
                if Instant::now() >= deadline {
                    break PaymentEnd::Cancelled(inserted);
                }
            }

            let event = match events_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    // Driver thread gone. Cancelling → clean cancel; else the link died.
                    break if coordinator_cancel.load(Ordering::SeqCst) {
                        PaymentEnd::Cancelled(inserted)
                    } else {
                        PaymentEnd::Failed(
                            KioskError::Hardware("veza sa uređajem prekinuta".into()),
                            inserted,
                        )
                    };
                }
            };

            match event {
                PaymentEvent::Connected => emit_progress(
                    &app,
                    inserted,
                    total_rsd,
                    false,
                    Some("Uređaj povezan".into()),
                ),
                PaymentEvent::Disconnected => emit_progress(
                    &app,
                    inserted,
                    total_rsd,
                    false,
                    Some("Uređaj nije povezan".into()),
                ),
                PaymentEvent::Notice { message } => {
                    emit_progress(&app, inserted, total_rsd, false, Some(message))
                }
                PaymentEvent::NoteInEscrow { value_rsd } => {
                    last_activity = Instant::now();
                    let decision = if coordinator_cancel.load(Ordering::SeqCst)
                        || inserted.saturating_add(value_rsd) > total_rsd
                    {
                        EscrowDecision::Reject
                    } else {
                        EscrowDecision::Accept
                    };
                    let _ = coordinator_decisions.send(decision);
                }
                PaymentEvent::Credited {
                    value_rsd: _,
                    total_inserted_rsd,
                } => {
                    inserted = total_inserted_rsd;
                    last_activity = Instant::now();
                    // Persist the running total on every credit BEFORE anything else, so
                    // cash in the box is never only in memory (survives power loss).
                    on_credit(inserted);
                    emit_progress(&app, inserted, total_rsd, false, None);
                    if inserted >= total_rsd {
                        break PaymentEnd::Paid(inserted);
                    }
                }
                PaymentEvent::NoteReturned { value_rsd: _ } => {
                    last_activity = Instant::now();
                    emit_progress(
                        &app,
                        inserted,
                        total_rsd,
                        false,
                        Some("Novčanica vraćena (nema kusura)".into()),
                    );
                }
                PaymentEvent::Error { message } => {
                    break PaymentEnd::Failed(KioskError::Hardware(message), inserted);
                }
            }
        };

        if let PaymentEnd::Paid(value) = &end {
            emit_progress(&app, *value, total_rsd, true, None);
        }
        // Tear down the driver, then resolve exactly once. EVERY exit path reaches here,
        // so on_done can never be left unresolved (that would hang start_payment).
        coordinator_stop.store(true, Ordering::SeqCst);
        finish_once(&coordinator_on_done, end);
        let _ = validator.join();
    });

    PaymentHandle {
        stop,
        cancel,
        decisions_tx,
        coordinator: Mutex::new(Some(coordinator)),
    }
}

impl PaymentHandle {
    /// Request cancellation. Does NOT drop the completion callback — the coordinator
    /// drains in-flight credits and resolves with `Cancelled(inserted)` so partial cash
    /// is still recorded.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        let _ = self.decisions_tx.send(EscrowDecision::Reject);
    }
}

impl Drop for PaymentHandle {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.decisions_tx.send(EscrowDecision::Reject);
        if let Some(coordinator) = self.coordinator.get_mut().take() {
            // A completion callback can indirectly drop its own handle; never self-join.
            if coordinator.thread().id() != thread::current().id() {
                let _ = coordinator.join();
            }
        }
    }
}

fn finish_once(callback: &Mutex<Option<DoneCallback>>, end: PaymentEnd) {
    let callback = callback.lock().take();
    if let Some(callback) = callback {
        callback(end);
    }
}

fn emit_progress(
    app: &tauri::AppHandle,
    inserted_rsd: i64,
    total_rsd: i64,
    complete: bool,
    note: Option<String>,
) {
    let _ = app.emit(
        "payment://progress",
        PaymentProgress {
            inserted_rsd,
            total_rsd,
            complete,
            note,
        },
    );
}
