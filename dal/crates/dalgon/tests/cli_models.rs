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
