use crate::model::{ChannelConfig, ChannelExecutorLocator};
use reqwest::{
    RequestBuilder,
    header::{HeaderMap, HeaderValue, USER_AGENT},
};

pub(crate) const PROFILE_CLAUDE_CODE: &str = "claude_code";
pub(crate) const PROFILE_CODEX: &str = "codex";
pub(crate) const PROFILE_OPENCODE: &str = "opencode";

// Keep the explicit profile used by user-configured API/custom channels
// stable. OpenCode Console is a separate subscription adapter and identifies
// as the captured first-party client instead.
pub(crate) const OPENCODE_COMPAT_USER_AGENT: &str = "opencode/1.18.31";

pub(crate) fn value(channel: &ChannelConfig) -> Option<String> {
    if !matches!(channel.v2.executor, ChannelExecutorLocator::HttpSurface) {
        return None;
    }
    if channel.source_driver() == crate::source_driver::SourceDriverId::OpencodeConsole {
        // Console speaks for the official OpenCode client. The full header
        // identity is completed by `opencode_client::apply_headers`.
        return Some(crate::opencode_client::USER_AGENT_VALUE.into());
    }
    match channel.v2.user_agent_profile.trim() {
        PROFILE_CLAUDE_CODE => Some(crate::claude_client_profile::CLAUDE_CODE_USER_AGENT.into()),
        PROFILE_CODEX => Some(crate::codex_identity::active_codex_identity().user_agent),
        PROFILE_OPENCODE => Some(OPENCODE_COMPAT_USER_AGENT.into()),
        _ => None,
    }
}

pub(crate) fn apply_to_headers(channel: &ChannelConfig, headers: &mut HeaderMap) {
    let Some(value) = value(channel) else {
        if let Some(value) = crate::coding_gateway::default_user_agent(channel.source_driver()) {
            headers
                .entry(USER_AGENT)
                .or_insert(HeaderValue::from_static(value));
        }
        return;
    };
    headers.insert(
        USER_AGENT,
        HeaderValue::from_str(&value).expect("channel User-Agent profiles are valid headers"),
    );
}

pub(crate) fn apply_to_request(channel: &ChannelConfig, request: RequestBuilder) -> RequestBuilder {
    match value(channel) {
        Some(value) => request.header(USER_AGENT, value),
        None => match crate::coding_gateway::default_user_agent(channel.source_driver()) {
            Some(value) => request.header(USER_AGENT, value),
            None => request,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{channel_from_supplier, default_supplier_config, source_driver::SourceDriverId};

    #[test]
    fn empty_profile_preserves_the_callers_user_agent() {
        let channel = channel_from_supplier("http".into(), &default_supplier_config());
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static("caller/7"));

        apply_to_headers(&channel, &mut headers);

        assert_eq!(headers[USER_AGENT], "caller/7");
    }

    #[test]
    fn selected_profile_overrides_the_callers_user_agent() {
        let mut channel = channel_from_supplier("http".into(), &default_supplier_config());
        channel.v2.user_agent_profile = PROFILE_OPENCODE.into();
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static("caller/7"));

        apply_to_headers(&channel, &mut headers);

        assert_eq!(headers[USER_AGENT], OPENCODE_COMPAT_USER_AGENT);
    }

    #[test]
    fn opencode_console_speaks_as_the_official_client() {
        let mut channel = channel_from_supplier("http".into(), &default_supplier_config());
        channel.set_source_driver(SourceDriverId::OpencodeConsole);
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static("caller/7"));

        apply_to_headers(&channel, &mut headers);

        assert_eq!(
            headers[USER_AGENT],
            crate::opencode_client::USER_AGENT_VALUE
        );
        assert_eq!(
            value(&channel).as_deref(),
            Some(crate::opencode_client::USER_AGENT_VALUE)
        );
    }

    #[test]
    fn retained_subscriptions_ignore_the_profile() {
        let mut channel = channel_from_supplier("subscription".into(), &default_supplier_config());
        channel.set_source_driver(SourceDriverId::OpenAiSubscription);
        channel.v2.user_agent_profile = PROFILE_OPENCODE.into();

        assert_eq!(value(&channel), None);
    }
}
