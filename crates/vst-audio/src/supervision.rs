//! Control-thread recovery. Prepared resources cross to audio; reclamation returns.
use crate::{
    Command, ControlEvent, HostEvent, PluginControl, RequestId, Started,
    api::{Launch, RecoveryPolicy, Shared},
    audio::PreparedAudio,
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
    let mut control = Some(initial);
    let mut pending = BTreeMap::new();
    let mut stopping = false;
    let mut failure_reported = false;
    let mut recovery: Option<(Instant, bool, Option<RequestId>)> = None;
    let mut attempts = 0u8;
    let mut prepared_generation = None;
    let mut restoration_pending = false;
    loop {
        if shared.processed_generation.load(Ordering::Acquire)
            == shared.generation.load(Ordering::Acquire)
        {
            restoration_pending = false;
        }
        while let Ok(retired) = retirements.pop() {
            drop(retired);
        }
        if let Some(generation) = prepared_generation
            && shared.applied_generation.load(Ordering::Acquire) == generation
        {
            shared.status.store(0, Ordering::Release);
            emit(&events, &shared, HostEvent::Recovered { generation });
            prepared_generation = None;
        }
        if shared.stop.load(Ordering::Acquire) && !stopping {
            if let Some(control) = &mut control {
                let _ = control.shutdown();
            }
            stopping = true;
            recovery = None;
        }
        if stopping && control.is_none() {
            shared.status.store(2, Ordering::Release);
            emit(&events, &shared, HostEvent::Stopped);
            return;
        }
        for _ in 0..8 {
            let Ok((id, command)) = requests.try_recv() else {
                break;
            };
            if let Command::Recover { use_previous } = command {
                if stopping || recovery.is_some() || prepared_generation.is_some() {
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
                shared.status.store(4, Ordering::Release);
                break;
            }
            if matches!(command, Command::Shutdown) {
                stopping = true;
                recovery = None;
                if let Some(control) = &mut control {
                    let _ = control.shutdown();
                }
                emit(&events, &shared, HostEvent::Completed { request: id });
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
                Ok(started) => {
                    let valid = started.geometry == recovery_config.geometry
                        && started.setup == recovery_config.setup;
                    if !valid || updates.is_full() {
                        shared.status.store(1, Ordering::Release);
                        let reason =
                            "recovery requires a compatible layout and an available audio boundary"
                                .to_string();
                        emit(
                            &events,
                            &shared,
                            if let Some(request) = request {
                                HostEvent::RequestFailed { request, reason }
                            } else {
                                HostEvent::Failed { reason }
                            },
                        );
                        drop(started);
                        continue;
                    }
                    let Some(generation) = shared.generation.load(Ordering::Acquire).checked_add(1)
                    else {
                        shared.status.store(1, Ordering::Release);
                        return;
                    };
                    let Started {
                        control: new_control,
                        region,
                        work,
                        publisher,
                        geometry,
                        latency_frames,
                        setup,
                        capabilities,
                        ..
                    } = started;
                    let prepared = PreparedAudio {
                        region,
                        work,
                        publisher,
                        geometry,
                        latency: latency_frames,
                        setup,
                        capabilities,
                        generation,
                    };
                    shared.generation.store(generation, Ordering::Release);
                    shared.status.store(3, Ordering::Release);
                    if updates.push(prepared).is_err() {
                        shared.status.store(1, Ordering::Release);
                        drop(new_control);
                        continue;
                    }
                    control = Some(new_control);
                    restoration_pending = true;
                    prepared_generation = Some(generation);
                    failure_reported = false;
                    if let Some(request) = request {
                        emit(&events, &shared, HostEvent::Completed { request });
                    }
                }
                Err(error) => {
                    shared.status.store(1, Ordering::Release);
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
                            Message::EditorClosed => {
                                emit(&events, &shared, HostEvent::EditorClosed)
                            }
                            Message::ParamsChanged => {
                                emit(&events, &shared, HostEvent::ParametersChanged)
                            }
                            Message::LatencyChanged { frames } => {
                                emit(&events, &shared, HostEvent::LatencyChanged(frames))
                            }
                            Message::Fault { text, .. } => {
                                shared.status.store(1, Ordering::Release);
                                failure_reported = true;
                                emit(&events, &shared, HostEvent::Failed { reason: text });
                            }
                            _ => {}
                        }
                    }
                    ControlEvent::Exited(code) => {
                        exited = Some(code);
                    }
                    ControlEvent::Action(Action::Kill { .. } | Action::Disable { .. }) => {
                        shared.status.store(1, Ordering::Release);
                        if !stopping && !failure_reported {
                            failure_reported = true;
                            emit(
                                &events,
                                &shared,
                                HostEvent::Failed {
                                    reason: "worker deadline exceeded".into(),
                                },
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Some(code) = exited {
            drop(control.take());
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
                shared.status.store(2, Ordering::Release);
                emit(&events, &shared, HostEvent::Stopped);
                return;
            }
            shared.status.store(1, Ordering::Release);
            if !failure_reported {
                emit(
                    &events,
                    &shared,
                    HostEvent::Failed {
                        reason: format!("worker exited with status {code}"),
                    },
                );
            }
            emit(&events, &shared, HostEvent::RecoveryRequired);
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
        thread::sleep(Duration::from_millis(5));
    }
}
