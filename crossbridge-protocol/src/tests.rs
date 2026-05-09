use super::*;
use std::ffi::OsString;
use std::io::Cursor;

fn sample_register() -> Register {
    Register {
        slug: "firmware".into(),
        group: "amd-psp".into(),
    }
}

fn sample_submit() -> SubmitIssue {
    SubmitIssue {
        title: "Found a bug".into(),
        body: "Coverage map attached.".into(),
        labels: vec!["bug".into(), "fuzz".into()],
        source_slug: "fuzzer".into(),
        source_uuid: "abcd-1234".into(),
        attachments: vec![Attachment {
            filename: "coverage.bin".into(),
            data: vec![0xDE, 0xAD, 0xBE, 0xEF],
        }],
    }
}

fn sample_answer() -> SubmitAnswer {
    SubmitAnswer {
        source_uuid: "abcd-1234".into(),
        comments: vec![AnswerComment {
            content: "Fixed in 0x1234".into(),
            kind: "result".into(),
        }],
        attachments: vec![],
    }
}

fn roundtrip<T>(value: &T)
where
    T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let bytes = postcard::to_stdvec(value).unwrap();
    let decoded: T = postcard::from_bytes(&bytes).unwrap();
    assert_eq!(value, &decoded);
}

#[test]
fn register_roundtrip() {
    roundtrip(&sample_register());
}

#[test]
fn register_response_variants_roundtrip() {
    roundtrip(&RegisterResponse::Ack {
        peers: vec!["a".into(), "b".into()],
    });
    roundtrip(&RegisterResponse::Nack {
        reason: "slug taken".into(),
    });
}

#[test]
fn notification_roundtrip() {
    roundtrip(&Notification::PeerJoined { slug: "x".into() });
    roundtrip(&Notification::PeerLeft { slug: "y".into() });
}

#[test]
fn supervisor_message_discrimination() {
    let ack = SupervisorMessage::RegisterResponse(RegisterResponse::Ack { peers: vec![] });
    let notif = SupervisorMessage::Notification(Notification::PeerJoined { slug: "x".into() });
    roundtrip(&ack);
    roundtrip(&notif);

    let ack_bytes = postcard::to_stdvec(&ack).unwrap();
    let notif_bytes = postcard::to_stdvec(&notif).unwrap();
    assert_ne!(
        ack_bytes[0], notif_bytes[0],
        "outer enum variants must use distinct postcard tags"
    );
}

#[test]
fn client_request_roundtrip() {
    roundtrip(&ClientRequest::Submit(sample_submit()));
    roundtrip(&ClientRequest::Answer(sample_answer()));
}

#[test]
fn server_response_roundtrip() {
    roundtrip(&ServerResponse::Ok { issue_id: 42 });
    roundtrip(&ServerResponse::Error {
        message: "nope".into(),
    });
}

#[test]
fn sync_framing_roundtrip() {
    let mut buf: Vec<u8> = Vec::new();
    let msg = ClientRequest::Submit(sample_submit());
    write_message_sync(&mut buf, &msg).unwrap();

    // Length prefix is 4 bytes big-endian and matches the postcard payload size.
    let payload_len = postcard::to_stdvec(&msg).unwrap().len();
    let prefix = u32::from_be_bytes(buf[..4].try_into().unwrap()) as usize;
    assert_eq!(prefix, payload_len);
    assert_eq!(buf.len(), 4 + payload_len);

    let mut cursor = Cursor::new(buf);
    let decoded: ClientRequest = read_message_sync(&mut cursor).unwrap();
    assert_eq!(decoded, msg);
}

#[test]
fn sync_framing_back_to_back() {
    let mut buf: Vec<u8> = Vec::new();
    let a = SupervisorMessage::RegisterResponse(RegisterResponse::Ack {
        peers: vec!["p1".into()],
    });
    let b = SupervisorMessage::Notification(Notification::PeerJoined { slug: "p2".into() });
    write_message_sync(&mut buf, &a).unwrap();
    write_message_sync(&mut buf, &b).unwrap();

    let mut cursor = Cursor::new(buf);
    let got_a: SupervisorMessage = read_message_sync(&mut cursor).unwrap();
    let got_b: SupervisorMessage = read_message_sync(&mut cursor).unwrap();
    assert_eq!(got_a, a);
    assert_eq!(got_b, b);
}

#[test]
fn sync_read_rejects_oversize_frame() {
    let mut buf: Vec<u8> = Vec::new();
    // MAX_FRAME_SIZE = 16 MiB, fits in u32 by construction.
    #[allow(clippy::cast_possible_truncation)]
    let oversize = (MAX_FRAME_SIZE as u32 + 1).to_be_bytes();
    buf.extend_from_slice(&oversize);
    let mut cursor = Cursor::new(buf);
    let err = read_message_sync::<_, Register>(&mut cursor).unwrap_err();
    assert!(matches!(err, Error::FrameTooLarge { .. }));
}

#[test]
fn sync_write_rejects_oversize_payload() {
    // Build an Attachment whose serialized form exceeds MAX_FRAME_SIZE.
    let big = Attachment {
        filename: "big".into(),
        data: vec![0u8; MAX_FRAME_SIZE + 1],
    };
    let mut sink: Vec<u8> = Vec::new();
    let err = write_message_sync(&mut sink, &big).unwrap_err();
    assert!(matches!(err, Error::FrameTooLarge { .. }));
}

#[test]
fn sync_read_short_input_is_io_error() {
    let mut cursor = Cursor::new(vec![0u8, 0, 0]); // only 3 bytes, prefix needs 4
    let err = read_message_sync::<_, Register>(&mut cursor).unwrap_err();
    assert!(matches!(err, Error::Io(_)));
}

#[tokio::test(flavor = "current_thread")]
async fn async_framing_roundtrip() {
    let msg = ClientRequest::Answer(sample_answer());
    let mut buf: Vec<u8> = Vec::new();
    write_message(&mut buf, &msg).await.unwrap();

    let mut cursor = Cursor::new(buf);
    let decoded: ClientRequest = read_message(&mut cursor).await.unwrap();
    assert_eq!(decoded, msg);
}

#[tokio::test(flavor = "current_thread")]
async fn async_framing_back_to_back() {
    let a = ServerResponse::Ok { issue_id: 1 };
    let b = ServerResponse::Error {
        message: "boom".into(),
    };
    let mut buf: Vec<u8> = Vec::new();
    write_message(&mut buf, &a).await.unwrap();
    write_message(&mut buf, &b).await.unwrap();

    let mut cursor = Cursor::new(buf);
    let got_a: ServerResponse = read_message(&mut cursor).await.unwrap();
    let got_b: ServerResponse = read_message(&mut cursor).await.unwrap();
    assert_eq!(got_a, a);
    assert_eq!(got_b, b);
}

#[tokio::test(flavor = "current_thread")]
async fn async_read_rejects_oversize_frame() {
    let mut buf: Vec<u8> = Vec::new();
    // MAX_FRAME_SIZE = 16 MiB, fits in u32 by construction.
    #[allow(clippy::cast_possible_truncation)]
    let oversize = (MAX_FRAME_SIZE as u32 + 1).to_be_bytes();
    buf.extend_from_slice(&oversize);
    let mut cursor = Cursor::new(buf);
    let err = read_message::<_, Register>(&mut cursor).await.unwrap_err();
    assert!(matches!(err, Error::FrameTooLarge { .. }));
}

#[test]
fn sync_and_async_wire_compatible() {
    // A frame written synchronously must be readable through the async path
    // (and vice-versa) — the wire format is identical.
    let msg = SupervisorMessage::Notification(Notification::PeerJoined { slug: "z".into() });
    let mut buf: Vec<u8> = Vec::new();
    write_message_sync(&mut buf, &msg).unwrap();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let decoded: SupervisorMessage = runtime.block_on(async {
        let mut cursor = Cursor::new(buf);
        read_message(&mut cursor).await.unwrap()
    });
    assert_eq!(decoded, msg);
}

#[test]
fn default_socket_root_prefers_crossbridge_env() {
    let resolved = default_socket_root(|k| match k {
        SOCKET_ROOT_ENV => Some(OsString::from("/srv/run")),
        XDG_RUNTIME_DIR_ENV => Some(OsString::from("/run/user/1000")),
        _ => None,
    });
    assert_eq!(resolved, PathBuf::from("/srv/run"));
}

#[test]
fn default_socket_root_falls_back_to_xdg() {
    let resolved = default_socket_root(|k| match k {
        XDG_RUNTIME_DIR_ENV => Some(OsString::from("/run/user/1000")),
        _ => None,
    });
    assert_eq!(resolved, PathBuf::from("/run/user/1000/crossbridge"));
}

#[test]
fn default_socket_root_falls_back_to_compiled_in_default() {
    let resolved = default_socket_root(|_| None);
    assert_eq!(resolved, PathBuf::from(DEFAULT_SOCKET_ROOT));
}
