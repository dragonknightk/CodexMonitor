use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

pub const BACKGROUND_ARGUMENT: &str = "--background";
pub const IMPORT_LEGACY_ARGUMENT: &str = "--import-legacy";
pub const QUIT_ARGUMENT: &str = "--quit";

/// 打开任何常驻资源前选定的启动行为。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StartupMode {
    Manual,
    Background,
    ImportLegacy(PathBuf),
    Quit,
}

/// 单实例通道允许转发的闭合命令。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ForwardedLaunchCommand {
    OpenDashboard,
    IgnoreBackgroundLaunch,
    ImportLegacy(PathBuf),
    Quit,
}

/// 不保留不可信参数原文的稳定失败代码。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupArgumentError {
    MissingExecutable,
    InvalidLegacySource,
    UnknownOrRepeatedArgument,
}

impl StartupMode {
    /// 在启动任何常驻资源前解析进程参数。
    ///
    /// # Errors
    ///
    /// 参数缺少可执行文件、包含未知或重复选项，或 `--import-legacy` 缺少绝对路径时，
    /// 返回稳定的 [`StartupArgumentError`]。
    pub fn parse_process_args<I, S>(args: I) -> Result<Self, StartupArgumentError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let mut args = args.into_iter();
        if args.next().is_none() {
            return Err(StartupArgumentError::MissingExecutable);
        }

        let Some(argument) = args.next().map(Into::into) else {
            return Ok(Self::Manual);
        };
        if argument == OsStr::new(BACKGROUND_ARGUMENT) {
            no_trailing_args(args).map(|()| Self::Background)
        } else if argument == OsStr::new(QUIT_ARGUMENT) {
            no_trailing_args(args).map(|()| Self::Quit)
        } else if argument == OsStr::new(IMPORT_LEGACY_ARGUMENT) {
            let Some(source) = args.next().map(Into::into).map(PathBuf::from) else {
                return Err(StartupArgumentError::InvalidLegacySource);
            };
            if args.next().is_some() || !source.is_absolute() {
                return Err(StartupArgumentError::InvalidLegacySource);
            }
            Ok(Self::ImportLegacy(source))
        } else {
            Err(StartupArgumentError::UnknownOrRepeatedArgument)
        }
    }
}

fn no_trailing_args(mut args: impl Iterator) -> Result<(), StartupArgumentError> {
    if args.next().is_none() {
        Ok(())
    } else {
        Err(StartupArgumentError::UnknownOrRepeatedArgument)
    }
}

impl ForwardedLaunchCommand {
    /// 将第二实例的参数解析为允许转发的闭合命令。
    ///
    /// # Errors
    ///
    /// 参数不满足 [`StartupMode::parse_process_args`] 的闭合语法时，返回对应的
    /// [`StartupArgumentError`]。
    pub fn parse(args: Vec<String>) -> Result<Self, StartupArgumentError> {
        match StartupMode::parse_process_args(args)? {
            StartupMode::Manual => Ok(Self::OpenDashboard),
            StartupMode::Background => Ok(Self::IgnoreBackgroundLaunch),
            StartupMode::ImportLegacy(source) => Ok(Self::ImportLegacy(source)),
            StartupMode::Quit => Ok(Self::Quit),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_background_import_and_quit_are_distinct() {
        let legacy = std::env::temp_dir().join("synthetic-legacy.sqlite3");
        assert_eq!(
            StartupMode::parse_process_args(["codex-monitor"]),
            Ok(StartupMode::Manual)
        );
        assert_eq!(
            StartupMode::parse_process_args(["codex-monitor", BACKGROUND_ARGUMENT]),
            Ok(StartupMode::Background)
        );
        assert_eq!(
            StartupMode::parse_process_args(["codex-monitor", QUIT_ARGUMENT]),
            Ok(StartupMode::Quit)
        );
        assert_eq!(
            StartupMode::parse_process_args([
                OsString::from("codex-monitor"),
                OsString::from(IMPORT_LEGACY_ARGUMENT),
                legacy.clone().into_os_string(),
            ]),
            Ok(StartupMode::ImportLegacy(legacy))
        );
    }

    #[test]
    fn forwarded_arguments_are_closed_and_reject_untrusted_shapes() {
        assert_eq!(
            ForwardedLaunchCommand::parse(vec!["codex-monitor".to_owned()]),
            Ok(ForwardedLaunchCommand::OpenDashboard)
        );
        for args in [
            vec![],
            vec![
                "codex-monitor".to_owned(),
                "--open-url=https://invalid".to_owned(),
            ],
            vec![
                "codex-monitor".to_owned(),
                BACKGROUND_ARGUMENT.to_owned(),
                BACKGROUND_ARGUMENT.to_owned(),
            ],
            vec![
                "codex-monitor".to_owned(),
                IMPORT_LEGACY_ARGUMENT.to_owned(),
                "relative.sqlite3".to_owned(),
            ],
        ] {
            assert!(ForwardedLaunchCommand::parse(args).is_err());
        }
    }
}
