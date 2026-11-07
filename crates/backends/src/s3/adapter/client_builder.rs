use super::{S3Adapter, S3Config};
use aws_sdk_s3::Client as S3Client;
use aws_smithy_http_client::{tls, Builder as HttpClientBuilder};
use iris_core_domain::types::error::BackendError;
use iris_core_domain::types::object::BackendId;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

const S3_KEEPALIVE_SECS: u64 = 30;

impl S3Adapter {
    pub fn new(mut config: S3Config, backend_id: BackendId) -> Self {
        config.prefix = config.prefix.map(|prefix| {
            if prefix.is_empty() {
                prefix
            } else {
                format!("{}/", prefix.trim_end_matches('/'))
            }
        });
        Self {
            config,
            backend_id,
            client: tokio::sync::OnceCell::new(),
            ticket_counter: AtomicU64::new(1),
        }
    }
}

pub(super) async fn build_client(config: &S3Config) -> Result<S3Client, BackendError> {
    let http_client = build_tuned_http_client();
    let sdk_config_loader = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(aws_sdk_s3::config::Region::new(config.region.clone()))
        .http_client(http_client);

    let sdk_config = sdk_config_loader.load().await;
    let mut s3_config = aws_sdk_s3::config::Builder::from(&sdk_config);
    // The connection's endpoint is authoritative. Otherwise the AWS SDK may
    // inherit an environment/profile endpoint that bypasses per-connection
    // custom-endpoint conditional-write confirmation.
    s3_config.set_endpoint_url(config.endpoint_url.clone());
    Ok(S3Client::from_conf(s3_config.build()))
}

fn build_tuned_http_client() -> aws_sdk_s3::config::SharedHttpClient {
    HttpClientBuilder::new()
        .pool_idle_timeout(Duration::from_secs(S3_KEEPALIVE_SECS))
        .tls_provider(tls::Provider::Rustls(
            tls::rustls_provider::CryptoMode::Ring,
        ))
        .build_https()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_construction_does_not_initialize_aws_client() {
        let adapter = S3Adapter::new(
            S3Config {
                bucket: "archive".into(),
                prefix: None,
                endpoint_url: Some("http://127.0.0.1:1".into()),
                custom_endpoint_conditional_writes_confirmed: false,
                region: "us-east-1".into(),
            },
            BackendId("s3:test".into()),
        );

        assert!(adapter.client.get().is_none());
    }
}
