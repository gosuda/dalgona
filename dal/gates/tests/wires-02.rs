#![expect(
    clippy::disallowed_methods,
    reason = "SC test exercises real server commands"
)]

//! Public serve requires the owner token and `force` to replace sessions.
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{error::Error, fs, process::Command};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use support::{TestDir, dalgon_binary};

#[tokio::test]
async fn public_serve_requires_owner_only_token_and_force_to_replace()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    let data_home = home.join(".local/share");
    let token = data_home.join("dal/serve.token");
    fs::create_dir_all(&workspace)?;
    let binary = dalgon_binary("dalgon")?;
    let missing = Command::new(binary)
        .current_dir(&workspace)
        .env_clear()
        .envs(support::captured_shell_vars())
        .env("HOME", &home)
        .env("XDG_DATA_HOME", &data_home)
        .env("NO_COLOR", "1")
        .args([
            "serve",
            "--bind",
            "0.0.0.0",
            "--port",
            "0",
            "--public",
            "--token-file",
        ])
        .arg(&token)
        .output()?;
    assert!(!missing.status.success());
    assert!(!token.exists());

    let created = Command::new(binary)
        .current_dir(&workspace)
        .env_clear()
        .envs(support::captured_shell_vars())
        .env("HOME", &home)
        .env("XDG_DATA_HOME", &data_home)
        .args(["serve", "token"])
        .output()?;
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let original = fs::read(&token)?;
    #[cfg(unix)]
    assert_eq!(fs::metadata(&token)?.permissions().mode() & 0o777, 0o600);
    #[cfg(windows)]
    {
        // `[System.IO.File]::GetAccessControl` needs no module autoload —
        // `Get-Acl` lives in `Microsoft.PowerShell.Security`, which fails
        // to load on runners without a complete `PSModulePath`.
        let acl_check = r"
$acl = [System.IO.File]::GetAccessControl($env:DALGON_TOKEN_PATH)
if (-not $acl.AreAccessRulesProtected) { exit 10 }
$owner = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$allowed = @($acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]) |
  Where-Object { $_.AccessControlType -eq [System.Security.AccessControl.AccessControlType]::Allow } |
  ForEach-Object { $_.IdentityReference.Value })
$privileged = @($owner, 'S-1-5-18', 'S-1-5-32-544')
if (-not $allowed.Contains($owner)) { exit 11 }
if (@($allowed | Where-Object { $_ -notin $privileged }).Count -ne 0) { exit 12 }
";
        // Runner images prepend PowerShell 7 module dirs to
        // PSModulePath; inbox 5.1 then finds the PS7 manifest for
        // Microsoft.PowerShell.Security and cannot load its managed
        // assembly. Put the inbox modules dir first so autoload resolves
        // the matching manifest.
        let module_path = format!(
            r"C:\Windows\system32\WindowsPowerShell\v1.0\Modules;{}",
            std::env::var("PSModulePath").unwrap_or_default()
        );
        let result = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", acl_check])
            .env("PSModulePath", module_path)
            .env("DALGON_TOKEN_PATH", &token)
            .output()?;
        assert!(
            result.status.success(),
            "token ACL did not grant only the current user and privileged system accounts: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    let refused = Command::new(binary)
        .current_dir(&workspace)
        .env_clear()
        .envs(support::captured_shell_vars())
        .env("HOME", &home)
        .env("XDG_DATA_HOME", &data_home)
        .args(["serve", "token"])
        .output()?;
    assert!(!refused.status.success());
    assert_eq!(fs::read(&token)?, original);

    let forced = Command::new(binary)
        .current_dir(&workspace)
        .env_clear()
        .envs(support::captured_shell_vars())
        .env("HOME", &home)
        .env("XDG_DATA_HOME", &data_home)
        .args(["serve", "token", "--force"])
        .output()?;
    assert!(
        forced.status.success(),
        "{}",
        String::from_utf8_lossy(&forced.stderr)
    );
    assert_ne!(fs::read(&token)?, original);
    Ok(())
}
