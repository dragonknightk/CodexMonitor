use std::ffi::OsStr;
use std::io::{self, Write as _};

use base64::Engine as _;

const INSTALLED_SMOKE_ENVIRONMENT: &str = "CODEX_MONITOR_INSTALLED_SMOKE";
const LINUX_DEGRADED_ENVIRONMENT: &str = "CODEX_MONITOR_INSTALLED_SMOKE_LINUX_DEGRADED";
pub(crate) const DASHBOARD_LAUNCH_PREFIX: &str = "CODEX_MONITOR_INSTALLED_SMOKE_DASHBOARD:";

/// Send a one-time Dashboard URL to the parent smoke process without writing
/// the bootstrap credential to disk or ordinary application logs.
pub(crate) fn capture_dashboard_launch(url: &str) -> io::Result<bool> {
    if !enabled() {
        return Ok(false);
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    write_dashboard_launch(&mut writer, url)?;
    writer.flush()?;
    Ok(true)
}

/// Deterministically exercise the documented Linux capability-degradation path
/// from an installed package. The switch is ignored outside the explicit smoke
/// process and outside Linux.
pub(crate) fn force_linux_degradation() -> bool {
    cfg!(target_os = "linux") && enabled() && environment_is_one(LINUX_DEGRADED_ENVIRONMENT)
}

fn enabled() -> bool {
    environment_is_one(INSTALLED_SMOKE_ENVIRONMENT)
}

fn environment_is_one(name: &str) -> bool {
    std::env::var_os(name).as_deref() == Some(OsStr::new("1"))
}

fn write_dashboard_launch(writer: &mut impl io::Write, url: &str) -> io::Result<()> {
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(url);
    writeln!(writer, "{DASHBOARD_LAUNCH_PREFIX}{encoded}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_credential_uses_the_dedicated_pipe_record() {
        let url = "http://127.0.0.1:4319/api/v1/session/bootstrap?token=synthetic-secret";
        let mut output = Vec::new();

        write_dashboard_launch(&mut output, url).unwrap();

        let record = String::from_utf8(output).unwrap();
        assert!(record.starts_with(DASHBOARD_LAUNCH_PREFIX));
        assert!(!record.contains("synthetic-secret"));
        let encoded = record
            .trim_end()
            .strip_prefix(DASHBOARD_LAUNCH_PREFIX)
            .unwrap();
        assert_eq!(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(encoded)
                .unwrap(),
            url.as_bytes()
        );
    }
}
