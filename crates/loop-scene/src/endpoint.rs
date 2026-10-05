//! Non-generative transport probe for the configured inference host.
use std::time::Duration;

pub(crate) async fn probe(base: &str) -> std::result::Result<String, String> {
    let mut url = reqwest::Url::parse(base).map_err(|_| "invalid model endpoint URL".to_owned())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("model endpoint requires HTTP(S)".into());
    }
    url.set_path(&format!("{}/models", url.path().trim_end_matches('/')));
    url.set_query(None);
    url.set_fragment(None);
    // Do not transmit URL credentials or follow redirects to other services.
    let _ = url.set_username("");
    let _ = url.set_password(None);
    let client = reqwest::Client::builder().connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(8)).redirect(reqwest::redirect::Policy::none())
        .build().map_err(|_| "could not create endpoint health client".to_owned())?;
    let response = client.get(url).send().await
        .map_err(|e| format!("model endpoint unavailable (no generation requested): {}", e.without_url()))?;
    classify_status(response.status().as_u16())
}

fn classify_status(code: u16) -> std::result::Result<String, String> {
    if code == 429 || code >= 500 {
        Err(format!("model endpoint unavailable: HTTP {code}; no generation requested"))
    } else if (200..300).contains(&code) {
        Ok(format!("HTTP {code}; non-generative endpoint check only, not inference validation"))
    } else if [401, 403, 404, 405].contains(&code) {
        Ok(format!("host reachable (HTTP {code}); unauthenticated catalog probe does not validate inference or credentials"))
    } else {
        Err(format!("unexpected endpoint health response HTTP {code}; no generation requested"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn health_status_distinguishes_outage_from_restricted_catalog() {
        for code in [200, 204, 401, 403, 404, 405] { assert!(classify_status(code).is_ok()); }
        for code in [301, 429, 500, 502, 503] { assert!(classify_status(code).is_err()); }
    }
    #[tokio::test]
    async fn refused_endpoint_is_a_blocker_without_generation() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(probe(&format!("http://127.0.0.1:{port}/v1")).await.is_err());
    }
    #[tokio::test]
    async fn probe_uses_get_catalog_not_completions() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0u8; 2048];
            let n = socket.read(&mut bytes).await.unwrap();
            let request = String::from_utf8_lossy(&bytes[..n]);
            assert!(request.starts_with("GET /v1/models HTTP/1.1"));
            assert!(!request.to_lowercase().contains("authorization:"));
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").await.unwrap();
        });
        assert!(probe(&format!("http://{address}/v1")).await.is_ok());
        server.await.unwrap();
    }
}
