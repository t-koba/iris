//! Deterministic download-envelope tests; no live pCloud account or signed URL.
use super::*;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

const SECRET: &str = "signed-download-secret";
const BODY: &str = "private-body";

fn range(length: Option<u64>) -> Option<ByteRange> {
    Some(ByteRange { offset: 4, length })
}

async fn download(
    range: Option<ByteRange>,
    response: Option<String>,
) -> Result<ReadHandle, BackendError> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let expected_range = range.as_ref().map(|range| match range.length {
        Some(length) => format!("bytes={}-{}", range.offset, range.offset + length - 1),
        None => format!("bytes={}-", range.offset),
    });
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        assert!(line.starts_with(&format!("GET /{SECRET}?signature={SECRET} ")));
        let mut observed_range = None;
        loop {
            line.clear();
            assert!(stream.read_line(&mut line).await.unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            let (name, value) = line.split_once(':').unwrap();
            if name.eq_ignore_ascii_case("range") {
                observed_range = Some(value.trim().to_owned());
            }
        }
        assert_eq!(observed_range, expected_range);
        if let Some(response) = response {
            stream.write_all(response.as_bytes()).await.unwrap();
        }
        // None closes before sending headers, exercising .send() failure.
    });
    let client = PCloudApiClient::with_test_base_url("token".into(), format!("http://{address}"))
        .transfer_http_client();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        download_read(
            client,
            &format!("http://{address}/{SECRET}?signature={SECRET}"),
            range,
        ),
    )
    .await
    .expect("bounded local download");
    server.await.unwrap();
    result
}

fn response(status: u16, headers: &str, body: &str) -> String {
    format!("HTTP/1.1 {status} Test\r\nConnection: close\r\n{headers}\r\n{body}")
}

fn assert_safe(error: &BackendError) {
    let details = format!("{error}\n{error:?}");
    for forbidden in [SECRET, BODY, "http://", "127.0.0.1"] {
        assert!(
            !details.contains(forbidden),
            "sensitive response details escaped: {details}"
        );
    }
    assert!(details.len() < 4096, "bounded error details");
}

#[tokio::test]
async fn rejects_invalid_download_envelopes_without_returning_a_stream() {
    let length = format!("Content-Length: {}\r\n", BODY.len());
    let cases = [
        ("non-success full", None, 403, length.as_str()),
        ("non-success range with matching length", range(Some(12)), 503, length.as_str()),
        ("unexpected success status for full", None, 204, ""),
        ("ignored Range", range(Some(12)), 200, length.as_str()),
        ("unexpected partial full", None, 206, "Content-Length: 12\r\nContent-Range: bytes 4-15/20\r\n"),
        ("partial header on full", None, 200, "Content-Length: 12\r\nContent-Range: bytes 4-15/20\r\n"),
        ("wrong offset same length", range(Some(12)), 206, "Content-Length: 12\r\nContent-Range: bytes 5-16/20\r\n"),
        ("missing range", range(Some(12)), 206, length.as_str()),
        ("malformed range", range(Some(12)), 206, "Content-Length: 12\r\nContent-Range: private-body\r\n"),
        ("reversed range", range(Some(12)), 206, "Content-Length: 12\r\nContent-Range: bytes 15-4/20\r\n"),
        ("invalid total", range(Some(12)), 206, "Content-Length: 12\r\nContent-Range: bytes 4-15/15\r\n"),
        ("wrong end", range(Some(12)), 206, "Content-Length: 12\r\nContent-Range: bytes 4-14/20\r\n"),
        ("short range with unknown total", range(Some(13)), 206, "Content-Length: 12\r\nContent-Range: bytes 4-15/*\r\n"),
        ("end exceeds request at EOF", range(Some(11)), 206, "Content-Length: 12\r\nContent-Range: bytes 4-15/16\r\n"),
        ("unsatisfiable start", range(Some(12)), 206, "Content-Length: 4\r\nContent-Range: bytes 0-3/4\r\n"),
        ("length mismatch", range(Some(12)), 206, "Content-Length: 11\r\nContent-Range: bytes 4-15/20\r\n"),
        ("open range truncated", range(None), 206, "Content-Length: 12\r\nContent-Range: bytes 4-15/20\r\n"),
        ("open range unknown EOF", range(None), 206, "Content-Length: 12\r\nContent-Range: bytes 4-15/*\r\n"),
        ("duplicate range", range(Some(12)), 206, "Content-Length: 12\r\nContent-Range: bytes 4-15/20\r\nContent-Range: bytes 4-15/20\r\n"),
        ("overflow range", range(None), 206, "Content-Length: 12\r\nContent-Range: bytes 0-18446744073709551615/*\r\n"),
    ];
    for (name, range, status, headers) in cases {
        let result = download(range, Some(response(status, headers, BODY))).await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("{name} exposed a stream"),
        };
        assert_safe(&error);
    }
}

#[tokio::test]
async fn valid_full_bounded_and_open_downloads_preserve_bytes_and_observed_range() {
    let cases = [
        (None, 200, "Content-Length: 12\r\n", Some(12)),
        (None, 200, "", None),
        (
            range(Some(12)),
            206,
            "Content-Length: 12\r\nContent-Range: bytes 4-15/20\r\n",
            Some(12),
        ),
        (
            range(Some(12)),
            206,
            "Content-Length: 12\r\nContent-Range: bytes 4-15/*\r\n",
            Some(12),
        ),
        (
            range(Some(20)),
            206,
            "Content-Length: 12\r\nContent-Range: bytes 4-15/16\r\n",
            Some(12),
        ),
        (
            range(Some(20)),
            206,
            "Content-Range: bytes 4-15/16\r\n",
            Some(12),
        ),
        (
            range(None),
            206,
            "Content-Length: 12\r\nContent-Range: bytes 4-15/16\r\n",
            Some(12),
        ),
        (
            range(None),
            206,
            "Content-Range: bytes 4-15/16\r\n",
            Some(12),
        ),
    ];
    for (range, status, headers, content_length) in cases {
        let ranged = range.is_some();
        let mut handle = download(range, Some(response(status, headers, BODY)))
            .await
            .unwrap();
        assert_eq!(handle.content_length, content_length);
        if ranged {
            let observed = handle.content_range.unwrap();
            assert_eq!(observed.offset, 4);
            assert_eq!(observed.length, Some(12));
        } else {
            assert!(handle.content_range.is_none());
        }
        let mut bytes = Vec::new();
        handle.stream.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, BODY.as_bytes());
    }
}

#[tokio::test]
async fn ranged_downloads_reject_short_and_overlong_close_delimited_bodies() {
    for range in [range(Some(12)), range(None)] {
        for body in ["short", "private-body-extra"] {
            let mut handle = download(
                range.clone(),
                Some(response(206, "Content-Range: bytes 4-15/16\r\n", body)),
            )
            .await
            .unwrap();
            let error = handle
                .stream
                .read_to_end(&mut Vec::new())
                .await
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
            assert_safe(&BackendError::Internal(Box::new(error)));
        }
    }
}

#[tokio::test]
async fn transport_errors_do_not_retain_signed_download_urls() {
    let error = match download(None, None).await {
        Err(error) => error,
        Ok(_) => panic!("missing response exposed a stream"),
    };
    assert_safe(&error);

    let mut handle = download(None, Some(response(200, "Content-Length: 24\r\n", BODY)))
        .await
        .unwrap();
    let error = handle
        .stream
        .read_to_end(&mut Vec::new())
        .await
        .unwrap_err();
    let details = format!("{error}\n{error:?}");
    assert!(!details.contains(SECRET));
    assert!(!details.contains("http://"));
}

#[tokio::test]
async fn invalid_requested_ranges_fail_without_sending() {
    for range in [
        ByteRange {
            offset: 4,
            length: Some(0),
        },
        ByteRange {
            offset: u64::MAX,
            length: Some(2),
        },
    ] {
        let client = PCloudApiClient::with_test_base_url("token".into(), "http://127.0.0.1".into())
            .transfer_http_client();
        let result = download_read(client, "not a URL", Some(range)).await;
        match result {
            Err(error) => assert!(error.to_string().contains("invalid requested byte range")),
            Ok(_) => panic!("invalid request exposed a stream"),
        }
    }
}
