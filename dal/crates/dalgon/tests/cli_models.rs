//! Binary-boundary tests for the model catalog command.

mod support;

use std::{
    error::Error,
    io::{Read, Write},
    net::TcpListener,
    thread,
};

use support::CliFixture;

#[test]
fn json_models_fetches_from_loopback_and_filters_catalog() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    let server = thread::spawn(move || -> std::io::Result<String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "no model-list request reached the loopback server",
                        ));
                    }
                    thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => return Err(error),
            }
        };
        // Accepted sockets inherit O_NONBLOCK on BSD/macOS; the reads below
        // must block or the request poll can surface EAGAIN before the
        // client's bytes arrive.
        stream.set_nonblocking(false)?;
        let mut request = [0_u8; 4096];
        let count = stream.read(&mut request)?;
        let recorded = String::from_utf8_lossy(&request[..count]).into_owned();
        let body = br#"{"data":[{"id":"local-test"}]}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )?;
        stream.write_all(body)?;
        Ok(recorded)
    });
    fixture.write_config(&format!(
        "[providers.openai]\nbase_url = \"http://{address}/v1\"\n"
    ))?;
    fixture.write_auth(r#"{"openai":{"kind":"api_key","key":"secret-model-token"}}"#)?;

    let output = fixture.output(&["models", "--json", "local-test"])?;
    let request = server
        .join()
        .map_err(|_| std::io::Error::other("loopback model server panicked"))??;

    assert!(request.starts_with("GET /v1/models "));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer")
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        output.stdout,
        b"[{\"provider\":\"openai\",\"id\":\"local-test\",\"context\":null}]\n"
    );
    assert_eq!(output.stderr, [] as [u8; 0]);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("secret-model-token"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("secret-model-token"));
    Ok(())
}

#[test]
fn fetch_failure_keeps_builtin_rows_and_uses_catalog_diagnostic() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    drop(listener);
    fixture.write_config(&format!(
        "[retry]\nrequest_max_retries = 0\n[providers.openai]\nbase_url = \"http://{address}/v1\"\n"
    ))?;
    fixture.write_auth(r#"{"openai":{"kind":"api_key","key":"secret-model-token"}}"#)?;

    let output = fixture.output(&["models"])?;
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout.lines().any(|line| line.starts_with("openai ")));
    let mut lines = stderr.lines();
    assert!(
        lines
            .next()
            .unwrap_or_default()
            .starts_with("dalgon: could not fetch the model list: "),
        "{stderr}"
    );
    assert_eq!(
        lines.next(),
        Some("Check the network and sign in with dalgon login PROVIDER, then try again.")
    );
    assert_eq!(lines.next(), None);
    Ok(())
}

#[cfg(unix)]
#[test]
fn insecure_auth_permissions_keep_the_auth_repair_diagnostic() -> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::PermissionsExt as _;

    let fixture = CliFixture::new()?;
    fixture.write_auth(r#"{"openai":{"kind":"api_key","key":"secret-model-token"}}"#)?;
    let path = fixture.auth_file();
    let mut permissions = std::fs::metadata(&path)?.permissions();
    permissions.set_mode(0o644);
    std::fs::set_permissions(&path, permissions)?;

    let output = fixture.output(&["models"])?;

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert_eq!(
        output.stderr,
        format!(
            "dalgon: auth.json has group or other permissions\nRun chmod 600 {}.\n",
            path.display()
        )
        .into_bytes()
    );
    Ok(())
}

/// Serves one model-list response, then closes.
type ModelServer = (std::net::SocketAddr, thread::JoinHandle<std::io::Result<()>>);

fn serve_models_once(body: Vec<u8>) -> Result<ModelServer, Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    let server = thread::spawn(move || -> std::io::Result<()> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "no model-list request reached the loopback server",
                        ));
                    }
                    thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => return Err(error),
            }
        };
        stream.set_nonblocking(false)?;
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request)?;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )?;
        stream.write_all(&body)?;
        Ok(())
    });
    Ok((address, server))
}

/// Every character outside a line break must be visible terminal text.
fn assert_visible_text(output: &str, what: &str) {
    assert!(
        output
            .chars()
            .all(|character| character == '\n' || !character.is_control()),
        "{what} leaks a control character: {output:?}"
    );
    assert!(
        output.contains(char::REPLACEMENT_CHARACTER),
        "{what} shows no sanitized text: {output:?}"
    );
}

const POISONED_IDS: &[u8] = br#"{"data":[{"id":"clean-model"},{"id":"new\nline\u001b[2Jtail"},{"id":"osc\u001b]52;;c2VjcmV0\u0007clip"}]}"#;

#[test]
fn poisoned_live_ids_render_as_visible_text_in_the_table() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let (address, server) = serve_models_once(POISONED_IDS.to_vec())?;
    fixture.write_config(&format!(
        "[providers.openai]\nbase_url = \"http://{address}/v1\"\n"
    ))?;
    fixture.write_auth(r#"{"openai":{"kind":"api_key","key":"secret-model-token"}}"#)?;

    let output = fixture.output(&["models"])?;
    server
        .join()
        .map_err(|_| std::io::Error::other("loopback model server panicked"))??;
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout)?;
    assert_visible_text(&stdout, "the model table");
    assert!(stdout.contains("clean-model"), "{stdout}");
    assert!(stdout.contains("new�line�[2Jtail"), "{stdout}");
    assert!(stdout.contains("osc�]52;;c2VjcmV0�clip"), "{stdout}");
    assert_eq!(output.stderr, [] as [u8; 0]);
    Ok(())
}

#[test]
fn poisoned_live_ids_stay_sanitized_in_json_output() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let (address, server) = serve_models_once(POISONED_IDS.to_vec())?;
    fixture.write_config(&format!(
        "[providers.openai]\nbase_url = \"http://{address}/v1\"\n"
    ))?;
    fixture.write_auth(r#"{"openai":{"kind":"api_key","key":"secret-model-token"}}"#)?;

    let output = fixture.output(&["models", "--json"])?;
    server
        .join()
        .map_err(|_| std::io::Error::other("loopback model server panicked"))??;
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout)?;
    assert_visible_text(&stdout, "the JSON model list");
    assert!(stdout.contains("\"id\":\"clean-model\""), "{stdout}");
    Ok(())
}

#[test]
fn poisoned_cached_rows_are_sanitized_when_the_live_list_fails() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let (address, server) = serve_models_once(POISONED_IDS.to_vec())?;
    fixture.write_config(&format!(
        "[providers.openai]\nbase_url = \"http://{address}/v1\"\n"
    ))?;
    fixture.write_auth(r#"{"openai":{"kind":"api_key","key":"secret-model-token"}}"#)?;

    let seeded = fixture.output(&["models"])?;
    server
        .join()
        .map_err(|_| std::io::Error::other("loopback model server panicked"))??;
    assert_eq!(seeded.status.code(), Some(0));
    let cache = fixture.data.join("dal").join("cache").join("models.json");
    let mut cached = String::from_utf8(std::fs::read(&cache)?)?;
    assert!(cached.contains(char::REPLACEMENT_CHARACTER), "{cached}");
    cached = cached.replace(char::REPLACEMENT_CHARACTER, "\\u001b");
    std::fs::write(&cache, cached.as_bytes())?;

    let closed = TcpListener::bind("127.0.0.1:0")?;
    let closed_address = closed.local_addr()?;
    drop(closed);
    fixture.write_config(&format!(
        "[retry]\nrequest_max_retries = 0\n[providers.openai]\nbase_url = \"http://{closed_address}/v1\"\n"
    ))?;
    let output = fixture.output(&["models"])?;
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout)?;
    assert_visible_text(&stdout, "the cached model table");
    assert!(stdout.contains("clean-model"), "{stdout}");
    assert!(
        std::fs::read(&cache)?
            .windows(6)
            .any(|window| window == b"\\u001b"),
        "the cache file keeps the poison the read path sanitized"
    );
    Ok(())
}
