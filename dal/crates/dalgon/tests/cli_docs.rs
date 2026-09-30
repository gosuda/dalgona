//! Binary-boundary tests for product manual listing and lookup.

mod support;

use std::{error::Error, fs};

use support::CliFixture;

#[test]
fn docs_lists_pages_reads_config_and_suggests_nearest_page() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    fixture.write_config("plugins = [\"broken\"]\n")?;
    let broken_plugin = fixture.data.join("dal/plugins/broken");
    fs::create_dir_all(&broken_plugin)?;
    fs::write(broken_plugin.join("plugin.star"), "def broken(:\n")?;

    let listing = fixture.output(&["docs"])?;
    assert_eq!(listing.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&listing.stdout).contains("dal://config"));
    assert!(listing.stderr.is_empty());

    let page = fixture.output(&["docs", "dal://config"])?;
    assert_eq!(page.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&page.stdout).starts_with("# Settings in dal.toml\n"));
    assert!(page.stderr.is_empty());

    let typo_uri = ["dal", "://", "confg"].concat();
    let miss = fixture.output(&["docs", &typo_uri])?;
    assert_eq!(miss.status.code(), Some(1));
    assert!(miss.stdout.is_empty());
    let expected = [
        "dalgon: no dal document at dal",
        "://",
        "confg\nDid you mean dal",
        "://config? Run dalgon docs for the list.\n",
    ]
    .concat();
    assert_eq!(miss.stderr, expected.as_bytes());
    Ok(())
}
