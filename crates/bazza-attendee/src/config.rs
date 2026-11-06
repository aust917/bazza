use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use reqwest::{header::HeaderValue, Url};
use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use tokio_rustls::TlsAcceptor;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("Use an HTTPS Attendee URL or an HTTP URL on this computer.")]
    InvalidApiUrl,
    #[error("Use a WSS URL for the Attendee audio callback.")]
    InvalidCallbackUrl,
    #[error("Use an IP address and port for the Attendee listener.")]
    InvalidBindAddress,
    #[error("Enter a valid Attendee API key.")]
    InvalidCredentials,
    #[error("The Attendee listener needs a matching PEM certificate and private key.")]
    InvalidCertificate,
    #[error("Use a Google Meet meeting link with a meeting code.")]
    InvalidMeetingUrl,
    #[error("Bazza could not start the Attendee audio listener.")]
    ListenerUnavailable,
    #[error("Bazza could not create the Attendee tracks directory.")]
    DirectoryUnavailable,
    #[error("Bazza could not start the Attendee recording task.")]
    RuntimeUnavailable,
    #[error("Attendee rejected the API key.")]
    Unauthorized,
    #[error("Attendee rejected the request to join the meeting.")]
    JoinRejected,
    #[error("Bazza could not confirm whether Attendee created the bot.")]
    JoinUnconfirmed,
    #[error("The Attendee bot could not join the meeting or start receiving audio.")]
    JoinFailed,
    #[error("The Attendee bot is not connected and available for this recording.")]
    NotReady,
    #[error("The prepared Attendee bot left because recording did not start in time.")]
    PrepareExpired,
    #[error("Bazza could not contact the Attendee service.")]
    ApiUnavailable,
    #[error("Bazza could not confirm that the Attendee bot left the meeting.")]
    LeaveUnconfirmed,
    #[error("Bazza could not save all Attendee tracks.")]
    SaveFailed,
    #[error("The Attendee recording reached the local track storage limit.")]
    TrackLimit,
}

pub struct Config {
    pub api_url: String,
    pub api_key: String,
    pub callback_url: String,
    pub bind_address: SocketAddr,
    pub certificate_path: PathBuf,
    pub private_key_path: PathBuf,
}

pub(crate) struct Validated {
    pub api: Url,
    pub authorization: HeaderValue,
    pub callback: Url,
    pub acceptor: TlsAcceptor,
}

fn without_credentials(url: &Url) -> bool {
    url.has_host() && url.username().is_empty() && url.password().is_none()
}

fn loopback(url: &Url) -> bool {
    match url.host_str() {
        Some("localhost") => true,
        Some(host) => host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback()),
        None => false,
    }
}

pub fn normalize_meeting_url(input: &str) -> Result<String, Error> {
    let url = Url::parse(input.trim()).map_err(|_| Error::InvalidMeetingUrl)?;
    if url.scheme() != "https"
        || url.host_str() != Some("meet.google.com")
        || !without_credentials(&url)
        || url.port().is_some()
    {
        return Err(Error::InvalidMeetingUrl);
    }
    let path = url.path().trim_end_matches('/');
    let code = path
        .strip_prefix("/meet/")
        .or_else(|| path.strip_prefix('/'))
        .ok_or(Error::InvalidMeetingUrl)?;
    let parts: Vec<_> = code.split('-').collect();
    if parts.iter().map(|part| part.len()).ne([3, 4, 3])
        || !parts
            .iter()
            .all(|part| part.bytes().all(|byte| byte.is_ascii_alphabetic()))
    {
        return Err(Error::InvalidMeetingUrl);
    }
    Ok(format!(
        "https://meet.google.com/{}",
        code.to_ascii_lowercase()
    ))
}

impl Config {
    pub fn validate(&self) -> Result<(), Error> {
        self.validated().map(|_| ())
    }

    pub(crate) fn validated(&self) -> Result<Validated, Error> {
        let mut api = Url::parse(self.api_url.trim()).map_err(|_| Error::InvalidApiUrl)?;
        if !without_credentials(&api)
            || (api.scheme() != "https" && !(api.scheme() == "http" && loopback(&api)))
            || api.query().is_some()
            || api.fragment().is_some()
        {
            return Err(Error::InvalidApiUrl);
        }
        let path = api.path().trim_end_matches('/');
        let path = if path.ends_with("/api/v1") {
            format!("{path}/")
        } else {
            format!("{path}/api/v1/")
        };
        api.set_path(&path);

        let callback =
            Url::parse(self.callback_url.trim()).map_err(|_| Error::InvalidCallbackUrl)?;
        if callback.scheme() != "wss"
            || !without_credentials(&callback)
            || callback.query().is_some()
            || callback.fragment().is_some()
        {
            return Err(Error::InvalidCallbackUrl);
        }
        if self.bind_address.port() == 0 {
            return Err(Error::InvalidBindAddress);
        }

        let key = self.api_key.trim();
        if key.is_empty() || key.len() > 512 || !key.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(Error::InvalidCredentials);
        }
        let mut authorization = HeaderValue::from_str(&format!("Token {key}"))
            .map_err(|_| Error::InvalidCredentials)?;
        authorization.set_sensitive(true);

        if !self.certificate_path.is_absolute() || !self.private_key_path.is_absolute() {
            return Err(Error::InvalidCertificate);
        }
        let certificates = CertificateDer::pem_file_iter(&self.certificate_path)
            .map_err(|_| Error::InvalidCertificate)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| Error::InvalidCertificate)?;
        let key = PrivateKeyDer::from_pem_file(&self.private_key_path)
            .map_err(|_| Error::InvalidCertificate)?;
        let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| Error::InvalidCertificate)?
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .map_err(|_| Error::InvalidCertificate)?;

        Ok(Validated {
            api,
            authorization,
            callback,
            acceptor: TlsAcceptor::from(Arc::new(tls)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_meet_links_without_forwarding_query_parameters() {
        assert_eq!(
            normalize_meeting_url("https://meet.google.com/meet/abc-defg-hij/?pli=1").as_deref(),
            Ok("https://meet.google.com/abc-defg-hij")
        );
        for bad in [
            "https://meet.google.com.attacker.invalid/abc-defg-hij",
            "https://example@meet.google.com/abc-defg-hij",
            "https://meet.google.com:444/abc-defg-hij",
            "https://meet.google.com/lookup/some-name",
            "http://meet.google.com/abc-defg-hij",
        ] {
            assert_eq!(normalize_meeting_url(bad), Err(Error::InvalidMeetingUrl));
        }
    }
}
