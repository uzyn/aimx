mod echo;

use axum::{
    extract::ConnectInfo,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tokio::net::TcpStream;

#[derive(Deserialize)]
struct ProbeRequest {
    ip: Option<String>,
}

#[derive(Serialize)]
struct ProbeResponse {
    reachable: bool,
    ip: String,
}

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    service: String,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        service: "aimx-verify".to_string(),
    })
}

async fn probe(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    params: axum::extract::Query<ProbeRequest>,
) -> Json<ProbeResponse> {
    let target_ip = params.ip.clone().unwrap_or_else(|| addr.ip().to_string());

    let reachable = check_port25(&target_ip).await;

    Json(ProbeResponse {
        reachable,
        ip: target_ip,
    })
}

async fn probe_post(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<ProbeRequest>,
) -> Json<ProbeResponse> {
    let target_ip = body.ip.unwrap_or_else(|| addr.ip().to_string());

    let reachable = check_port25(&target_ip).await;

    Json(ProbeResponse {
        reachable,
        ip: target_ip,
    })
}

async fn check_port25(ip: &str) -> bool {
    let addr = format!("{ip}:25");
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            TcpStream::connect(&addr)
        )
        .await,
        Ok(Ok(_))
    )
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() > 1 && args[1] == "echo" {
        if let Err(e) = echo::run_echo() {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
        return;
    }

    let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
    rt.block_on(async {
        tracing_subscriber::fmt::init();

        let app = Router::new()
            .route("/", get(health))
            .route("/health", get(health))
            .route("/probe", get(probe))
            .route("/probe", post(probe_post));

        let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3025".to_string());
        let listener = tokio::net::TcpListener::bind(&bind_addr)
            .await
            .expect("Failed to bind");

        tracing::info!("aimx-verify listening on {bind_addr}");

        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("Server error");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn health_returns_ok() {
        let response = health().await;
        assert_eq!(response.status, "ok");
        assert_eq!(response.service, "aimx-verify");
    }

    #[tokio::test]
    async fn check_port25_unreachable_host() {
        let result = check_port25("192.0.2.1").await;
        assert!(!result);
    }

    #[test]
    fn probe_response_serializes() {
        let resp = ProbeResponse {
            reachable: true,
            ip: "1.2.3.4".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"reachable\":true"));
        assert!(json.contains("\"ip\":\"1.2.3.4\""));
    }

    #[test]
    fn probe_response_false_serializes() {
        let resp = ProbeResponse {
            reachable: false,
            ip: "5.6.7.8".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"reachable\":false"));
    }

    #[test]
    fn probe_request_deserializes_with_ip() {
        let json = r#"{"ip": "1.2.3.4"}"#;
        let req: ProbeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.ip.unwrap(), "1.2.3.4");
    }

    #[test]
    fn probe_request_deserializes_without_ip() {
        let json = r#"{}"#;
        let req: ProbeRequest = serde_json::from_str(json).unwrap();
        assert!(req.ip.is_none());
    }

    #[test]
    fn health_response_serializes() {
        let resp = HealthResponse {
            status: "ok".to_string(),
            service: "aimx-verify".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"status\":\"ok\""));
        assert!(json.contains("\"service\":\"aimx-verify\""));
    }
}
