use std::net::{IpAddr, SocketAddr};

use tokio::net::TcpStream;

pub async fn is_someone_listening_on_that_tcp_port(
    port: u16,
    timeout: tokio::time::Duration,
) -> bool {
    match tokio::time::timeout(timeout, TcpStream::connect(&format!("127.0.0.1:{}", port))).await {
        Ok(Ok(_)) => true,   // Connection successful
        Ok(Err(_)) => false, // Connection failed, refused
        Err(e) => {
            // Timeout occurred
            tracing::error!("Timeout occurred while checking port {}: {}", port, e);
            false // still no one is listening, as far as we can tell
        }
    }
}

pub fn resolve_http_bind_addr(
    http_host: Option<&str>,
    inside_container: bool,
    port: u16,
) -> Result<SocketAddr, String> {
    let host = http_host
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .unwrap_or(if inside_container {
            "0.0.0.0"
        } else {
            "127.0.0.1"
        });
    let ip = host
        .parse::<IpAddr>()
        .map_err(|error| format!("invalid --http-host '{host}': {error}"))?;
    Ok(SocketAddr::new(ip, port))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::net::TcpListener;

    use super::*;

    #[tokio::test]
    async fn returns_true_when_localhost_port_is_listening() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        assert!(is_someone_listening_on_that_tcp_port(port, Duration::from_secs(1)).await);
    }

    #[tokio::test]
    async fn returns_false_for_unused_localhost_port() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        assert!(!is_someone_listening_on_that_tcp_port(port, Duration::from_secs(1)).await);
    }

    #[test]
    fn http_bind_defaults_to_loopback() {
        let addr = resolve_http_bind_addr(None, false, 8001).unwrap();
        assert_eq!(addr.to_string(), "127.0.0.1:8001");
    }

    #[test]
    fn http_bind_preserves_inside_container_default() {
        let addr = resolve_http_bind_addr(None, true, 8001).unwrap();
        assert_eq!(addr.to_string(), "0.0.0.0:8001");
    }

    #[test]
    fn http_bind_uses_explicit_host() {
        let addr = resolve_http_bind_addr(Some("127.0.0.1"), true, 8001).unwrap();
        assert_eq!(addr.to_string(), "127.0.0.1:8001");
    }

    #[test]
    fn http_bind_rejects_invalid_host() {
        assert!(resolve_http_bind_addr(Some("localhost"), false, 8001).is_err());
    }
}
