//! Provider-free second consumer: a std thread owns a small Tokio runtime.
//! It uses no kranz-engine, Sgian, process, credential or permission authority.
//! A duplex synthetic peer replaces a supervised child's pipes. Real embedding
//! still needs a supervisor and a nonblocking, bounded UI/control bridge.
use kranz_acp::{classify_line, encode_message, io::BoundedLines, Client, Frame, RpcOutcome};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

async fn write(writer: &mut (impl AsyncWrite + Unpin), message: Value) {
    writer
        .write_all(encode_message(&message).unwrap().as_bytes())
        .await
        .unwrap();
    writer.flush().await.unwrap();
}

async fn read(reader: &mut BoundedLines<impl AsyncRead + Unpin>) -> Frame {
    classify_line(&reader.next_line().await.unwrap().expect("peer frame"))
}

fn result(frame: Frame, expected: u64) -> Value {
    match frame {
        Frame::Response {
            id,
            outcome: RpcOutcome::Result(value),
        } if id == expected => value,
        other => panic!("unexpected response: {other:?}"),
    }
}

async fn synthetic_peer(stream: tokio::io::DuplexStream) {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut lines = BoundedLines::new_strict(reader);
    match read(&mut lines).await {
        Frame::Request {
            id, method, params, ..
        } => {
            assert_eq!(method, "initialize");
            assert_eq!(
                params["clientCapabilities"],
                json!({
                    "fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false,
                })
            );
            write(
                &mut writer,
                json!({"jsonrpc":"2.0", "id":id, "result":{"protocolVersion":1}}),
            )
            .await;
        }
        other => panic!("unexpected initialization: {other:?}"),
    }
    match read(&mut lines).await {
        Frame::Request { id, method, .. } => {
            assert_eq!(method, "session/new");
            write(
                &mut writer,
                json!({"jsonrpc":"2.0", "id":id, "result":{"sessionId":"acp-mock-session-1"}}),
            )
            .await;
        }
        other => panic!("unexpected session setup: {other:?}"),
    }
    assert!(
        matches!(read(&mut lines).await, Frame::Request {id, method, ..}
        if id == 3 && method == "session/prompt")
    );
    // Fragment every frame, including inside UTF-8/JSON tokens. The reader
    // must retain framing rather than treating each write as a notification.
    for bytes in kranz_acp::conformance::TURN.as_bytes().chunks(7) {
        writer.write_all(bytes).await.unwrap();
        tokio::task::yield_now().await;
    }
    assert!(
        matches!(read(&mut lines).await, Frame::Request {id, method, ..}
        if id == 4 && method == "session/prompt")
    );
    write(&mut writer, json!({"jsonrpc":"2.0", "id":"consent-1", "method":"session/request_permission",
        "params":{"sessionId":"acp-mock-session-1", "toolCall":{"toolCallId":"call-2","kind":"execute","rawInput":{"command":"false"}},
        "options":[{"optionId":"once","kind":"allow_once","name":"Allow once"}]}})).await;
    // The consumer cancels this synthetic request. It has no policy authority.
    assert!(matches!(read(&mut lines).await, Frame::Unrecognized(raw)
        if raw["id"] == "consent-1" && raw["result"]["outcome"]["outcome"] == "cancelled"));
    assert!(
        matches!(read(&mut lines).await, Frame::Notification {method, ..} if method == "session/cancel")
    );
    write(
        &mut writer,
        json!({"jsonrpc":"2.0","id":4,"result":{"stopReason":"cancelled"}}),
    )
    .await;
    assert!(
        lines.next_line().await.unwrap().is_none(),
        "consumer closes its pipe"
    );
}

async fn consumer(stream: tokio::io::DuplexStream) -> Vec<Value> {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut lines = BoundedLines::new_strict(reader);
    let mut client = Client::new();
    let request = client
        .initialize(json!({"name":"threaded-consumer-spike","version":"0"}))
        .unwrap();
    write(&mut writer, request.message).await;
    client
        .accept_initialize(request.id, &result(read(&mut lines).await, request.id))
        .unwrap();
    let request = client.new_session("/synthetic").unwrap();
    write(&mut writer, request.message).await;
    client
        .accept_session(request.id, &result(read(&mut lines).await, request.id))
        .unwrap();
    let request = client.prompt("fixture turn").unwrap();
    write(&mut writer, request.message).await;
    let mut raw_updates = Vec::new();
    loop {
        match read(&mut lines).await {
            Frame::Notification { params, raw, .. } => {
                let update = client.session_update(&params).unwrap().unwrap();
                assert_eq!(update.session_id, "acp-mock-session-1");
                raw_updates.push(raw);
            }
            Frame::Response {
                id,
                outcome: RpcOutcome::Result(value),
            } => {
                assert!(client.complete_prompt(id));
                assert_eq!(value["stopReason"], "end_turn");
                break;
            }
            other => panic!("unexpected turn frame: {other:?}"),
        }
    }
    let request = client.prompt("cancel fixture turn").unwrap();
    write(&mut writer, request.message).await;
    match read(&mut lines).await {
        Frame::Request {
            id, method, params, ..
        } => {
            assert_eq!(method, "session/request_permission");
            assert_eq!(params["sessionId"], client.session_id().unwrap());
            write(
                &mut writer,
                json!({"jsonrpc":"2.0","id":id,"result":{"outcome":{"outcome":"cancelled"}}}),
            )
            .await;
        }
        other => panic!("unexpected permission: {other:?}"),
    }
    write(&mut writer, client.cancel().unwrap()).await;
    assert!(
        client.prompt_in_flight(),
        "cancel is not completion or a kill"
    );
    let response = result(read(&mut lines).await, request.id);
    assert_eq!(response["stopReason"], "cancelled");
    assert!(client.complete_prompt(request.id));
    writer.shutdown().await.unwrap();
    raw_updates
}

fn run() -> Vec<Value> {
    std::thread::spawn(|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                let (left, right) = tokio::io::duplex(64);
                let (updates, ()) = tokio::join!(consumer(left), synthetic_peer(right));
                updates
            })
            .await
            .expect("bounded synthetic consumer")
        })
    })
    .join()
    .expect("consumer thread joined")
}

fn main() {
    let updates = run();
    assert_eq!(updates.len(), 5);
    println!("threaded ACP consumer: 5 raw updates, 2 turns, cancelled permission, joined thread; no provider calls");
}

#[test]
fn shared_acp_threaded_consumer_retains_the_common_fixture_and_cancels() {
    let expected: Vec<Value> = kranz_acp::conformance::TURN
        .lines()
        .take(5)
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(run(), expected);
}
