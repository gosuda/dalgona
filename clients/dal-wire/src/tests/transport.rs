use futures::{StreamExt, stream};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

use crate::transport::{MAX_FRAME_BYTES, ReadFrameError, StdioTransport, Transport};

#[tokio::test]
async fn cr_blank_final_frame_reads_each_nonempty_line() {
    let (mut input, reader) = tokio::io::duplex(32);
    input.write_all(b"a\r\nb\n\nc").await.expect("write input");
    drop(input);
    let mut transport = Transport::Stdio(StdioTransport::new(reader, tokio::io::sink()));

    assert_eq!(transport.read_frame().await, Ok("a".to_owned()));
    assert_eq!(transport.read_frame().await, Ok("b".to_owned()));
    assert_eq!(transport.read_frame().await, Ok("c".to_owned()));
    assert!(matches!(
        transport.read_frame().await,
        Err(ReadFrameError::EndOfInput)
    ));
}

#[tokio::test]
async fn eof_half_closes_reads_without_closing_reply_writes() {
    let (server, client) = tokio::io::duplex(64);
    let (server_reader, server_writer) = tokio::io::split(server);
    let (client_reader, mut client_writer) = tokio::io::split(client);
    let mut client_reader = tokio::io::BufReader::new(client_reader);
    let mut transport = Transport::Stdio(StdioTransport::new(server_reader, server_writer));
    let request = r#"{"id":1}"#;
    client_writer
        .write_all(request.as_bytes())
        .await
        .expect("send request");
    client_writer
        .write_all(b"\n")
        .await
        .expect("terminate request");
    client_writer
        .shutdown()
        .await
        .expect("half-close request stream");

    assert_eq!(transport.read_frame().await, Ok(request.to_owned()));
    assert!(matches!(
        transport.read_frame().await,
        Err(ReadFrameError::EndOfInput)
    ));
    let response = r#"{"id":1,"result":{}}"#;
    transport
        .write_frame(response)
        .await
        .expect("reply remains writable after request EOF");

    let mut reply = String::new();
    client_reader
        .read_line(&mut reply)
        .await
        .expect("read reply");
    assert_eq!(reply, format!("{response}\n"));
}

#[tokio::test]
async fn chunked_read_uses_only_lf_and_one_terminal_cr() {
    let wire = "a\u{2028}b\r\nc\u{2029}d\r\r\n\nlast".as_bytes().to_vec();
    let (mut input, reader) = tokio::io::duplex(1);
    let writer = async move {
        for byte in wire.chunks(1) {
            input.write_all(byte).await.expect("send one input byte");
        }
    };
    let mut transport = Transport::Stdio(StdioTransport::new(reader, tokio::io::sink()));
    let reader = async {
        let mut frames = Vec::new();
        loop {
            match transport.read_frame().await {
                Ok(frame) => frames.push(frame),
                Err(ReadFrameError::EndOfInput) => break,
                Err(error) => panic!("unexpected frame read error: {error}"),
            }
        }
        frames
    };
    let ((), frames) = tokio::join!(writer, reader);
    assert_eq!(frames, ["a\u{2028}b", "c\u{2029}d\r", "last"]);
}

#[tokio::test]
async fn oversized_frame_closes_the_transport() {
    let bytes = vec![b'x'; MAX_FRAME_BYTES + 1];
    let reader = std::io::Cursor::new(bytes);
    let mut transport = Transport::Stdio(StdioTransport::new(reader, tokio::io::sink()));

    assert!(matches!(
        transport.read_frame().await,
        Err(ReadFrameError::FrameTooLarge(MAX_FRAME_BYTES))
    ));
    assert!(matches!(
        transport.read_frame().await,
        Err(ReadFrameError::Closed)
    ));
}

#[tokio::test]
async fn invalid_utf8_consumes_only_its_frame() {
    let (mut input, reader) = tokio::io::duplex(16);
    input.write_all(b"\xff\n{}\n").await.expect("write input");
    drop(input);
    let mut transport = Transport::Stdio(StdioTransport::new(reader, tokio::io::sink()));

    assert!(matches!(
        transport.read_frame().await,
        Err(ReadFrameError::InvalidUtf8)
    ));
    assert_eq!(transport.read_frame().await, Ok("{}".to_owned()));
}

#[tokio::test]
async fn concurrent_writes_preserve_whole_lines() {
    const WORKERS: usize = 200;
    const FRAMES_PER_WORKER: usize = 50;
    const PAYLOAD_BYTES: usize = 990;

    let (writer_side, reader_side) = tokio::io::duplex(64 * 1024);
    let stdio = StdioTransport::new(tokio::io::empty(), writer_side);
    let writer = stdio.writer();
    let send = stream::iter(0..WORKERS).for_each_concurrent(Some(WORKERS), |worker| {
        let writer = writer.clone();
        async move {
            for index in 0..FRAMES_PER_WORKER {
                let payload = "x".repeat(PAYLOAD_BYTES);
                let frame = format!("{worker:03}:{index:03}:{payload}");
                writer.write_frame(&frame).await.expect("whole frame write");
            }
        }
    });
    let receive = async move {
        let mut reader = tokio::io::BufReader::new(reader_side);
        let mut seen = vec![false; WORKERS * FRAMES_PER_WORKER];
        for _ in 0..seen.len() {
            let mut line = String::new();
            let bytes = reader
                .read_line(&mut line)
                .await
                .expect("read complete line");
            assert!(bytes > 0 && line.ends_with('\n'));
            line.pop();
            let mut fields = line.splitn(3, ':');
            let worker = fields
                .next()
                .and_then(|value| value.parse::<usize>().ok())
                .expect("worker id");
            let index = fields
                .next()
                .and_then(|value| value.parse::<usize>().ok())
                .expect("frame id");
            let payload = fields.next().expect("payload");
            assert_eq!(payload.len(), PAYLOAD_BYTES);
            let slot = worker * FRAMES_PER_WORKER + index;
            assert!(!seen[slot], "duplicate frame {worker}:{index}");
            seen[slot] = true;
        }
        assert!(seen.into_iter().all(|received| received));
    };
    let ((), ()) = tokio::join!(send, receive);
}

#[tokio::test]
async fn reset_closes_all_later_writes() {
    let (writer_side, reader_side) = tokio::io::duplex(16);
    let stdio = StdioTransport::new(tokio::io::empty(), writer_side);
    let writer = stdio.writer();
    drop(reader_side);

    let first = writer
        .write_frame("first")
        .await
        .expect_err("reset must fail");
    assert_eq!(first.to_string(), "transport error: connection is closed");
    let second = writer
        .write_frame("second")
        .await
        .expect_err("closed writer stays closed");
    assert_eq!(second.to_string(), "transport error: connection is closed");
}

#[tokio::test]
async fn line_delimiter_is_rejected_before_writing() {
    let (writer_side, mut reader_side) = tokio::io::duplex(32);
    let stdio = StdioTransport::new(tokio::io::empty(), writer_side);
    let writer = stdio.writer();
    assert_eq!(
        writer.write_frame("a\nb").await.unwrap_err().to_string(),
        "invalid wire frame"
    );
    writer
        .write_frame("valid")
        .await
        .expect("valid frame writes");
    let mut line = [0; 6];
    reader_side
        .read_exact(&mut line)
        .await
        .expect("read one line");
    assert_eq!(&line, b"valid\n");
}

#[tokio::test]
async fn websocket_oversized_message_ends_without_a_close_frame() {
    use futures::SinkExt;
    use tokio_tungstenite::{
        WebSocketStream,
        tungstenite::{Message, protocol::Role},
    };

    use crate::transport::WebSocketTransport;

    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let mut server = WebSocketTransport::server(server_io).await;
    let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
    let oversized = Message::Binary(vec![0_u8; 16 * 1024 * 1024 + 1].into());

    let send = async {
        // The server stops reading at the size limit, so this send ends in a write error.
        let _ = client.send(oversized).await;
    };
    let serve = async move {
        assert_eq!(server.read_frame().await, Err(ReadFrameError::Closed));
        server.close().await;
    };
    tokio::join!(send, serve);

    match client.next().await {
        None | Some(Err(_)) => {}
        Some(Ok(frame)) => panic!("an oversized message must end without a reply, got {frame:?}"),
    }
}
