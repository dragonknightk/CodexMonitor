#[allow(dead_code, reason = "the desktop build script consumes this name")]
pub const ENVIRONMENT: &str = "CODEX_MONITOR_BUILD_CHANNEL";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildChannel {
    Production,
    UnsignedDevelopment,
    LocalDevelopment,
}

impl BuildChannel {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "production" => Some(Self::Production),
            "unsigned-development" => Some(Self::UnsignedDevelopment),
            "local-development" => Some(Self::LocalDevelopment),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Production => "production",
            Self::UnsignedDevelopment => "unsigned-development",
            Self::LocalDevelopment => "local-development",
        }
    }

    pub const fn app_user_model_id(self) -> &'static str {
        match self {
            Self::Production => "dev.codexmonitor.desktop",
            Self::UnsignedDevelopment => "dev.codexmonitor.desktop.unsigned-development",
            Self::LocalDevelopment => "dev.codexmonitor.desktop.local-development",
        }
    }

    pub const fn shortcut_name(self) -> &'static str {
        match self {
            Self::Production => "CodexMonitor.lnk",
            Self::UnsignedDevelopment => "CodexMonitor (Unsigned Development).lnk",
            Self::LocalDevelopment => "CodexMonitor (Local Development).lnk",
        }
    }

    pub fn current() -> Self {
        option_env!("CODEX_MONITOR_BUILD_CHANNEL")
            .and_then(Self::parse)
            .expect("build.rs must embed a supported CodexMonitor build channel")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_channels_are_closed_and_have_distinct_windows_identities() {
        let channels = [
            BuildChannel::Production,
            BuildChannel::UnsignedDevelopment,
            BuildChannel::LocalDevelopment,
        ];
        for channel in channels {
            assert_eq!(BuildChannel::parse(channel.as_str()), Some(channel));
            assert!(!channel.app_user_model_id().contains(' '));
            assert!(channel.app_user_model_id().len() <= 128);
            assert!(
                std::path::Path::new(channel.shortcut_name())
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("lnk"))
            );
        }
        assert_eq!(BuildChannel::parse("production-like"), None);
        assert_eq!(BuildChannel::parse(""), None);
        assert_ne!(
            channels[0].app_user_model_id(),
            channels[1].app_user_model_id()
        );
        assert_ne!(
            channels[0].app_user_model_id(),
            channels[2].app_user_model_id()
        );
        assert_ne!(channels[1].shortcut_name(), channels[2].shortcut_name());
    }
}
