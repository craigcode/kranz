use kranz_acp::{classify_line, encode_message, Client, Frame, RpcOutcome, UpdateKind};
use serde_json::json;

fn ready() -> Client {
    let mut client = Client::new();
    let request = client.initialize(json!({"name":"conformance"})).unwrap();
    client
        .accept_initialize(request.id, &json!({"protocolVersion":1}))
        .unwrap();
    let request = client.new_session("/fixture").unwrap();
    client
        .accept_session(request.id, &json!({"sessionId":"peer-1"}))
        .unwrap();
    client
}

#[test]
fn shared_acp_setup_requires_matching_responses_and_stable_version() {
    let mut client = Client::new();
    assert!(client.prompt("too early").is_err());
    assert!(client.new_session("/fixture").is_err());
    let request = client.initialize(json!({"name":"test"})).unwrap();
    assert_eq!(request.id, 1);
    assert_eq!(
        request.message["params"]["clientCapabilities"]["terminal"],
        false
    );
    assert!(client
        .accept_initialize(99, &json!({"protocolVersion":1}))
        .is_err());
    for value in [
        json!({}),
        json!({"protocolVersion":2}),
        json!({"protocolVersion":"1"}),
    ] {
        assert!(client.accept_initialize(1, &value).is_err());
    }
    client
        .accept_initialize(
            1,
            &json!({"protocolVersion":1,"agentCapabilities":{"loadSession":true}}),
        )
        .unwrap();
    assert!(client.initialize(json!({})).is_err());
    let request = client.new_session("/fixture").unwrap();
    assert_eq!(request.id, 2);
    assert!(client
        .accept_session(99, &json!({"sessionId":"peer-1"}))
        .is_err());
    for id in [json!(null), json!(""), json!(" "), json!("x".repeat(257))] {
        assert!(client.accept_session(2, &json!({"sessionId":id})).is_err());
        assert!(client.session_id().is_none());
    }
    assert_eq!(
        client
            .accept_session(2, &json!({"sessionId":"peer-1"}))
            .unwrap(),
        "peer-1"
    );
    assert!(client.new_session("/other").is_err());
}

#[test]
fn shared_acp_cancel_waits_for_correlated_completion_before_followup() {
    let mut client = ready();
    let first = client.prompt("first").unwrap();
    assert_eq!(first.id, 3);
    assert!(client.prompt("overlap").is_err());
    assert!(!client.complete_prompt(2));
    assert_eq!(client.cancel().unwrap()["params"]["sessionId"], "peer-1");
    assert!(client.prompt_in_flight());
    assert!(client.complete_prompt(first.id));
    assert!(!client.complete_prompt(first.id));
    let second = client.prompt("second").unwrap();
    assert_eq!(second.id, 4);
    assert!(!client.complete_prompt(first.id));
    assert!(client.prompt_in_flight());
}

#[test]
fn shared_acp_raw_updates_preserve_identity_unknown_fields_and_cost_gaps() {
    let client = ready();
    for id in [json!(null), json!("foreign")] {
        assert!(client
            .session_update(&json!({"sessionId":id,"update":{}}))
            .is_err());
    }
    assert!(Client::new()
        .session_update(&json!({"update":{}}))
        .unwrap()
        .is_none());
    let params = json!({"sessionId":"peer-1","update":{"sessionUpdate":"usage_update","used":12,"size":100,"extension":{"opaque":true}}});
    let update = client.session_update(&params).unwrap().unwrap();
    assert_eq!(update.kind, UpdateKind::UsageUpdate);
    assert_eq!(update.update, &params["update"]);
    assert!(update.update.get("cost").is_none());
    let unknown =
        json!({"sessionId":"peer-1","update":{"sessionUpdate":"future_update","opaque":[1,2]}});
    let update = client.session_update(&unknown).unwrap().unwrap();
    assert_eq!(update.kind, UpdateKind::Other);
    assert_eq!(update.update, &unknown["update"]);
}

#[test]
fn shared_acp_authority_frames_refuse_duplicate_keys_and_ambiguous_shapes() {
    for line in [
        r#"{"jsonrpc":"2.0","id":1,"result":{},"result":{"stopReason":"end_turn"}}"#,
        r#"{"jsonrpc":"2.0","id":"p","method":"session/request_permission","params":{"toolCall":{"rawInput":{"command":"a","command":"b"}}}}"#,
        r#"{"jsonrpc":"2.0","id":1,"result":{},"error":{}}"#,
        r#"{"jsonrpc":"2.0","id":1,"method":"session/update","result":{}}"#,
        r#"{"jsonrpc":"2.0","id":1.5,"method":"session/request_permission"}"#,
        r#"{"jsonrpc":"1.0","id":1,"result":{}}"#,
        r#"{"jsonrpc":"2.0","id":1"#,
    ] {
        assert!(
            matches!(classify_line(line), Frame::Unrecognized(_)),
            "{line}"
        );
    }
    for id in [json!("permission-a"), json!(-7), json!(9)] {
        let value = json!({"jsonrpc":"2.0","id":id,"method":"session/request_permission","params":{"extension":true}});
        match classify_line(&value.to_string()) {
            Frame::Request {
                id: echoed, raw, ..
            } => {
                assert_eq!(echoed, id);
                assert_eq!(raw, value);
            }
            other => panic!("identity lost: {other:?}"),
        }
    }
    assert!(matches!(
        classify_line(r#"{"jsonrpc":"2.0","id":3,"error":{"code":-1}}"#),
        Frame::Response {
            id: 3,
            outcome: RpcOutcome::Error(_)
        }
    ));
}

#[tokio::test]
async fn shared_acp_strict_stream_rejects_invalid_utf8_and_oversized_lines() {
    use kranz_acp::io::{BoundedLines, STDOUT_LINE_CAP};
    for bytes in [vec![0xff, b'\n'], vec![b'x'; STDOUT_LINE_CAP + 1]] {
        let mut lines = BoundedLines::new_strict(bytes.as_slice());
        assert_eq!(
            lines.next_line().await.unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }
    assert!(encode_message(&json!({"text":"x".repeat(STDOUT_LINE_CAP)})).is_err());
}
