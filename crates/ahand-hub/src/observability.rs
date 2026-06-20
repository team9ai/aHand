use std::borrow::Cow;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct HubSentryConfig {
    pub dsn: String,
    pub environment: Option<String>,
    pub release: Option<String>,
}

pub fn hub_sentry_config_from_env() -> Option<HubSentryConfig> {
    hub_sentry_config_from_lookup(|key| std::env::var(key).ok())
}

pub fn hub_sentry_config_from_lookup<F>(lookup: F) -> Option<HubSentryConfig>
where
    F: Fn(&str) -> Option<String>,
{
    let dsn = non_empty(lookup("SENTRY_DSN"))?;
    let environment = non_empty(lookup("SENTRY_ENVIRONMENT"));
    let release = non_empty(lookup("SENTRY_RELEASE")).or_else(|| non_empty(lookup("GIT_SHA")));

    Some(HubSentryConfig {
        dsn,
        environment,
        release,
    })
}

pub fn init_sentry(config: Option<HubSentryConfig>) -> Option<sentry::ClientInitGuard> {
    let config = config?;
    let release = config
        .release
        .map(Cow::Owned)
        .or_else(|| sentry::release_name!());

    Some(sentry::init((
        config.dsn,
        sentry::ClientOptions {
            release,
            environment: config.environment.map(Cow::Owned),
            send_default_pii: false,
            ..Default::default()
        },
    )))
}

fn non_empty(value: Option<String>) -> Option<String> {
    let value = value?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(values: HashMap<&'static str, &'static str>) -> impl Fn(&str) -> Option<String> {
        move |key| values.get(key).map(|value| (*value).to_owned())
    }

    #[test]
    fn empty_dsn_disables_sentry() {
        let values = HashMap::from([("SENTRY_DSN", "")]);

        let config = hub_sentry_config_from_lookup(lookup(values));

        assert_eq!(config, None);
    }

    #[test]
    fn reads_dsn_environment_and_release() {
        let values = HashMap::from([
            ("SENTRY_DSN", "https://public@example.invalid/1"),
            ("SENTRY_ENVIRONMENT", "production"),
            ("SENTRY_RELEASE", "abc123"),
            ("GIT_SHA", "ignored"),
        ]);

        let config = hub_sentry_config_from_lookup(lookup(values)).expect("config");

        assert_eq!(config.dsn, "https://public@example.invalid/1");
        assert_eq!(config.environment.as_deref(), Some("production"));
        assert_eq!(config.release.as_deref(), Some("abc123"));
    }

    #[test]
    fn falls_back_to_git_sha_for_release() {
        let values = HashMap::from([
            ("SENTRY_DSN", "https://public@example.invalid/1"),
            ("SENTRY_RELEASE", ""),
            ("GIT_SHA", "git-sha"),
        ]);

        let config = hub_sentry_config_from_lookup(lookup(values)).expect("config");

        assert_eq!(config.release.as_deref(), Some("git-sha"));
    }
}
