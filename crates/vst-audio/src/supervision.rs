//! Control-thread recovery. Prepared resources cross to audio; reclamation returns.
use crate::{
    Command, ControlEvent, HostEvent, PluginControl, RequestId, Started,
    api::{Launch, Lifecycle, RecoveryPolicy, Shared},
    audio::PreparedAudio,
    watch::ControlWatch,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::Ordering, mpsc},
    thread,
    time::{Duration, Instant},
};
use vst_audio_protocol::{Action, Message};
fn emit(tx: &mpsc::SyncSender<HostEvent>, shared: &Shared, event: HostEvent) {
    if tx.try_send(event).is_err() {
        shared.overflow.store(true, Ordering::Release);
    }
}
pub(crate) struct RecoveryConfig {
    pub launch: Launch,
    pub geometry: vst_audio_ring::Geometry,
    pub setup: vst_audio_protocol::ProcessingSetup,
}

struct OrphanedWatch {
    watch: ControlWatch,
    base_ms: u64,
    taken_at: Instant,
}

impl OrphanedWatch {
    fn new(watch: ControlWatch, base_ms: u64, taken_at: Instant) -> Self {
        Self {
            watch,
            base_ms,
            taken_at,
        }
    }

    fn now_ms(&self) -> u64 {
        crate::watch::elapsed_ms(self.base_ms, self.taken_at)
    }
}

fn prepare_audio_handover(
    started: Started,
    expected_geometry: vst_audio_ring::Geometry,
    expected_setup: &vst_audio_protocol::ProcessingSetup,
    shared: &Shared,
    updates: &mut rtrb::Producer<PreparedAudio>,
    events: &mpsc::SyncSender<HostEvent>,
    latency: &mut u32,
) -> Result<(PluginControl, u64), String> {
    if started.geometry != expected_geometry
        || started.setup != *expected_setup
        || updates.is_full()
    {
        return Err("recovery requires a compatible layout and an available audio boundary".into());
    }
    let generation = shared
        .generation
        .load(Ordering::Acquire)
        .checked_add(1)
        .ok_or_else(|| "recovery generation exhausted".to_string())?;
    let latency_frames = started.latency_frames;
    let (prepared, control) = PreparedAudio::from_started(started, generation)
        .map_err(|error| format!("recovery ring lane is invalid: {error:?}"))?;
    updates
        .push(prepared)
        .map_err(|_| "recovery audio boundary became unavailable".to_string())?;
    shared.generation.store(generation, Ordering::Release);
    shared.set_lifecycle(Lifecycle::Replacing);
    shared.worker_pid.store(control.pid(), Ordering::Release);
    // The application sized its delays for the old worker's plugin latency.
    if latency_frames != *latency {
        *latency = latency_frames;
        emit(events, shared, HostEvent::LatencyChanged(latency_frames));
    }
    Ok((control, generation))
}

pub(crate) fn supervise(
    initial: PluginControl,
    requests: mpsc::Receiver<(RequestId, Command)>,
    events: mpsc::SyncSender<HostEvent>,
    shared: Arc<Shared>,
    mut updates: rtrb::Producer<PreparedAudio>,
    mut retirements: rtrb::Consumer<PreparedAudio>,
    recovery_config: RecoveryConfig,
) {
    let launch = recovery_config.launch;
    let watch_policy = launch.configuration.recovery == RecoveryPolicy::Watch;
    let mut latency = initial.latency();
    let mut control = Some(initial);
    let mut orphaned_watch: Option<OrphanedWatch> = None;
    let mut pending = BTreeMap::new();
    let mut stopping = false;
    let mut failure_reported = false;
    let mut recovery: Option<(Instant, bool, Option<RequestId>)> = None;
    let mut attempts = 0u8;
    let mut prepared_generation = None;
    let mut restoration_pending = false;
    loop {
        if restoration_pending
            && shared.processed_generation.load(Ordering::Acquire)
                == shared.generation.load(Ordering::Acquire)
        {
            restoration_pending = false;
            if watch_policy {
                let history = {
                    let mut history = shared.history.lock().unwrap_or_else(|e| e.into_inner());
                    history.restore_succeeded();
                    history.clone()
                };
                if let Some(control) = &mut control {
                    let now_ms = control.elapsed_ms();
                    let mut watch = control.take_watch();
                    watch.states = history;
                    control.restore_watch(watch, now_ms);
                }
            }
        }
        while let Ok(retired) = retirements.pop() {
            drop(retired);
        }
        if let Some(generation) = prepared_generation
            && shared.applied_generation.load(Ordering::Acquire) == generation
        {
            shared.set_lifecycle(Lifecycle::Running);
            emit(&events, &shared, HostEvent::Recovered { generation });
            prepared_generation = None;
        }
        if shared.stop.load(Ordering::Acquire) && !stopping {
            let already_exited = control
                .as_ref()
                .and_then(|control| control.exit_code().ok().flatten())
                .is_some();
            if already_exited {
                drop(control.take());
            } else if let Some(control) = &mut control {
                let _ = control.shutdown();
            }
            stopping = true;
            recovery = None;
            orphaned_watch = None;
        }
        if stopping && control.is_none() {
            shared.set_lifecycle(Lifecycle::Stopped);
            emit(&events, &shared, HostEvent::Stopped);
            return;
        }
        for _ in 0..8 {
            let Ok((id, command)) = requests.try_recv() else {
                break;
            };
            if let Command::Recover { use_previous } = command {
                let watch_pending = watch_policy && shared.lifecycle() == Lifecycle::Recovering;
                if stopping
                    || recovery.is_some()
                    || orphaned_watch.is_some()
                    || prepared_generation.is_some()
                    || watch_pending
                {
                    emit(
                        &events,
                        &shared,
                        HostEvent::RequestFailed {
                            request: id,
                            reason: "recovery already pending or stopping".into(),
                        },
                    );
                    continue;
                }
                recovery = Some((Instant::now(), use_previous, Some(id)));
                shared.set_lifecycle(Lifecycle::Recovering);
                break;
            }
            if matches!(command, Command::Shutdown) {
                stopping = true;
                recovery = None;
                orphaned_watch = None;
                let already_exited = control
                    .as_ref()
                    .and_then(|control| control.exit_code().ok().flatten())
                    .is_some();
                if already_exited {
                    drop(control.take());
                } else if let Some(control) = &mut control {
                    let _ = control.shutdown();
                }
                emit(&events, &shared, HostEvent::Completed { request: id });
                continue;
            }
            if watch_policy && shared.lifecycle() == Lifecycle::Recovering {
                emit(
                    &events,
                    &shared,
                    HostEvent::RequestFailed {
                        request: id,
                        reason: "worker is not running".into(),
                    },
                );
                continue;
            }
            let Some(control) = &mut control else {
                emit(
                    &events,
                    &shared,
                    HostEvent::RequestFailed {
                        request: id,
                        reason: "worker is not running".into(),
                    },
                );
                continue;
            };
            if pending.len() >= 64 {
                emit(
                    &events,
                    &shared,
                    HostEvent::RequestFailed {
                        request: id,
                        reason: "too many pending requests".into(),
                    },
                );
                continue;
            }
            let result = match command {
                Command::Editor(action) => control.editor_action(action),
                Command::OpenEditor => control.open_editor(),
                Command::OpenEditorIn(parent) => control.open_editor_in(parent),
                Command::OpenEditorWith { parent, scaling } => {
                    control.open_editor_with(parent, scaling)
                }
                Command::CloseEditor => control.request_close_editor(),
                Command::ListParameters { start, count } => control.parameters(start, count),
                Command::SetParameter { id, value } => control.set_parameter(id, value),
                Command::SaveState => control.get_state(),
                Command::RestoreState(bytes) => {
                    // A later explicit edit is independent of startup restoration.
                    restoration_pending = false;
                    control.set_state(&bytes)
                }
                Command::Recover { .. } | Command::Shutdown => unreachable!(),
            };
            match result {
                Ok(wire) => {
                    pending.insert(wire, id);
                }
                Err(error) => emit(
                    &events,
                    &shared,
                    HostEvent::RequestFailed {
                        request: id,
                        reason: error.to_string(),
                    },
                ),
            }
        }
        if let Some((when, previous, request)) = recovery
            && Instant::now() >= when
        {
            recovery = None;
            // Dropping the old process kills it; its audio mapping remains owned
            // until the callback hands it back through the retirement queue.
            drop(control.take());
            for (_, request) in std::mem::take(&mut pending) {
                emit(
                    &events,
                    &shared,
                    HostEvent::RequestFailed {
                        request,
                        reason: "worker replaced".into(),
                    },
                );
            }
            let state = {
                let history = shared.history.lock().unwrap_or_else(|e| e.into_inner());
                if previous {
                    history.previous().or(history.current())
                } else {
                    history.current()
                }
                .map(<[u8]>::to_vec)
            };
            let result = launch.start(state.as_deref(), shared.stop.clone());
            match result {
                Ok(started) => match prepare_audio_handover(
                    started,
                    recovery_config.geometry,
                    &recovery_config.setup,
                    &shared,
                    &mut updates,
                    &events,
                    &mut latency,
                ) {
                    Ok((mut new_control, generation)) => {
                        if watch_policy && request.is_some() {
                            let now_ms = new_control.elapsed_ms();
                            let mut watch = ControlWatch::new(now_ms);
                            watch.states = shared
                                .history
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .clone();
                            new_control.restore_watch(watch, now_ms);
                        }
                        control = Some(new_control);
                        restoration_pending = true;
                        prepared_generation = Some(generation);
                        failure_reported = false;
                        if let Some(request) = request {
                            emit(&events, &shared, HostEvent::Completed { request });
                        }
                    }
                    Err(reason) => {
                        shared.set_lifecycle(Lifecycle::Failed);
                        emit(
                            &events,
                            &shared,
                            if let Some(request) = request {
                                HostEvent::RequestFailed { request, reason }
                            } else {
                                HostEvent::Failed { reason }
                            },
                        );
                    }
                },
                Err(error) => {
                    shared.set_lifecycle(Lifecycle::Failed);
                    if let Some(request) = request {
                        emit(
                            &events,
                            &shared,
                            HostEvent::RequestFailed {
                                request,
                                reason: error.to_string(),
                            },
                        );
                    } else {
                        emit(
                            &events,
                            &shared,
                            HostEvent::Failed {
                                reason: format!("recovery failed: {error}"),
                            },
                        );
                        if let RecoveryPolicy::Automatic {
                            attempts: limit,
                            backoff_ms,
                        } = launch.configuration.recovery
                            && attempts < limit
                        {
                            attempts += 1;
                            recovery = Some((
                                Instant::now()
                                    + Duration::from_millis(
                                        u64::from(backoff_ms) * u64::from(attempts),
                                    ),
                                true,
                                None,
                            ));
                        }
                    }
                }
            }
        }
        let mut exited = None;
        let mut respawn_requested = false;
        let mut disabled = false;
        if let Some(control) = &mut control {
            for event in control.poll(control.elapsed_ms()) {
                match event {
                    ControlEvent::Message(frame) => {
                        let request = pending.remove(&frame.request);
                        match frame.message {
                            Message::State { blob } => {
                                shared
                                    .history
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .save(blob.clone());
                                emit(
                                    &events,
                                    &shared,
                                    HostEvent::State {
                                        request,
                                        bytes: blob,
                                    },
                                );
                            }
                            Message::Ack => {
                                if let Some(request) = request {
                                    emit(&events, &shared, HostEvent::Completed { request });
                                }
                            }
                            Message::Parameters { total, entries } => {
                                if let Some(request) = request {
                                    emit(
                                        &events,
                                        &shared,
                                        HostEvent::Parameters {
                                            request,
                                            total,
                                            entries,
                                        },
                                    );
                                }
                            }
                            Message::RequestError { text, .. } => {
                                if let Some(request) = request {
                                    emit(
                                        &events,
                                        &shared,
                                        HostEvent::RequestFailed {
                                            request,
                                            reason: text,
                                        },
                                    );
                                }
                            }
                            Message::EditorStatus(status) => {
                                if let Some(request) = request {
                                    emit(
                                        &events,
                                        &shared,
                                        HostEvent::EditorStatus { request, status },
                                    );
                                }
                            }
                            Message::EditorImage(image) => {
                                if let Some(request) = request {
                                    emit(
                                        &events,
                                        &shared,
                                        HostEvent::EditorImage { request, image },
                                    );
                                }
                            }
                            Message::EditorResized { width, height } => {
                                emit(&events, &shared, HostEvent::EditorResized { width, height })
                            }
                            Message::EditorClosed => {
                                emit(&events, &shared, HostEvent::EditorClosed)
                            }
                            Message::ParameterEdit { id, phase, value } => emit(
                                &events,
                                &shared,
                                HostEvent::ParameterEdited { id, phase, value },
                            ),
                            Message::ParamsChanged => {
                                emit(&events, &shared, HostEvent::ParametersChanged)
                            }
                            Message::LatencyChanged { frames } => {
                                latency = frames;
                                emit(&events, &shared, HostEvent::LatencyChanged(frames))
                            }
                            Message::Fault { text, .. } => {
                                if let Some(request) = request {
                                    emit(
                                        &events,
                                        &shared,
                                        HostEvent::RequestFailed {
                                            request,
                                            reason: text.clone(),
                                        },
                                    );
                                }
                                shared.set_lifecycle(Lifecycle::Failed);
                                failure_reported = true;
                                emit(&events, &shared, HostEvent::Failed { reason: text });
                            }
                            _ => {}
                        }
                    }
                    ControlEvent::Exited(code) => {
                        exited = Some(code);
                    }
                    ControlEvent::NeedsRespawn if watch_policy => respawn_requested = true,
                    ControlEvent::Action(action @ Action::Disable { .. }) => {
                        disabled = true;
                        shared.set_lifecycle(Lifecycle::Failed);
                        if !stopping && !failure_reported {
                            failure_reported = true;
                            let Action::Disable { reason } = action else {
                                unreachable!()
                            };
                            emit(
                                &events,
                                &shared,
                                HostEvent::Failed {
                                    reason: format!("worker disabled ({reason:?})"),
                                },
                            );
                        }
                        if watch_policy && !stopping {
                            emit(&events, &shared, HostEvent::RecoveryRequired);
                        }
                    }
                    ControlEvent::Action(action @ Action::Kill { .. }) => {
                        shared.set_lifecycle(if watch_policy {
                            Lifecycle::Recovering
                        } else {
                            Lifecycle::Failed
                        });
                        if !stopping && !failure_reported {
                            failure_reported = true;
                            let Action::Kill { reason } = action else {
                                unreachable!()
                            };
                            emit(
                                &events,
                                &shared,
                                HostEvent::Failed {
                                    reason: format!("worker deadline exceeded ({reason:?})"),
                                },
                            );
                        }
                    }
                    ControlEvent::Action(Action::EnterFallback) if watch_policy => {
                        shared.set_lifecycle(Lifecycle::Recovering);
                        if !stopping && !failure_reported {
                            failure_reported = true;
                            emit(
                                &events,
                                &shared,
                                HostEvent::Failed {
                                    reason: "worker entered fallback".into(),
                                },
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
        if disabled && watch_policy {
            drop(control.take());
        }
        if respawn_requested && !stopping {
            for (_, request) in std::mem::take(&mut pending) {
                emit(
                    &events,
                    &shared,
                    HostEvent::RequestFailed {
                        request,
                        reason: "worker replaced".into(),
                    },
                );
            }
            let mut old_control = control.take().expect("respawn has a control");
            let base_ms = old_control.elapsed_ms();
            let taken_at = Instant::now();
            let watch = old_control.take_watch();
            orphaned_watch = Some(OrphanedWatch::new(watch, base_ms, taken_at));
            drop(old_control);
        }
        if let Some(code) = exited {
            if !watch_policy {
                drop(control.take());
            }
            prepared_generation = None;
            for (_, request) in std::mem::take(&mut pending) {
                emit(
                    &events,
                    &shared,
                    HostEvent::RequestFailed {
                        request,
                        reason: "worker exited".into(),
                    },
                );
            }
            if stopping {
                shared.set_lifecycle(Lifecycle::Stopped);
                emit(&events, &shared, HostEvent::Stopped);
                return;
            }
            shared.set_lifecycle(if watch_policy && !disabled {
                Lifecycle::Recovering
            } else {
                Lifecycle::Failed
            });
            if !failure_reported {
                failure_reported = true;
                emit(
                    &events,
                    &shared,
                    HostEvent::Failed {
                        reason: format!("worker exited with status {code}"),
                    },
                );
            }
            if !watch_policy && !disabled {
                emit(&events, &shared, HostEvent::RecoveryRequired);
            }
            if let RecoveryPolicy::Automatic {
                attempts: limit,
                backoff_ms,
            } = launch.configuration.recovery
                && attempts < limit
            {
                attempts += 1;
                recovery = Some((
                    Instant::now()
                        + Duration::from_millis(u64::from(backoff_ms) * u64::from(attempts)),
                    restoration_pending,
                    None,
                ));
            }
        }
        if watch_policy && !stopping && respawn_requested {
            let mut orphan = orphaned_watch.take().expect("respawn preserved its watch");
            let state = orphan.watch.states.respawn_state();
            *shared.history.lock().unwrap_or_else(|e| e.into_inner()) = orphan.watch.states.clone();
            match launch.start(state.as_deref(), shared.stop.clone()) {
                Ok(started) => match prepare_audio_handover(
                    started,
                    recovery_config.geometry,
                    &recovery_config.setup,
                    &shared,
                    &mut updates,
                    &events,
                    &mut latency,
                ) {
                    Ok((mut new_control, generation)) => {
                        let now_ms = orphan.now_ms();
                        new_control.restore_watch(orphan.watch, now_ms);
                        *shared.history.lock().unwrap_or_else(|e| e.into_inner()) =
                            new_control.state_history();
                        control = Some(new_control);
                        restoration_pending = true;
                        prepared_generation = Some(generation);
                        failure_reported = false;
                    }
                    Err(reason) => {
                        shared.set_lifecycle(Lifecycle::Failed);
                        emit(&events, &shared, HostEvent::Failed { reason });
                        emit(&events, &shared, HostEvent::RecoveryRequired);
                    }
                },
                Err(error) => {
                    let now_ms = orphan.now_ms();
                    let actions = orphan.watch.poll(now_ms, 0, Some(1));
                    shared.set_lifecycle(Lifecycle::Recovering);
                    failure_reported = false;
                    let disabled = actions
                        .iter()
                        .any(|action| matches!(action, Action::Disable { .. }));
                    emit(
                        &events,
                        &shared,
                        HostEvent::Failed {
                            reason: if disabled {
                                "worker disabled (ThreeFailuresInTenMinutes)".into()
                            } else {
                                format!("recovery failed: {error}")
                            },
                        },
                    );
                    if disabled {
                        shared.set_lifecycle(Lifecycle::Failed);
                        emit(&events, &shared, HostEvent::RecoveryRequired);
                    } else {
                        orphaned_watch = Some(orphan);
                    }
                }
            }
        } else if watch_policy && !stopping && control.is_none() {
            let mut spawn = false;
            let mut disable = None;
            if let Some(orphan) = &mut orphaned_watch {
                for action in orphan.watch.poll(orphan.now_ms(), 0, None) {
                    match action {
                        Action::Spawn => spawn = true,
                        Action::Disable { reason } => disable = Some(reason),
                        _ => {}
                    }
                }
            }
            if spawn {
                let orphan = orphaned_watch.as_mut().expect("orphaned watch exists");
                let state = orphan.watch.states.respawn_state();
                *shared.history.lock().unwrap_or_else(|e| e.into_inner()) =
                    orphan.watch.states.clone();
                match launch.start(state.as_deref(), shared.stop.clone()) {
                    Ok(started) => match prepare_audio_handover(
                        started,
                        recovery_config.geometry,
                        &recovery_config.setup,
                        &shared,
                        &mut updates,
                        &events,
                        &mut latency,
                    ) {
                        Ok((mut new_control, generation)) => {
                            let orphan = orphaned_watch.take().expect("orphaned watch exists");
                            let now_ms = orphan.now_ms();
                            new_control.restore_watch(orphan.watch, now_ms);
                            *shared.history.lock().unwrap_or_else(|e| e.into_inner()) =
                                new_control.state_history();
                            control = Some(new_control);
                            restoration_pending = true;
                            prepared_generation = Some(generation);
                            failure_reported = false;
                        }
                        Err(reason) => {
                            orphaned_watch = None;
                            shared.set_lifecycle(Lifecycle::Failed);
                            emit(&events, &shared, HostEvent::Failed { reason });
                            emit(&events, &shared, HostEvent::RecoveryRequired);
                        }
                    },
                    Err(error) => {
                        let orphan = orphaned_watch.as_mut().expect("orphaned watch exists");
                        let actions = orphan.watch.poll(orphan.now_ms(), 0, Some(1));
                        if actions
                            .iter()
                            .any(|action| matches!(action, Action::Disable { .. }))
                        {
                            disable =
                                Some(vst_audio_protocol::DisableReason::ThreeFailuresInTenMinutes);
                        } else {
                            emit(
                                &events,
                                &shared,
                                HostEvent::Failed {
                                    reason: format!("recovery failed: {error}"),
                                },
                            );
                        }
                    }
                }
            }
            if let Some(reason) = disable {
                orphaned_watch = None;
                shared.set_lifecycle(Lifecycle::Failed);
                emit(
                    &events,
                    &shared,
                    HostEvent::Failed {
                        reason: format!("worker disabled ({reason:?})"),
                    },
                );
                emit(&events, &shared, HostEvent::RecoveryRequired);
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
}
