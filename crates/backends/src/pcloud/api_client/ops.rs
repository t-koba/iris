use super::{PCloudApiClient, PCloudMetadata, PCloudResponse};
use futures_util::StreamExt;
use iris_core_domain::types::error::{BackendError, NetworkErrorDetail};
use reqwest::{multipart, RequestBuilder};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::io::Cursor;
use std::pin::Pin;
use std::time::Duration;
use tokio::io::AsyncRead;
use tokio::sync::watch;
use tokio_util::io::ReaderStream;

impl PCloudApiClient {
    pub async fn list_folder(&self, folder_id: u64) -> Result<PCloudMetadata, BackendError> {
        self.call_metadata("listfolder", &[("folderid", folder_id.to_string())])
            .await
    }

    pub async fn stat_file(&self, file_id: u64) -> Result<PCloudMetadata, BackendError> {
        self.call_metadata("stat", &[("fileid", file_id.to_string())])
            .await
    }

    pub async fn get_file_link(&self, file_id: u64) -> Result<String, BackendError> {
        let data: PCloudFileLinkResponse = self
            .call("getfilelink", &[("fileid", file_id.to_string())])
            .await?
            .ok_or_else(|| internal_error("missing file link data"))?;
        let host = data
            .hosts
            .first()
            .ok_or_else(|| internal_error("no download host available"))?;
        Ok(format!("https://{}{}", host, data.path))
    }

    pub async fn copy_file(
        &self,
        file_id: u64,
        to_folder_id: u64,
        new_name: &str,
    ) -> Result<PCloudMetadata, BackendError> {
        self.call_metadata(
            "copyfile",
            &[
                ("fileid", file_id.to_string()),
                ("tofolderid", to_folder_id.to_string()),
                ("toname", new_name.to_string()),
            ],
        )
        .await
    }

    pub async fn copy_folder(
        &self,
        folder_id: u64,
        to_folder_id: u64,
    ) -> Result<PCloudMetadata, BackendError> {
        self.call_metadata(
            "copyfolder",
            &[
                ("folderid", folder_id.to_string()),
                ("tofolderid", to_folder_id.to_string()),
            ],
        )
        .await
    }

    /// pCloud documents renamefile replacement as atomic (unlike folder replacement).
    pub async fn rename_file(
        &self,
        file_id: u64,
        to_folder_id: u64,
        new_name: &str,
    ) -> Result<PCloudMetadata, BackendError> {
        self.call_metadata(
            "renamefile",
            &[
                ("fileid", file_id.to_string()),
                ("tofolderid", to_folder_id.to_string()),
                ("toname", new_name.to_string()),
            ],
        )
        .await
    }

    pub async fn rename_folder(
        &self,
        folder_id: u64,
        to_folder_id: u64,
        new_name: &str,
    ) -> Result<PCloudMetadata, BackendError> {
        self.call_metadata(
            "renamefolder",
            &[
                ("folderid", folder_id.to_string()),
                ("tofolderid", to_folder_id.to_string()),
                ("toname", new_name.to_string()),
            ],
        )
        .await
    }

    pub async fn delete_file(&self, file_id: u64) -> Result<(), BackendError> {
        self.call::<serde_json::Value>("deletefile", &[("fileid", file_id.to_string())])
            .await
            .map(drop)
    }

    pub async fn delete_folder_recursive(&self, folder_id: u64) -> Result<(), BackendError> {
        self.call::<serde_json::Value>(
            "deletefolderrecursive",
            &[("folderid", folder_id.to_string())],
        )
        .await
        .map(drop)
    }

    pub async fn create_folder(
        &self,
        parent_folder_id: u64,
        name: &str,
    ) -> Result<PCloudMetadata, BackendError> {
        self.call_metadata(
            "createfolder",
            &[
                ("folderid", parent_folder_id.to_string()),
                ("name", name.to_string()),
            ],
        )
        .await
    }

    pub async fn get_thumb(
        &self,
        file_id: u64,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, BackendError> {
        let response = self
            .request("getthumb")
            .query(&[
                ("fileid", file_id.to_string()),
                ("size", format!("{width}x{height}")),
            ])
            .send()
            .await
            .map_err(|e| network_error("getthumb", e))?;
        if !response.status().is_success() {
            return Err(BackendError::Unsupported {
                operation: "get_thumbnail for this file type",
            });
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|e| internal_reqwest_error("getthumb", e))?;
        Ok(bytes.to_vec())
    }

    pub async fn upload_file_to_folder(
        &self,
        folder_id: u64,
        file_name: &str,
        data: Vec<u8>,
    ) -> Result<PCloudMetadata, BackendError> {
        let content_length =
            u64::try_from(data.len()).map_err(|error| BackendError::Internal(Box::new(error)))?;
        self.upload_stream_to_folder(
            folder_id,
            file_name,
            Box::pin(Cursor::new(data)),
            Some(content_length),
        )
        .await
    }

    pub async fn upload_stream_to_folder(
        &self,
        folder_id: u64,
        file_name: &str,
        reader: Pin<Box<dyn AsyncRead + Send>>,
        content_length: Option<u64>,
    ) -> Result<PCloudMetadata, BackendError> {
        let (progress_tx, progress_rx) = watch::channel(tokio::time::Instant::now());
        let body_progress = progress_tx.clone();
        let progress = ReaderStream::new(reader).inspect(move |result| {
            if let Ok(bytes) = result {
                if !bytes.is_empty() {
                    body_progress.send_replace(tokio::time::Instant::now());
                }
            }
        });
        let body = reqwest::Body::wrap_stream(progress);
        let part = match content_length {
            Some(length) => multipart::Part::stream_with_length(body, length),
            None => multipart::Part::stream(body),
        };
        self.upload(
            folder_id,
            part.file_name(file_name.to_string()),
            progress_tx,
            progress_rx,
        )
        .await
    }

    async fn upload(
        &self,
        folder_id: u64,
        part: multipart::Part,
        progress_tx: watch::Sender<tokio::time::Instant>,
        progress: watch::Receiver<tokio::time::Instant>,
    ) -> Result<PCloudMetadata, BackendError> {
        // uploadfile overwrites an existing file, retaining its old contents as a
        // revision. nopartial prevents incomplete uploads from replacing that file.
        let upload = async {
            let response = self
                .transfer_http
                .post(format!("{}/uploadfile", self.api_base))
                .query(&[
                    ("access_token", self.access_token.clone()),
                    ("folderid", folder_id.to_string()),
                    ("nopartial", "1".to_string()),
                ])
                .multipart(multipart::Form::new().part("file", part))
                .send()
                .await
                .map_err(|e| network_error("uploadfile", e))?;
            // Keep the sender alive after request EOF: response bytes are transfer
            // progress too, even when the entire request body was buffered.
            let body =
                parse_response::<serde_json::Value>("uploadfile", response, Some(&progress_tx))
                    .await?;
            extract_uploaded_metadata(&body.unwrap_or_default())
        };
        tokio::select! {
            result = upload => result,
            () = wait_for_upload_progress(progress, self.transfer_idle_timeout) => Err(upload_stalled_error()),
        }
    }

    fn request(&self, method: &str) -> RequestBuilder {
        self.http
            .get(format!("{}/{method}", self.api_base))
            .query(&[("access_token", self.access_token.as_str())])
            .timeout(self.api_timeout)
    }

    /// Calls `method` and returns the body of a successful response.
    async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: &[(&str, String)],
    ) -> Result<Option<T>, BackendError> {
        let response = self
            .request(method)
            .query(params)
            .send()
            .await
            .map_err(|e| network_error(method, e))?;
        parse_response(method, response, None).await
    }

    async fn call_metadata(
        &self,
        method: &str,
        params: &[(&str, String)],
    ) -> Result<PCloudMetadata, BackendError> {
        self.call::<PCloudMetadataResponse>(method, params)
            .await?
            .map(|data| data.metadata)
            .ok_or_else(|| internal_error(&format!("missing metadata in pCloud {method} response")))
    }
}

async fn parse_response<T: DeserializeOwned>(
    method: &str,
    mut response: reqwest::Response,
    progress: Option<&watch::Sender<tokio::time::Instant>>,
) -> Result<Option<T>, BackendError> {
    let body: PCloudResponse<T> = if let Some(progress) = progress {
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| internal_reqwest_error(method, e))?
        {
            if !chunk.is_empty() {
                progress.send_replace(tokio::time::Instant::now());
                bytes.extend_from_slice(&chunk);
            }
        }
        serde_json::from_slice(&bytes).map_err(|e| internal_response_error(method, e))?
    } else {
        response
            .json()
            .await
            .map_err(|e| internal_reqwest_error(method, e))?
    };
    PCloudApiClient::ensure_success(method, &body)?;
    Ok(body.data)
}

async fn wait_for_upload_progress(
    mut progress: watch::Receiver<tokio::time::Instant>,
    idle_timeout: Duration,
) {
    let mut last_progress = *progress.borrow_and_update();
    loop {
        let elapsed = tokio::time::Instant::now().saturating_duration_since(last_progress);
        let remaining = idle_timeout.saturating_sub(elapsed);
        if remaining.is_zero() {
            return;
        }
        match tokio::time::timeout(remaining, progress.changed()).await {
            Ok(Ok(())) => last_progress = *progress.borrow_and_update(),
            Ok(Err(_)) => {
                // The body has ended; keep the response wait bounded by the same idle policy.
                tokio::time::sleep(remaining).await;
                return;
            }
            Err(_) => return,
        }
    }
}

fn upload_stalled_error() -> BackendError {
    BackendError::Network(NetworkErrorDetail {
        message: "pCloud uploadfile stalled without transfer progress".to_owned(),
        is_timeout: true,
    })
}

fn network_error(method: &str, error: reqwest::Error) -> BackendError {
    let is_timeout = error.is_timeout();
    let error = error.without_url();
    BackendError::Network(NetworkErrorDetail {
        message: format!("pCloud {method} failed: {error}"),
        is_timeout,
    })
}

fn internal_reqwest_error(method: &str, error: reqwest::Error) -> BackendError {
    internal_response_error(method, error.without_url())
}

fn internal_response_error(
    method: &str,
    error: impl std::error::Error + Send + Sync + 'static,
) -> BackendError {
    BackendError::Internal(Box::new(PCloudResponseError {
        method: method.to_owned(),
        source: error,
    }))
}

#[derive(Debug)]
struct PCloudResponseError<E> {
    method: String,
    source: E,
}

impl<E: std::fmt::Display> std::fmt::Display for PCloudResponseError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pCloud {} response failed: {}", self.method, self.source)
    }
}

impl<E: std::error::Error + 'static> std::error::Error for PCloudResponseError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn internal_error(message: &str) -> BackendError {
    BackendError::Internal(Box::new(std::io::Error::other(message.to_string())))
}

#[derive(Debug, Deserialize)]
struct PCloudMetadataResponse {
    metadata: PCloudMetadata,
}

#[derive(Debug, Deserialize)]
struct PCloudFileLinkResponse {
    hosts: Vec<String>,
    path: String,
}

fn extract_uploaded_metadata(body: &serde_json::Value) -> Result<PCloudMetadata, BackendError> {
    let metadata = match body.get("metadata") {
        Some(serde_json::Value::Array(items)) => items.first(),
        other => other,
    }
    .ok_or_else(|| internal_error("missing uploaded metadata in pCloud upload response"))?;
    serde_json::from_value(metadata.clone()).map_err(|e| BackendError::Internal(Box::new(e)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::time::{sleep, Duration};

    const TOKEN: &str = "synthetic-pcloud-test-token";

    fn assert_token_redacted(error: &BackendError) {
        let display = error.to_string();
        let debug = format!("{error:?}");
        assert!(
            !display.contains(TOKEN),
            "token leaked through Display: {display}"
        );
        assert!(
            !debug.contains(TOKEN),
            "token leaked through Debug: {debug}"
        );
    }

    async fn read_request_line(stream: &mut tokio::net::TcpStream) {
        let mut request_line = Vec::new();
        loop {
            let byte = stream.read_u8().await.expect("read request line");
            request_line.push(byte);
            if request_line.ends_with(b"\r\n") {
                break;
            }
        }
        assert!(
            String::from_utf8_lossy(&request_line).contains(TOKEN),
            "mock request must carry the synthetic token in its URL"
        );
    }

    async fn read_content_length(stream: &mut tokio::net::TcpStream) -> usize {
        let mut content_length = None;
        loop {
            let mut line = Vec::new();
            loop {
                let byte = stream.read_u8().await.expect("read HTTP headers");
                line.push(byte);
                assert!(line.len() <= 8192, "bounded HTTP header line");
                if line.ends_with(b"\r\n") {
                    break;
                }
            }
            if line == b"\r\n" {
                break;
            }
            if let Ok(line) = std::str::from_utf8(&line) {
                if let Some((name, value)) = line.trim_end().split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        content_length = value.trim().parse::<usize>().ok();
                    }
                }
            }
        }
        content_length.expect("known multipart content length")
    }

    async fn response_server(
        body: Vec<u8>,
        content_length: usize,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock pCloud server");
        let address = listener.local_addr().expect("mock server address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            read_request_line(&mut stream).await;
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {content_length}\r\nConnection: close\r\n\r\n"
            );
            stream
                .write_all(headers.as_bytes())
                .await
                .expect("write response headers");
            stream.write_all(&body).await.expect("write response body");
        });
        (format!("http://{address}"), server)
    }

    #[tokio::test]
    async fn send_timeout_redacts_url_and_preserves_timeout_classification() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock pCloud server");
        let address = listener.local_addr().expect("mock server address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            read_request_line(&mut stream).await;
            sleep(Duration::from_millis(250)).await;
        });
        let api = PCloudApiClient::with_test_base_url_and_timeouts(
            TOKEN.to_owned(),
            format!("http://{address}"),
            Duration::from_millis(50),
            Duration::from_millis(100),
        );

        let error = api
            .list_folder(0)
            .await
            .expect_err("request should time out");
        assert_token_redacted(&error);
        assert!(error.to_string().contains("listfolder"));
        assert!(matches!(
            &error,
            BackendError::Network(detail) if detail.is_timeout
        ));
        server.abort();
    }

    #[tokio::test]
    async fn api_request_deadline_covers_a_slow_response_body() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock pCloud server");
        let address = listener.local_addr().expect("mock server address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept API request");
            read_request_line(&mut stream).await;
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 128\r\nConnection: close\r\n\r\n{",
                )
                .await
                .expect("write partial API response");
            sleep(Duration::from_millis(250)).await;
        });
        let api = PCloudApiClient::with_test_base_url_and_timeouts(
            TOKEN.to_owned(),
            format!("http://{address}"),
            Duration::from_millis(50),
            Duration::from_millis(200),
        );
        let result = tokio::time::timeout(Duration::from_millis(150), api.list_folder(0)).await;
        let error = result
            .expect("API request deadline applies while reading the response body")
            .expect_err("incomplete API response should fail");
        assert_token_redacted(&error);
        assert!(error.to_string().contains("listfolder response failed"));
        server.abort();
    }

    #[tokio::test]
    async fn progressing_stream_upload_can_outlive_the_api_request_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock pCloud server");
        let address = listener.local_addr().expect("mock server address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept upload");
            read_request_line(&mut stream).await;
            let content_length = read_content_length(&mut stream).await;
            let mut body = vec![0; content_length];
            stream
                .read_exact(&mut body)
                .await
                .expect("read multipart upload");
            let metadata = serde_json::json!({
                "result": 0,
                "metadata": {"name": "slow.bin", "fileid": 7, "isfolder": false, "size": 12}
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                metadata.len(), metadata
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write upload response");
        });
        let api = PCloudApiClient::with_test_base_url_and_timeouts(
            TOKEN.to_owned(),
            format!("http://{address}"),
            Duration::from_millis(100),
            Duration::from_millis(200),
        );
        let (mut source, reader) = tokio::io::duplex(64);
        let producer = tokio::spawn(async move {
            for chunk in [b"abc".as_slice(), b"def", b"ghi", b"jkl"] {
                sleep(Duration::from_millis(40)).await;
                source.write_all(chunk).await.expect("write upload source");
            }
        });

        let started = tokio::time::Instant::now();
        let metadata = tokio::time::timeout(
            Duration::from_secs(2),
            api.upload_stream_to_folder(1, "slow.bin", Box::pin(reader), Some(12)),
        )
        .await
        .expect("bounded progressing upload")
        .expect("upload beyond API request deadline");
        assert!(started.elapsed() > Duration::from_millis(100));
        assert_eq!(metadata.name, "slow.bin");
        producer.await.expect("upload producer");
        server.await.expect("mock server task");
    }

    #[tokio::test]
    async fn buffered_upload_response_progress_resets_idle_watchdog_after_request_eof() {
        let idle_timeout = Duration::from_millis(200);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock pCloud server");
        let address = listener.local_addr().expect("mock server address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept buffered upload");
            stream
                .set_nodelay(true)
                .expect("disable response coalescing");
            read_request_line(&mut stream).await;
            let content_length = read_content_length(&mut stream).await;
            let mut body = vec![0; content_length];
            stream
                .read_exact(&mut body)
                .await
                .expect("consume complete multipart request before responding");
            let response = serde_json::json!({
                "result": 0,
                "metadata": {"name": "buffered.bin", "fileid": 7, "isfolder": false, "size": 10}
            })
            .to_string();
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            );
            stream
                .write_all(headers.as_bytes())
                .await
                .expect("write upload response headers");
            // Each gap is well below the idle budget, but the whole response
            // lasts several budgets. All progress is after request EOF.
            for chunk in response.as_bytes().chunks(10) {
                sleep(Duration::from_millis(50)).await;
                stream.write_all(chunk).await.expect("write response chunk");
            }
        });
        let api = PCloudApiClient::with_test_base_url_and_timeouts(
            TOKEN.to_owned(),
            format!("http://{address}"),
            Duration::from_millis(50),
            idle_timeout,
        );
        let started = tokio::time::Instant::now();
        let metadata = tokio::time::timeout(
            Duration::from_secs(2),
            api.upload_file_to_folder(1, "buffered.bin", b"small body".to_vec()),
        )
        .await
        .expect("bounded progressing response")
        .expect("response progress keeps buffered upload alive");
        assert!(started.elapsed() > idle_timeout);
        assert_eq!(metadata.name, "buffered.bin");
        assert_eq!(metadata.file_id, Some(7));
        tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .expect("bounded mock response server")
            .expect("mock server task");
    }

    #[tokio::test]
    async fn stalled_upload_response_expires_after_request_eof() {
        let idle_timeout = Duration::from_millis(200);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock pCloud server");
        let address = listener.local_addr().expect("mock server address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept buffered upload");
            read_request_line(&mut stream).await;
            let content_length = read_content_length(&mut stream).await;
            let mut body = vec![0; content_length];
            stream
                .read_exact(&mut body)
                .await
                .expect("consume complete multipart request before responding");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 128\r\nConnection: close\r\n\r\n{",
                )
                .await
                .expect("write partial upload response");
            assert_eq!(
                stream
                    .read_u8()
                    .await
                    .expect_err("client closes idle response")
                    .kind(),
                std::io::ErrorKind::UnexpectedEof
            );
        });
        let mut api = PCloudApiClient::with_test_base_url_and_timeouts(
            TOKEN.to_owned(),
            format!("http://{address}"),
            Duration::from_millis(50),
            idle_timeout,
        );
        // Isolate the upload watchdog from reqwest's separate read timeout.
        api.transfer_http = super::super::build_http_client(None);
        let started = tokio::time::Instant::now();
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            api.upload_file_to_folder(1, "stalled.bin", b"small body".to_vec()),
        )
        .await
        .expect("idle budget bounds response wait")
        .expect_err("incomplete response must not wait indefinitely after request EOF");
        assert!(started.elapsed() >= idle_timeout);
        assert!(matches!(
            &error,
            BackendError::Network(detail) if detail.is_timeout
                && detail.message.contains("stalled without transfer progress")
        ));
        assert_token_redacted(&error);
        drop(api);
        tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .expect("server observes idle response cancellation")
            .expect("mock server task");
    }

    #[tokio::test]
    async fn stalled_upload_body_write_expires_when_progress_stops() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock pCloud server");
        let address = listener.local_addr().expect("mock server address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept stalled body");
            read_request_line(&mut stream).await;
            let content_length = read_content_length(&mut stream).await;
            assert!(content_length > 1_000_000_000);
            // Do not consume the upload body until the client-side progress guard cancels it.
            sleep(Duration::from_millis(400)).await;
            let mut buffer = [0; 16 * 1024];
            loop {
                if stream
                    .read(&mut buffer)
                    .await
                    .expect("drain canceled request")
                    == 0
                {
                    break;
                }
            }
        });
        let api = PCloudApiClient::with_test_base_url_and_timeouts(
            TOKEN.to_owned(),
            format!("http://{address}"),
            Duration::from_millis(50),
            Duration::from_millis(80),
        );
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            api.upload_stream_to_folder(
                1,
                "large.bin",
                Box::pin(tokio::io::repeat(0_u8)),
                Some(1_000_000_000),
            ),
        )
        .await
        .expect("bounded stalled upload");
        let error = result.expect_err("body write should stop after idle timeout");
        assert!(matches!(
            &error,
            BackendError::Network(detail) if detail.is_timeout
        ));
        assert_token_redacted(&error);
        drop(api);
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("server observes canceled body request")
            .expect("mock server task");
    }

    #[tokio::test]
    async fn stalled_upload_remains_cancellable_after_the_api_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock pCloud server");
        let address = listener.local_addr().expect("mock server address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept stalled upload");
            read_request_line(&mut stream).await;
            let content_length = read_content_length(&mut stream).await;
            let mut body = vec![0; content_length];
            stream
                .read_exact(&mut body)
                .await
                .expect("read stalled upload body");
            let mut byte = [0; 1];
            assert_eq!(
                stream
                    .read(&mut byte)
                    .await
                    .expect("observe client cancellation"),
                0,
                "canceling the request closes its HTTP connection"
            );
        });
        let api = PCloudApiClient::with_test_base_url_and_timeouts(
            TOKEN.to_owned(),
            format!("http://{address}"),
            Duration::from_millis(50),
            Duration::from_secs(1),
        );
        let started = tokio::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_millis(150),
            api.upload_file_to_folder(1, "stalled.bin", b"small body".to_vec()),
        )
        .await;
        assert!(
            result.is_err(),
            "external cancellation, not API timeout, ends upload"
        );
        assert!(started.elapsed() >= Duration::from_millis(150));
        drop(api);
        tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .expect("server observes request cancellation")
            .expect("mock server task");
    }

    #[tokio::test]
    async fn malformed_json_response_redacts_url_and_keeps_endpoint_context() {
        let body = b"not-json".to_vec();
        let (api_base, server) = response_server(body.clone(), body.len()).await;
        let api = PCloudApiClient::with_test_base_url(TOKEN.to_owned(), api_base);

        let error = api
            .list_folder(0)
            .await
            .expect_err("invalid JSON should fail");
        assert_token_redacted(&error);
        assert!(error
            .to_string()
            .contains("pCloud listfolder response failed"));
        assert!(matches!(error, BackendError::Internal(_)));
        server.await.expect("mock server task");
    }

    #[tokio::test]
    async fn thumbnail_body_error_redacts_url_and_keeps_endpoint_context() {
        let body = b"x".to_vec();
        let (api_base, server) = response_server(body.clone(), body.len() + 10).await;
        let api = PCloudApiClient::with_test_base_url(TOKEN.to_owned(), api_base);

        let error = api
            .get_thumb(1, 64, 64)
            .await
            .expect_err("truncated body should fail");
        assert_token_redacted(&error);
        assert!(error
            .to_string()
            .contains("pCloud getthumb response failed"));
        assert!(matches!(error, BackendError::Internal(_)));
        server.await.expect("mock server task");
    }
}
