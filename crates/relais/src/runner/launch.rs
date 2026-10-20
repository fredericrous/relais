//! Launching a dispatch: admission, the write lease and usage booking.

use super::{
    cost_booked, dispatch_ended, effort_str, Arc, AtomicBool, BlockCode, Decision, DispatchId,
    DispatchRequest, DispatchSource, Duration, Event, Gate, GateError, Instant, LaunchResult,
    Launched, LedgerError, Limit, ManagedDispatch, MicroUsd, Observation, Ordering, Reason,
    Refusal, ReleaseWriteOutcome, ResourceClass, RunEngine, RunError, Terminal, UsageEvent,
    WaitKind, WriteLeaseOutcome, ADMISSION_POLL, HEARTBEAT_FAILURES_ALLOWED,
};

impl<'a> RunEngine<'a> {
    /// Marks a dispatch row `native_run`, with the agent once one is bound:
    /// from the moment the row exists, so it never disagrees with what the
    /// coordinator is told.
    pub(crate) fn record_native_row(
        &self,
        dispatch_id: &DispatchId,
        agent_id: Option<&str>,
    ) -> Result<(), LedgerError> {
        self.config
            .ledger
            .record_native_dispatch(dispatch_id, agent_id)
    }

    /// Books a dispatch's usage. A native dispatch's goes in with its
    /// agent's message ids, so an older relais's usage import skips them;
    /// without the ids the usage is booked anyway and the run says so.
    pub(crate) fn book_usage(
        &self,
        event: &UsageEvent,
        result: &LaunchResult,
    ) -> Result<(), LedgerError> {
        // Only what the dispatch's per-turn bookings have not already
        // booked: `native usage` may have booked most of it.
        let booked = self.config.ledger.record_usage_delta(
            event,
            &result.booked_message_ids,
            crate::ledger::Booking::Final,
        )?;
        if let Some(booked) = booked {
            self.events.emit(cost_booked(&booked, &result.unpriced));
        }
        if result.session_id.is_some() && result.booked_message_ids.is_empty() {
            self.events.emit(Event::Decision {
                what: "rollback_ids_missing".into(),
                reason: event.event_id.clone(),
            });
        }
        Ok(())
    }

    /// Managed dispatch (SPEC §23): admission before launch, heartbeats
    /// during, release and settlement after. A coordinator outage blocks
    /// the launch rather than making it unmanaged; a queue wait counts
    /// against the run's wall clock; refusals on budget, depth or the
    /// aggregate agent cap are budget exhaustion, and a run cancelled
    /// while queued or running ends cancelled with its evidence kept.
    ///
    /// `ManagedDispatch::write_lease` names the worktree this dispatch
    /// will write, when it writes one: the lease is taken after
    /// admission and before the process exists, and given back when the
    /// process has ended. A worktree somebody else is writing refuses
    /// the launch outright — two writers in one tree is the one thing
    /// scope checks cannot catch (SPEC §23: "scope checks alone are not
    /// filesystem isolation"). Readers — the reviewer, the planner —
    /// pass `None`.
    pub(crate) fn managed_launch(
        &mut self,
        dispatch: ManagedDispatch<'_>,
    ) -> Result<Launched, RunError> {
        let dispatch_id = dispatch.spec.dispatch_id.clone();
        self.events.emit(Event::DispatchStarted {
            dispatch: dispatch_id.clone(),
            agent_kind: dispatch.spec.agent,
            attempt: dispatch.attempt,
            model: dispatch.spec.model.clone(),
            effort: effort_str(dispatch.spec.effort.as_ref()),
        });
        let launched = self.launch_admitted(dispatch)?;
        self.events.emit(dispatch_ended(dispatch_id, &launched));
        Ok(launched)
    }

    pub(super) fn launch_admitted(
        &mut self,
        dispatch: ManagedDispatch<'_>,
    ) -> Result<Launched, RunError> {
        let ManagedDispatch {
            mut spec,
            depth,
            parent,
            reserve_micros,
            budget,
            write_lease,
            ..
        } = dispatch;
        // The dispatch is `launched` in the ledger BEFORE the process
        // exists (SPEC §12): a runner crash from here on leaves a live
        // dispatch for `resume` to reconcile, never a silent gap.
        self.config.ledger.attach_dispatch_process(
            &DispatchId::from_stored(spec.dispatch_id.clone()),
            None,
            Some(&self.config.session_id),
        )?;
        let Some(gate) = self.config.gate else {
            return Ok(match self.config.backend.launch(&spec) {
                Ok(result) => Ok(result),
                Err(e) => {
                    Err(self.block(Reason::BlockedPreflight, e.block_code(), e.to_string())?)
                }
            });
        };
        let request = DispatchRequest {
            disk: None,
            dispatch_id: spec.dispatch_id.clone(),
            run_id: self.run_id.as_str().to_string(),
            session_id: self.config.session_id.clone(),
            parent_dispatch: parent.map(str::to_string),
            depth,
            resource: ResourceClass::ModelWork,
            reserve_micros,
            source: DispatchSource::NativeRun,
            // A managed dispatch self-reports its parent through
            // `parent_dispatch` above; `caller_agent_id` exists only for
            // the hook path, which has a caller's agent id and no
            // dispatch id of its own to report.
            caller_agent_id: None,
            weight: 1,
        };
        // Time spent queued is charged to `admission_wait_seconds` and to
        // the total, never to the work budget (ADR-0010).
        let mut queued = self.budgets.wait(WaitKind::Admission);
        loop {
            match gate.admit(&request) {
                Err(e) => {
                    let (reason, code) = admission_block(&e);
                    return Ok(Err(self.block(
                        reason,
                        code,
                        format!("{e}; the request is preserved and nothing was launched"),
                    )?));
                }
                Ok(Decision::Granted) => break,
                Ok(Decision::AlreadyAdmitted) => {
                    // Somebody already holds this ID: launching would
                    // duplicate an agent. Stop and let resume reconcile.
                    return Ok(Err(self.finish(
                        Reason::DuplicateDispatch,
                        serde_json::json!({ "dispatch_id": spec.dispatch_id }),
                        Terminal::Interrupted {
                            detail: format!(
                                "dispatch {} was already admitted elsewhere; not launching a duplicate",
                                spec.dispatch_id
                            ),
                        },
                    )?));
                }
                Ok(decision @ (Decision::Queued { .. } | Decision::WaitingForDisk { .. })) => {
                    // Model work asks nothing of the disk, so the second
                    // form is not expected; it waits like a queue.
                    let position = match decision {
                        Decision::Queued { position } => position,
                        _ => 0,
                    };
                    if let Some(exhausted) = queued.tick() {
                        // Best effort: nothing was launched, and a
                        // request the coordinator still holds expires
                        // with the queue entry it never granted.
                        let _ = gate.withdraw(&spec.dispatch_id);
                        self.events.emit(Event::Decision {
                            what: "admission_queue_ended".into(),
                            reason: format!("still queued at position {position}"),
                        });
                        return Ok(Err(self.end_on_exhausted(exhausted, budget)?));
                    }
                    std::thread::sleep(ADMISSION_POLL);
                }
                Ok(Decision::Refused { code, detail }) => {
                    return Ok(Err(match code {
                        Refusal::RunCancelled => {
                            self.stop(budget, Observation::Cancelled(detail))?
                        }
                        Refusal::BudgetExceeded | Refusal::RunAgentCap | Refusal::DepthExceeded => {
                            self.stop(
                                budget,
                                Observation::LimitReached(Limit::Admission {
                                    code: code.as_str().to_string(),
                                    detail,
                                }),
                            )?
                        }
                        Refusal::UnknownRun | Refusal::WeightOutOfRange => self.block(
                            Reason::AdmissionRefused,
                            BlockCode::AdmissionRefused,
                            detail,
                        )?,
                        // C8: this dispatch ID already ran and settled.
                        // Launching it again duplicates an agent, so it
                        // ends exactly where `AlreadyAdmitted` does.
                        Refusal::AlreadyFinished => self.finish(
                            Reason::DuplicateDispatch,
                            serde_json::json!({ "dispatch_id": spec.dispatch_id }),
                            Terminal::Interrupted { detail },
                        )?,
                    }));
                }
            }
        }

        drop(queued);
        let lease_key = write_lease.map(|path| path.to_string_lossy().into_owned());
        if let Some(key) = lease_key.as_deref() {
            match gate.acquire_write(&spec.dispatch_id, key) {
                Ok(WriteLeaseOutcome::Taken) => {
                    self.own_write_leases.insert(spec.dispatch_id.clone());
                }
                Ok(WriteLeaseOutcome::HeldBy { holder }) => {
                    // Best effort: the seat is given back so the refusal
                    // does not hold one, and an unwithdrawn request
                    // expires on its own.
                    let _ = gate.withdraw(&spec.dispatch_id);
                    return Ok(Err(self.block(
                        Reason::AdmissionRefused,
                        BlockCode::AdmissionRefused,
                        format!(
                            "worktree {key} is being written by {holder}; a second writer is \
                             never launched into one tree (SPEC §23)"
                        ),
                    )?));
                }
                Err(e) => {
                    // Best effort, and to the same coordinator that just
                    // failed: the run is ending on that failure anyway.
                    let _ = gate.withdraw(&spec.dispatch_id);
                    let (reason, code) = admission_block(&e);
                    return Ok(Err(self.block(
                        reason,
                        code,
                        format!("{e}; the write lease could not be taken and nothing was launched"),
                    )?));
                }
            }
        }

        let cancel = Arc::new(AtomicBool::new(false));
        spec.cancel = Some(Arc::clone(&cancel));
        let stop = AtomicBool::new(false);
        let heartbeat_every = self.config.heartbeat_every;
        // Cancellation reaches a worker only on the heartbeat (SPEC §23).
        // A coordinator that stops answering therefore makes `relais
        // cancel` a no-op, silently, for as long as the worker runs —
        // so the errors are counted and the run ends on the record (R6).
        let heartbeat_lost: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
        let launched = std::thread::scope(|scope| {
            let dispatch_id = spec.dispatch_id.clone();
            let cancel = Arc::clone(&cancel);
            let stop = &stop;
            let heartbeat_lost = &heartbeat_lost;
            let events = &self.events;
            scope.spawn(move || {
                let mut last: Option<Instant> = None;
                let mut last_relay = Instant::now();
                let mut heartbeat_errors: u32 = 0;
                while !stop.load(Ordering::SeqCst) {
                    if last.is_none_or(|last| last.elapsed() >= heartbeat_every) {
                        last = Some(Instant::now());
                        match gate.heartbeat(&dispatch_id) {
                            Ok(status) => {
                                // One answer clears the count: a single
                                // dropped call is a blip, not an outage.
                                heartbeat_errors = 0;
                                if status.cancelled {
                                    cancel.store(true, Ordering::SeqCst);
                                    // The seat and the reservation go
                                    // now, not at lease grace (C5).
                                    // Best effort: the worker is already
                                    // stopping and the coordinator reaps
                                    // the seat on its own lease grace.
                                    let _ = gate.acknowledge_cancel(&dispatch_id);
                                }
                            }
                            Err(e) => {
                                heartbeat_errors += 1;
                                if heartbeat_errors >= HEARTBEAT_FAILURES_ALLOWED {
                                    record_lost_seat(
                                        heartbeat_lost,
                                        format!(
                                            "{HEARTBEAT_FAILURES_ALLOWED} consecutive heartbeats \
                                             for dispatch {dispatch_id} went unanswered ({e})"
                                        ),
                                    );
                                }
                            }
                        }
                    }
                    // Usage the worker reports through `relais native
                    // usage` reaches the sequenced stream while it runs.
                    if last_relay.elapsed() >= Duration::from_millis(200) {
                        last_relay = Instant::now();
                        events.relay_usage();
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            });
            let result = self.config.backend.launch(&spec);
            stop.store(true, Ordering::SeqCst);
            result
        });
        // The last report may have landed after the final tick.
        self.events.relay_usage();

        // The process has ended: whatever it wrote is written. The lease
        // goes back before the seat, so verification never waits on a
        // writer that is already gone (SPEC §23).
        if let Some(key) = lease_key.as_deref() {
            self.release_write_lease(gate, &spec.dispatch_id, key)?;
        }
        let result = match launched {
            Ok(result) => result,
            Err(e) => {
                // Best effort: the launch never produced a process, and
                // the coordinator reaps an unbound seat and its
                // reservation on its own lease grace.
                let _ = gate.release(&spec.dispatch_id);
                let _ = gate.settle(&spec.dispatch_id, None);
                return Ok(Err(self.block(
                    Reason::BlockedPreflight,
                    e.block_code(),
                    e.to_string(),
                )?));
            }
        };
        // Bind, free the seat, settle the reservation: unknown usage
        // settles as unknown, never zero (SPEC §23). Failures here are
        // the coordinator's to reconcile; the work already happened and
        // an agent ID it cannot attach costs nothing.
        let _ = gate.bind(&spec.dispatch_id, result.session_id.as_deref(), None);
        let _ = gate.release(&spec.dispatch_id);
        let spent = result.usage.cost.micros().map(MicroUsd::to_micros);
        let _ = gate.settle(&spec.dispatch_id, spent);
        // The worker ran unheard: whatever it produced is on disk and in
        // the ledger, but nothing could have stopped it, so the run ends
        // interrupted rather than judging a candidate it could not
        // supervise (R6).
        if let Some(detail) = heartbeat_lost
            .into_inner()
            .unwrap_or_else(|e| e.into_inner())
        {
            return Ok(Err(self.finish(
                Reason::CoordinatorUnreachable,
                serde_json::json!({
                    "dispatch_id": spec.dispatch_id,
                    "detail": detail,
                }),
                Terminal::Interrupted {
                    detail: format!(
                        "{detail}; a cancellation could not have reached the worker, so the run \
                         is not supervised and its evidence is preserved"
                    ),
                },
            )?));
        }
        Ok(Ok(result))
    }

    /// Give a worktree's write lease back when the process that held it
    /// has ended.
    ///
    /// A discarded failure here is not cosmetic: leases have no expiry
    /// (`admission`), so this run's own dispatch stays on record as the
    /// writer and `wait_for_writers` waits for it until the run's wall
    /// clock runs out — an acceptable candidate ending `interrupted`
    /// because of one dropped socket call (R3). One retry, then the
    /// failure goes on the record and the lease is remembered as this
    /// run's own so verification does not wait on itself.
    pub(super) fn release_write_lease(
        &mut self,
        gate: &(dyn Gate + Sync),
        dispatch_id: &str,
        key: &str,
    ) -> Result<(), RunError> {
        let mut failure = None;
        for _ in 0..RELEASE_WRITE_TRIES {
            match gate.release_write(dispatch_id, key) {
                // Released, or somebody else holds the tree (or nobody
                // does): our lease is not there to give back either
                // way, and a real holder is exactly what verification
                // must wait for.
                Ok(ReleaseWriteOutcome::Released | ReleaseWriteOutcome::NotTheHolder) => {
                    self.own_write_leases.remove(dispatch_id);
                    return Ok(());
                }
                Err(e) => failure = Some(e),
            }
        }
        let detail = failure.map_or_else(
            || "the write lease could not be released".to_string(),
            |e| e.to_string(),
        );
        self.transition(
            self.state,
            Reason::WriteLeaseNotReleased,
            serde_json::json!({
                "worktree": key,
                "dispatch_id": dispatch_id,
                "error": detail,
                "detail": "the lease is this run's own and its process has ended, so \
                           verification treats the tree as still",
            }),
        )
    }
}

/// How a failed admission call is reported. An unreachable coordinator
/// is an outage — the request is preserved and nothing was launched — and
/// anything else is a decision the coordinator made and answered with.
/// Reporting a refusal as `admission_unavailable` sent the operator
/// looking for a dead daemon that was answering perfectly well (A12).
pub(super) fn admission_block(error: &GateError) -> (Reason, BlockCode) {
    if error.unavailable() {
        (
            Reason::AdmissionUnavailable,
            BlockCode::AdmissionUnavailable,
        )
    } else {
        (Reason::AdmissionRefused, BlockCode::AdmissionRefused)
    }
}

/// Record the first reason this dispatch lost its seat. The first is the
/// one that explains the rest, and a poisoned slot is recovered rather
/// than turning a lost seat into a second panic.
pub(super) fn record_lost_seat(slot: &std::sync::Mutex<Option<String>>, detail: String) {
    let mut slot = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    slot.get_or_insert(detail);
}

/// How many times a finished dispatch asks for its write lease to be
/// taken back before the failure goes on the record (R3).
const RELEASE_WRITE_TRIES: u32 = 2;
