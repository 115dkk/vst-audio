#![forbid(unsafe_code)]

use std::io::Cursor;

use vst_audio_protocol::{
    Action, Bus, BusInfo, BusKind, ClassInfo, CodecError, DecodedFrame, DisableReason, EditPhase,
    Frame, HostWatch, KillReason, MAX_STRING_BYTES, Message, PluginFormat, Received, RequestKind,
    StateHistory, WatchState, decode_frame, encode_frame, read_frame,
};

#[test]
fn editor_platforms_round_trip_and_reject_unknown_tags() {
    use vst_audio_protocol::{EditorPlatform, EditorScaling, EditorStatus};
    for platform in [
        EditorPlatform::Win32,
        EditorPlatform::Cocoa,
        EditorPlatform::X11,
        EditorPlatform::Wayland,
    ] {
        let message = Message::EditorStatus(EditorStatus {
            platform,
            scaling: EditorScaling::Plugin,
            visible: true,
            embedded: false,
            width: 320,
            height: 200,
            scale: 1.0,
            resize_supported: true,
            scale_supported: false,
        });
        round_trip(message.clone());
        let mut bytes = encode_frame(&Frame {
            flags: 0,
            request: 1,
            message,
        })
        .unwrap();
        // The platform tag precedes 16 bytes of fields and the scaling byte.
        let offset = bytes.len() - 18;
        bytes[offset] = 4;
        assert!(matches!(
            decode_frame(&bytes),
            Err(CodecError::InvalidValue)
        ));
    }
}

fn round_trip(message: Message) {
    let frame = Frame {
        flags: 7,
        request: 42,
        message,
    };
    let encoded = encode_frame(&frame).unwrap();
    let declared = u32::from_le_bytes(encoded[0..4].try_into().unwrap()) as usize;
    assert_eq!(declared, encoded.len() - 4);
    assert_eq!(
        decode_frame(&encoded).unwrap(),
        DecodedFrame::Known(frame.clone())
    );
    assert_eq!(
        read_frame(&mut Cursor::new(encoded)).unwrap(),
        Some(DecodedFrame::Known(frame))
    );
}

#[test]
fn codec_round_trips_every_message() {
    let id = [9; 16];
    let buses = vec![BusInfo {
        layout: Bus {
            channels: 6,
            arrangement: 0x3f,
        },
        kind: BusKind::Aux,
        name: "Sidechain".into(),
        default_active: false,
    }];
    for message in [
        Message::hello(),
        Message::Sandboxed {
            mechanism: "landlock-abi-7+seccomp".into(),
        },
        Message::Load {
            path: "plugin.vst3".into(),
            format: PluginFormat::Vst3,
            class_id: Some(id),
        },
        Message::Configure {
            rate: 48_000,
            max_frames: 480,
            in_channels: 2,
            out_channels: 6,
        },
        Message::Activate,
        Message::SetState {
            blob: vec![1, 2, 3],
        },
        Message::GetState,
        Message::OpenEditor {
            scaling: vst_audio_protocol::EditorScaling::Auto,
        },
        Message::OpenEditorIn {
            parent: vst_audio_protocol::ParentWindow::Win32 { hwnd: u64::MAX },
            scaling: vst_audio_protocol::EditorScaling::Stretch,
        },
        Message::OpenEditorIn {
            parent: vst_audio_protocol::ParentWindow::X11 { window: 0x12345678 },
            scaling: vst_audio_protocol::EditorScaling::Off,
        },
        Message::EditorStatus(vst_audio_protocol::EditorStatus {
            platform: vst_audio_protocol::EditorPlatform::Wayland,
            scaling: vst_audio_protocol::EditorScaling::Plugin,
            visible: true,
            embedded: true,
            width: 640,
            height: 480,
            scale: 1.5,
            resize_supported: true,
            scale_supported: false,
        }),
        Message::CloseEditor,
        Message::Shutdown,
        Message::Scan {
            path: "plugin.dll".into(),
        },
        Message::Loaded {
            name: "Upmixer".into(),
            vendor: "Vendor".into(),
            latency_frames: 32,
            inputs: buses.clone(),
            outputs: buses.clone(),
        },
        Message::Classes {
            entries: vec![ClassInfo {
                id,
                name: "Class".into(),
                vendor: "Vendor".into(),
                category: "Fx".into(),
            }],
        },
        Message::LatencyChanged { frames: 64 },
        Message::State {
            blob: vec![4, 5, 6],
        },
        Message::ParamsChanged,
        Message::Fault {
            code: 9,
            text: "failed".into(),
        },
        Message::Ack,
        Message::EditorClosed,
        Message::EditorResized {
            width: 500,
            height: 280,
        },
        Message::ParameterEdit {
            id: 42,
            phase: EditPhase::Perform,
            value: 0.5,
        },
        Message::Scan {
            path: "C:\\".to_owned() + &"a".repeat(MAX_STRING_BYTES - 3),
        },
    ] {
        round_trip(message);
    }
}

#[test]
fn parameter_edit_rejects_invalid_payloads() {
    let valid = encode_frame(&Frame {
        flags: 0,
        request: 0,
        message: Message::ParameterEdit {
            id: 7,
            phase: EditPhase::Perform,
            value: 0.5,
        },
    })
    .unwrap();

    let mut phase = valid.clone();
    phase[16] = 3;
    assert!(matches!(
        decode_frame(&phase),
        Err(CodecError::InvalidValue)
    ));

    for value in [f64::NAN, 1.5] {
        let mut bytes = valid.clone();
        bytes[17..25].copy_from_slice(&value.to_le_bytes());
        assert!(matches!(
            decode_frame(&bytes),
            Err(CodecError::InvalidValue)
        ));
    }

    let mut short = valid.clone();
    short.truncate(short.len() - 1);
    assert!(matches!(decode_frame(&short), Err(CodecError::Truncated)));

    let mut trailing = valid;
    trailing[0..4].copy_from_slice(&22_u32.to_le_bytes());
    trailing.push(0);
    assert!(matches!(
        decode_frame(&trailing),
        Err(CodecError::TrailingBytes)
    ));
}

#[test]
fn embedding_rejects_unknown_platform_tags() {
    let mut bytes = encode_frame(&Frame {
        flags: 0,
        request: 1,
        message: Message::OpenEditorIn {
            parent: vst_audio_protocol::ParentWindow::Win32 { hwnd: 1 },
            scaling: vst_audio_protocol::EditorScaling::Auto,
        },
    })
    .unwrap();
    for tag in [2, 127, 255] {
        bytes[12] = tag;
        assert!(matches!(
            decode_frame(&bytes),
            Err(CodecError::InvalidValue)
        ));
    }
}

#[test]
fn malicious_frames_are_rejected_and_unknown_kinds_skip() {
    let mut huge = Vec::new();
    huge.extend_from_slice(&(65_537_u32).to_le_bytes());
    huge.extend_from_slice(&4_u16.to_le_bytes());
    huge.extend_from_slice(&0_u16.to_le_bytes());
    huge.extend_from_slice(&0_u32.to_le_bytes());
    assert!(matches!(
        read_frame(&mut Cursor::new(huge)),
        Err(CodecError::TooLarge { .. })
    ));

    let frame = encode_frame(&Frame {
        flags: 0,
        request: 0,
        message: Message::hello(),
    })
    .unwrap();
    assert!(matches!(
        decode_frame(&frame[..frame.len() - 1]),
        Err(CodecError::Truncated)
    ));

    let mut bad_utf8 = vec![11, 0, 0, 0, 9, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0xff];
    assert!(matches!(
        decode_frame(&bad_utf8),
        Err(CodecError::InvalidUtf8)
    ));
    bad_utf8[4..6].copy_from_slice(&99_u16.to_le_bytes());
    assert!(matches!(
        decode_frame(&bad_utf8).unwrap(),
        DecodedFrame::Unknown(_)
    ));

    let mut wrong_major = frame;
    wrong_major[16..18].copy_from_slice(&1_u16.to_le_bytes());
    assert!(matches!(
        decode_frame(&wrong_major),
        Err(CodecError::WrongMajor { .. })
    ));

    assert!(
        encode_frame(&Frame {
            flags: 0,
            request: 0,
            message: Message::Scan {
                path: "x".repeat(MAX_STRING_BYTES + 1)
            }
        })
        .is_err()
    );
}

#[test]
fn bus_info_rejects_unknown_kinds_and_flags() {
    let frame = Frame {
        flags: 0,
        request: 1,
        message: Message::Loaded {
            name: String::new(),
            vendor: String::new(),
            latency_frames: 0,
            inputs: vec![BusInfo {
                layout: Bus {
                    channels: 2,
                    arrangement: 3,
                },
                kind: BusKind::Main,
                name: String::new(),
                default_active: true,
            }],
            outputs: Vec::new(),
        },
    };
    let encoded = encode_frame(&frame).unwrap();
    // Header (12), two empty strings (4), latency (4), count (2), layout (10).
    let kind = 32;
    let flags = kind + 1;

    let mut unknown_kind = encoded.clone();
    unknown_kind[kind] = 2;
    assert!(matches!(
        decode_frame(&unknown_kind),
        Err(CodecError::InvalidValue)
    ));

    let mut unknown_flags = encoded;
    unknown_flags[flags] = 2;
    assert!(matches!(
        decode_frame(&unknown_flags),
        Err(CodecError::InvalidValue)
    ));
}

fn running(now: u64) -> HostWatch {
    let mut watch = HostWatch::new(now);
    assert_eq!(watch.received(now, Received::Hello), Action::None);
    assert_eq!(watch.received(now, Received::Loaded), Action::None);
    assert_eq!(
        watch.received(now, Received::Configured),
        Action::SaveStateNow
    );
    watch
}

#[test]
fn every_deadline_fires_at_its_edge() {
    let mut watch = HostWatch::new(0);
    assert_eq!(watch.tick(4_999), Action::None);
    assert_eq!(
        watch.tick(5_000),
        Action::Kill {
            reason: KillReason::HelloTimeout
        }
    );

    let mut watch = HostWatch::new(0);
    watch.received(0, Received::Hello);
    assert_eq!(watch.tick(59_999), Action::None);
    assert_eq!(
        watch.tick(60_000),
        Action::Kill {
            reason: KillReason::LoadTimeout
        }
    );

    let mut watch = HostWatch::new(0);
    watch.received(0, Received::Hello);
    watch.received(0, Received::Loaded);
    assert_eq!(watch.tick(9_999), Action::None);
    assert_eq!(
        watch.tick(10_000),
        Action::Kill {
            reason: KillReason::ConfigureTimeout
        }
    );

    let mut watch = running(0);
    assert_eq!(watch.tick(499), Action::None);
    assert_eq!(
        watch.tick(500),
        Action::Kill {
            reason: KillReason::HeartbeatTimeout
        }
    );

    let mut watch = running(0);
    watch.heartbeat(1, 1);
    watch.request_sent(1, 7, RequestKind::StateSave);
    assert_eq!(
        watch.tick(10_000),
        Action::Kill {
            reason: KillReason::HeartbeatTimeout
        }
    );

    let mut watch = running(0);
    watch.shutdown(0);
    assert_eq!(watch.tick(1_999), Action::None);
    assert_eq!(
        watch.tick(2_000),
        Action::Kill {
            reason: KillReason::StopTimeout
        }
    );
}

#[test]
fn heartbeat_silence_counts_only_while_the_host_observes() {
    let mut watch = running(0);
    watch.heartbeat(100, 1);
    watch.heartbeat(200, 1);
    // The host polls 600 ms late: the worker was not observed in that time.
    watch.heartbeat(800, 1);
    assert_eq!(watch.tick(800), Action::None);
    let mut now = 800;
    while now < 1_150 {
        now += 50;
        watch.heartbeat(now, 1);
        assert_eq!(watch.tick(now), Action::None);
    }
    // 100 ms before the gap and 400 ms after it: 500 ms of observed silence.
    watch.heartbeat(1_200, 1);
    assert_eq!(
        watch.tick(1_200),
        Action::Kill {
            reason: KillReason::HeartbeatTimeout
        }
    );

    // Polls at most OBSERVER_GAP_MS apart count in full.
    let mut watch = running(0);
    watch.heartbeat(250, 0);
    assert_eq!(watch.tick(250), Action::None);
    watch.heartbeat(500, 0);
    assert_eq!(
        watch.tick(500),
        Action::Kill {
            reason: KillReason::HeartbeatTimeout
        }
    );
}

#[test]
fn running_request_deadline_is_independent_of_heartbeat() {
    let mut watch = running(0);
    watch.request_sent(0, 7, RequestKind::Editor);
    for now in [400, 800, 1_200, 2_000, 5_000, 9_999] {
        watch.heartbeat(now, now);
        assert_eq!(watch.tick(now), Action::None);
    }
    watch.heartbeat(10_000, 10_000);
    assert_eq!(
        watch.tick(10_000),
        Action::Kill {
            reason: KillReason::RequestTimeout
        }
    );
}

#[test]
fn backoff_sequence_and_three_failures_window() {
    let mut watch = HostWatch::new(0);
    assert_eq!(watch.process_exited(0, 1), Action::EnterFallback);
    assert_eq!(watch.tick(499), Action::None);
    assert_eq!(watch.tick(500), Action::Spawn);
    assert_eq!(watch.process_exited(500, 1), Action::EnterFallback);
    assert_eq!(watch.tick(2_499), Action::None);
    assert_eq!(watch.tick(2_500), Action::Spawn);
    assert_eq!(
        watch.process_exited(2_500, 1),
        Action::Disable {
            reason: DisableReason::ThreeFailuresInTenMinutes
        }
    );
    assert_eq!(watch.state(), WatchState::Disabled);

    let mut watch = HostWatch::new(0);
    watch.process_exited(0, 1);
    assert_eq!(watch.tick(500), Action::Spawn);
    watch.process_exited(330_001, 1);
    assert_eq!(watch.tick(332_001), Action::Spawn);
    assert_eq!(watch.process_exited(660_001, 1), Action::EnterFallback);
    assert_eq!(watch.tick(668_000), Action::None);
    assert_eq!(watch.tick(668_001), Action::Spawn);
}

#[test]
fn timeout_and_following_process_exit_count_once() {
    let mut watch = HostWatch::new(0);
    assert_eq!(
        watch.tick(5_000),
        Action::Kill {
            reason: KillReason::HelloTimeout
        }
    );
    assert_eq!(watch.process_exited(5_001, 1), Action::None);
    assert_eq!(watch.tick(5_500), Action::Spawn);
    assert_eq!(watch.process_exited(5_500, 1), Action::EnterFallback);
    assert_eq!(watch.tick(7_500), Action::Spawn);
    assert_eq!(
        watch.process_exited(7_500, 1),
        Action::Disable {
            reason: DisableReason::ThreeFailuresInTenMinutes
        }
    );
}

#[test]
fn fallback_enters_after_exactly_three_and_leaves_on_delivery() {
    let mut watch = running(0);
    assert_eq!(watch.block_missed(), Action::None);
    assert_eq!(watch.block_missed(), Action::None);
    assert_eq!(watch.block_missed(), Action::EnterFallback);
    assert_eq!(watch.block_missed(), Action::None);
    assert_eq!(watch.block_delivered(), Action::LeaveFallback);
}

#[test]
fn state_save_timing_and_previous_fallback() {
    let mut watch = running(0);
    watch.heartbeat(1_000, 1);
    assert_eq!(watch.received(1_000, Received::ParamsChanged), Action::None);
    watch.heartbeat(2_999, 2);
    assert_eq!(watch.tick(2_999), Action::None);
    watch.heartbeat(3_000, 3);
    assert_eq!(watch.tick(3_000), Action::SaveStateNow);
    assert_eq!(
        watch.received(3_001, Received::EditorClosed),
        Action::SaveStateNow
    );
    assert_eq!(watch.shutdown(3_002), Action::SaveStateNow);

    let mut history = StateHistory::new();
    history.save(vec![1]);
    history.save(vec![2]);
    assert_eq!(history.current(), Some([2].as_slice()));
    assert_eq!(history.previous(), Some([1].as_slice()));
    assert_eq!(history.begin_restore(), Some([2].as_slice()));
    assert_eq!(history.crash_after_restore(), Some([1].as_slice()));
}

#[test]
fn a_requested_stop_is_not_a_failure() {
    let mut watch = running(0);
    assert_eq!(watch.shutdown(10), Action::SaveStateNow);
    assert_eq!(watch.process_exited(20, 0), Action::None);
    assert_eq!(watch.state(), WatchState::Stopped);
    assert_eq!(watch.tick(60_000), Action::None);

    let mut watch = running(0);
    watch.shutdown(0);
    assert_eq!(
        watch.tick(2_000),
        Action::Kill {
            reason: KillReason::StopTimeout
        }
    );
    assert_eq!(watch.state(), WatchState::Stopped);
    assert_eq!(watch.process_exited(2_001, 1), Action::None);
    assert_eq!(watch.tick(20_000), Action::None);
}

#[test]
fn every_pending_request_has_its_own_deadline() {
    let mut watch = running(0);
    watch.request_sent(0, 1, RequestKind::Editor);
    watch.request_sent(5_000, 2, RequestKind::StateSave);
    watch.heartbeat(6_000, 1);
    assert_eq!(
        watch.received(6_000, Received::Response { request: 2 }),
        Action::None
    );
    watch.heartbeat(9_999, 2);
    assert_eq!(watch.tick(9_999), Action::None);
    watch.heartbeat(10_000, 3);
    assert_eq!(
        watch.tick(10_000),
        Action::Kill {
            reason: KillReason::RequestTimeout
        }
    );

    let mut watch = running(0);
    watch.request_sent(0, 1, RequestKind::Editor);
    watch.request_sent(1, 2, RequestKind::StateSave);
    watch.received(2, Received::Response { request: 1 });
    watch.received(3, Received::Response { request: 2 });
    watch.heartbeat(10_000, 1);
    assert_eq!(watch.tick(10_000), Action::None);
}

#[test]
fn a_restarted_host_leaves_the_fallback_on_its_first_block() {
    let mut watch = running(0);
    assert_eq!(
        watch.tick(500),
        Action::Kill {
            reason: KillReason::HeartbeatTimeout
        }
    );
    assert_eq!(watch.tick(1_000), Action::Spawn);
    watch.received(1_000, Received::Hello);
    watch.received(1_000, Received::Loaded);
    watch.received(1_000, Received::Configured);
    assert_eq!(watch.block_delivered(), Action::LeaveFallback);
    assert_eq!(watch.block_delivered(), Action::None);
}

#[test]
fn the_backoff_starts_again_once_the_window_is_clear() {
    let mut watch = HostWatch::new(0);
    watch.process_exited(0, 1);
    assert_eq!(watch.tick(500), Action::Spawn);
    watch.process_exited(1_000, 1);
    assert_eq!(watch.tick(3_000), Action::Spawn);
    // Both failures have left the ten-minute window.
    watch.process_exited(700_000, 1);
    assert_eq!(watch.tick(700_499), Action::None);
    assert_eq!(watch.tick(700_500), Action::Spawn);
}

#[test]
fn editor_resized_rejects_out_of_range_dimensions() {
    for (width, height) in [(0, 1), (1, 0), (8193, 1), (1, 8193), (u32::MAX, 1)] {
        let frame = Frame {
            flags: 0,
            request: 0,
            message: Message::EditorResized { width, height },
        };
        assert!(matches!(
            encode_frame(&frame),
            Err(CodecError::InvalidValue)
        ));

        let mut bytes = vec![0; 20];
        bytes[..4].copy_from_slice(&16u32.to_le_bytes());
        bytes[4..6].copy_from_slice(&31u16.to_le_bytes());
        bytes[12..16].copy_from_slice(&width.to_le_bytes());
        bytes[16..20].copy_from_slice(&height.to_le_bytes());
        assert!(matches!(
            decode_frame(&bytes),
            Err(CodecError::InvalidValue)
        ));
    }
}

#[test]
fn editor_resized_rejects_short_body() {
    let bytes = encode_frame(&Frame {
        flags: 0,
        request: 0,
        message: Message::EditorResized {
            width: 500,
            height: 280,
        },
    })
    .unwrap();
    assert_eq!(u16::from_le_bytes(bytes[4..6].try_into().unwrap()), 31);
    for length in 0..8 {
        let mut short = bytes[..12 + length].to_vec();
        short[..4].copy_from_slice(&((8 + length) as u32).to_le_bytes());
        assert!(decode_frame(&short).is_err());
    }
}
