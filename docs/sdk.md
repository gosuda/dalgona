# Embed dal in a Rust program

Link the library; `Config::default` and `Config::defaults`; register extensions; the five `Agent` operations, plus `ext_status`, `poll_status`, and `is_quiet` for extension status kinds (`Host::is_quiet` covers every open session); the `SessionRef` variants including ephemeral; the stability rule; the example `examples/sdk/main.rs` in full.

File-backed extensions use the session sidecar service for small private
values. Sidecar names are lowercase file names of up to 64 bytes; they may
contain letters, digits, `.`, `_`, and `-`, but cannot start with punctuation,
be exactly `.` or `..`, or include a path separator. Sidecar files stay inside
the owning extension's session directory and writes are atomic.

```rust
//! The smallest dal embedder: start a host, open one ephemeral session, print the answer.
use dal_agent::{ClientId, Env, Host, Product, SessionRef};
use dal_core::Workspace;
use dal_core::{Command, Config, Expect, Part};
use std::collections::BTreeMap;
use std::time::Duration;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()?;
    let workspace = Workspace::new(cwd.clone())?;
    let product = Product {
        name: "dal",
        data_root: cwd.clone(),
        defaults: "",
        extensions: Vec::new(),
        bundled: Vec::new(),
    };
    let env = Env { vars: BTreeMap::new(), cwd };
    let host = Host::start(product, Config::default(), env).await?;
    let agent = host
        .open(SessionRef::Ephemeral { workspace }, ClientId::new("sdk-example"))
        .await?;
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: "Reply with exactly one word: hello.".into() }],
        })
        .await?;
    let mut subscription = agent.subscribe(None)?;
    while subscription.next().await.is_some() {}
    let _report = host.shutdown(Duration::from_secs(5)).await;
    Ok(())
}
```
