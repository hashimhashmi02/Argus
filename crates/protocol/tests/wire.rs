//! The wire format, pinned.
//!
//! Some of these tests assert exact JSON strings. That is deliberate and a
//! little unusual: the JSON *is* the contract with every binary not built from
//! this commit, so a change to it must be a conscious decision that also bumps
//! `PROTOCOL_VERSION`, not a side effect of renaming a field.

use argus_protocol::{
    CodecError, Envelope, ErrorCode, FrameDecoder, MAX_FRAME_LEN, PROTOCOL_VERSION, Reply, Request,
    Response, RpcError, SessionInfo, Status, WatchEvent, encode,
};

fn json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap()
}

fn session() -> SessionInfo {
    SessionInfo {
        id: "3fa1c2d4".into(),
        agent: "shell".into(),
        status: Status::NeedsYou,
        since_ms: 1500,
        status_for_ms: 240_000,
        rule: Some("permission-prompt".into()),
        pid: Some(4242),
        exit_code: None,
        lost: false,
        resumable: false,
    }
}

// ---- the wire format -------------------------------------------------------

#[test]
fn requests_have_a_stable_readable_shape() {
    assert_eq!(
        json(&Envelope::new(Request::List)),
        r#"{"version":1,"method":"list"}"#
    );

    assert_eq!(
        json(&Envelope::new(Request::SendText {
            id: "a1".into(),
            text: "ls\r".into()
        })),
        r#"{"version":1,"method":"send_text","id":"a1","text":"ls\r"}"#
    );

    // Optional fields are omitted, not sent as null, so the common case stays
    // short enough to type by hand when debugging.
    assert_eq!(
        json(&Envelope::new(Request::Spawn {
            agent: "shell".into(),
            cwd: None,
            cols: None,
            rows: None,
        })),
        r#"{"version":1,"method":"spawn","agent":"shell"}"#
    );
}

#[test]
fn responses_say_ok_or_err_in_words() {
    assert_eq!(
        json(&Response::Ok(Reply::Done)),
        r#"{"ok":{"kind":"done"}}"#
    );

    assert_eq!(
        json(&Response::Err(RpcError::new(
            ErrorCode::UnknownSession,
            "no session 99"
        ))),
        r#"{"err":{"code":"unknown_session","message":"no session 99"}}"#
    );
}

#[test]
fn status_serialises_in_snake_case() {
    let reply = Response::Ok(Reply::Session { session: session() });
    let text = json(&reply);

    assert!(text.contains(r#""status":"needs_you""#), "got {text}");
    // `exit_code` is None and must be omitted rather than sent as null.
    assert!(!text.contains("exit_code"), "got {text}");
}

#[test]
fn every_request_round_trips() {
    let requests = [
        Request::List,
        Request::Spawn {
            agent: "claude-code".into(),
            cwd: Some(r"C:\work\repo".into()),
            cols: Some(100),
            rows: Some(30),
        },
        Request::SendText {
            id: "a".into(),
            text: "hello\r".into(),
        },
        Request::Resize {
            id: "a".into(),
            cols: 80,
            rows: 24,
        },
        Request::ReadScreen { id: "a".into() },
        Request::Kill { id: "a".into() },
        Request::Watch { id: "a".into() },
        Request::Resume {
            id: "a".into(),
            cols: None,
            rows: Some(40),
        },
    ];

    for request in requests {
        let envelope = Envelope::new(request.clone());
        let back: Envelope = serde_json::from_str(&json(&envelope)).unwrap();
        assert_eq!(back, envelope);
        assert_eq!(back.version, PROTOCOL_VERSION);
    }
}

#[test]
fn an_unknown_field_from_a_newer_client_is_tolerated() {
    // A newer client may add an optional field this build has never heard of.
    // It should still be understood, with the version check -- not a parse
    // failure -- deciding whether the two can talk.
    let raw = r#"{"version":1,"method":"list","some_future_flag":true}"#;
    let envelope: Envelope = serde_json::from_str(raw).unwrap();
    assert_eq!(envelope.request, Request::List);
}

// ---- framing ---------------------------------------------------------------

#[test]
fn a_frame_is_a_big_endian_length_then_the_payload() {
    let frame = encode(&Envelope::new(Request::List)).unwrap();
    let payload = br#"{"version":1,"method":"list"}"#;

    assert_eq!(&frame[..4], &(payload.len() as u32).to_be_bytes());
    assert_eq!(&frame[4..], payload);
}

#[test]
fn a_header_split_across_reads_is_reassembled() {
    // The case "one read = one message" code gets wrong: the length prefix
    // itself arrives in two pieces.
    let frame = encode(&Envelope::new(Request::List)).unwrap();
    let mut decoder = FrameDecoder::new();

    decoder.push(&frame[..2]);
    assert!(decoder.next_frame::<Envelope>().unwrap().is_none());
    decoder.push(&frame[2..]);
    assert_eq!(
        decoder.next_frame::<Envelope>().unwrap().unwrap().request,
        Request::List
    );
}

#[test]
fn a_frame_delivered_one_byte_at_a_time_is_reassembled() {
    let frame = encode(&Envelope::new(Request::ReadScreen { id: "abc".into() })).unwrap();
    let mut decoder = FrameDecoder::new();

    for (i, byte) in frame.iter().enumerate() {
        decoder.push(std::slice::from_ref(byte));
        let result = decoder.next_frame::<Envelope>().unwrap();
        if i + 1 < frame.len() {
            assert!(
                result.is_none(),
                "frame reported complete after {} bytes",
                i + 1
            );
        } else {
            assert_eq!(
                result.unwrap().request,
                Request::ReadScreen { id: "abc".into() }
            );
        }
    }
    assert_eq!(decoder.buffered(), 0);
}

#[test]
fn two_frames_in_one_read_both_come_out() {
    let mut bytes = encode(&Envelope::new(Request::List)).unwrap();
    bytes.extend(encode(&Envelope::new(Request::Kill { id: "x".into() })).unwrap());

    let mut decoder = FrameDecoder::new();
    decoder.push(&bytes);

    assert_eq!(
        decoder.next_frame::<Envelope>().unwrap().unwrap().request,
        Request::List
    );
    assert_eq!(
        decoder.next_frame::<Envelope>().unwrap().unwrap().request,
        Request::Kill { id: "x".into() }
    );
    assert!(decoder.next_frame::<Envelope>().unwrap().is_none());
}

#[test]
fn an_oversized_frame_is_rejected_from_its_header_alone() {
    // The limit is useless if it is checked after the body has been buffered.
    // Only the four header bytes are pushed here, and that must be enough.
    let mut decoder = FrameDecoder::new();
    decoder.push(&u32::MAX.to_be_bytes());

    let error = decoder.next_frame::<Envelope>().unwrap_err();
    assert!(matches!(error, CodecError::TooLarge { .. }));
    assert!(
        error.is_fatal(),
        "the stream cannot be resynchronised after this"
    );
}

#[test]
fn a_frame_exactly_at_the_limit_is_not_rejected_as_too_large() {
    let mut decoder = FrameDecoder::new();
    decoder.push(&(MAX_FRAME_LEN as u32).to_be_bytes());

    // Not complete yet, but not refused either.
    assert!(decoder.next_frame::<Envelope>().unwrap().is_none());
}

#[test]
fn a_malformed_payload_is_consumed_and_the_stream_carries_on() {
    // One bad request should cost the client an error reply, not its
    // connection: the frame boundary was known, so nothing is lost.
    let garbage = b"not json at all";
    let mut bytes = (garbage.len() as u32).to_be_bytes().to_vec();
    bytes.extend_from_slice(garbage);
    bytes.extend(encode(&Envelope::new(Request::List)).unwrap());

    let mut decoder = FrameDecoder::new();
    decoder.push(&bytes);

    let error = decoder.next_frame::<Envelope>().unwrap_err();
    assert!(matches!(error, CodecError::Payload(_)));
    assert!(!error.is_fatal());

    assert_eq!(
        decoder.next_frame::<Envelope>().unwrap().unwrap().request,
        Request::List
    );
}

#[test]
fn an_unknown_method_is_a_payload_error_not_a_framing_error() {
    let mut decoder = FrameDecoder::new();
    let payload = br#"{"version":1,"method":"self_destruct"}"#;
    decoder.push(&(payload.len() as u32).to_be_bytes());
    decoder.push(payload);

    let error = decoder.next_frame::<Envelope>().unwrap_err();
    assert!(!error.is_fatal());
}

#[test]
fn encoding_refuses_to_produce_a_frame_the_peer_must_reject() {
    let huge = Request::SendText {
        id: "a".into(),
        text: "x".repeat(MAX_FRAME_LEN),
    };
    assert!(matches!(
        encode(&Envelope::new(huge)),
        Err(CodecError::TooLarge { .. })
    ));
}

#[test]
fn a_reply_from_an_engine_predating_status_for_ms_still_parses() {
    // `status_for_ms` was added after v1 without a version bump. That is only
    // sound if a newer client can read an older engine's reply, which simply
    // lacks the field. It must default rather than fail.
    let old = r#"{"id":"a1","agent":"shell","status":"idle","since_ms":10}"#;
    let info: SessionInfo = serde_json::from_str(old).unwrap();
    assert_eq!(info.status_for_ms, 0);
    assert_eq!(info.status, Status::Idle);
}

#[test]
fn the_ui_methods_have_a_stable_shape() {
    assert_eq!(
        json(&Envelope::new(Request::Agents)),
        r#"{"version":1,"method":"agents"}"#
    );
    assert_eq!(
        json(&Envelope::new(Request::Respond {
            id: "a1".into(),
            approve: true
        })),
        r#"{"version":1,"method":"respond","id":"a1","approve":true}"#
    );
    assert_eq!(
        json(&Response::Ok(Reply::Agents {
            agents: vec![argus_protocol::AgentInfo {
                id: "shell".into(),
                display_name: "Shell".into()
            }]
        })),
        r#"{"ok":{"kind":"agents","agents":[{"id":"shell","display_name":"Shell"}]}}"#
    );
}

#[test]
fn watching_has_a_stable_readable_shape() {
    assert_eq!(
        json(&Envelope::new(Request::Watch { id: "a1".into() })),
        r#"{"version":1,"method":"watch","id":"a1"}"#
    );
    assert_eq!(
        json(&Response::Ok(Reply::Watching)),
        r#"{"ok":{"kind":"watching"}}"#
    );

    // Terminal output travels as JSON text, escape sequences and all.
    assert_eq!(
        json(&WatchEvent::Repaint {
            cols: 120,
            rows: 30,
            data: "\u{1b}[H\u{1b}[2J>".into()
        }),
        r#"{"event":"repaint","cols":120,"rows":30,"data":"\u001b[H\u001b[2J>"}"#
    );
    assert_eq!(
        json(&WatchEvent::Output {
            data: "日本\r\n".into()
        }),
        r#"{"event":"output","data":"日本\r\n"}"#
    );
    assert_eq!(json(&WatchEvent::Ended), r#"{"event":"ended"}"#);
}

#[test]
fn watch_events_round_trip_through_frames() {
    let events = [
        WatchEvent::Repaint {
            cols: 80,
            rows: 24,
            data: "\u{1b}[0m\u{1b}[31mred".into(),
        },
        WatchEvent::Output {
            data: "more\u{1b}[?25l".into(),
        },
        WatchEvent::Ended,
    ];
    let mut decoder = FrameDecoder::new();
    for event in &events {
        decoder.push(&encode(event).unwrap());
    }
    for event in events {
        assert_eq!(decoder.next_frame::<WatchEvent>().unwrap(), Some(event));
    }
}

#[test]
fn an_older_engine_rejects_a_newer_method_as_a_payload_error() {
    // Not a version bump, because nothing is misread: a method this build has
    // never heard of fails to parse, as a recoverable payload error, and the
    // engine answers bad_request. Pinned here with a method that does not exist.
    let mut decoder = FrameDecoder::new();
    let payload = br#"{"version":1,"method":"teleport","id":"a1"}"#;
    decoder.push(&(payload.len() as u32).to_be_bytes());
    decoder.push(payload);

    let error = decoder.next_frame::<Envelope>().unwrap_err();
    assert!(!error.is_fatal());
}

#[test]
fn resume_and_resumable_are_additive() {
    assert_eq!(
        json(&Envelope::new(Request::Resume {
            id: "3fa1".into(),
            cols: None,
            rows: None,
        })),
        r#"{"version":1,"method":"resume","id":"3fa1"}"#
    );

    // Omitted when false, so an older client sees exactly what it always did;
    // defaulted when absent, so a newer client reads an older engine as
    // "nothing to resume".
    assert!(!json(&session()).contains("resumable"));
    let lost = SessionInfo {
        status: Status::Done,
        pid: None,
        lost: true,
        resumable: true,
        ..session()
    };
    assert!(json(&lost).contains(r#""resumable":true"#));
    let from_an_older_engine: SessionInfo =
        serde_json::from_str(&json(&lost).replace(r#","resumable":true"#, "")).unwrap();
    assert!(!from_an_older_engine.resumable);
}
