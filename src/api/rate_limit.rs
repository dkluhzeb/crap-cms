//! Per-IP gRPC rate limiting as a tower Layer/Service.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use axum::http::{Request, Response};
use tonic::{Status, transport::server::TcpConnectInfo};
use tower::{Layer, Service};

use crate::core::rate_limit::GrpcRateLimiter;

/// Tower layer that applies per-IP rate limiting to gRPC requests.
#[derive(Clone)]
pub struct GrpcRateLimitLayer {
    limiter: Arc<GrpcRateLimiter>,
}

impl GrpcRateLimitLayer {
    /// Create a new rate limit layer with the given limiter.
    pub fn new(limiter: Arc<GrpcRateLimiter>) -> Self {
        Self { limiter }
    }
}

impl<S> Layer<S> for GrpcRateLimitLayer {
    type Service = GrpcRateLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        GrpcRateLimitService {
            inner,
            limiter: self.limiter.clone(),
        }
    }
}

/// Tower service that checks the per-IP rate limit before forwarding requests.
#[derive(Clone)]
pub struct GrpcRateLimitService<S> {
    inner: S,
    limiter: Arc<GrpcRateLimiter>,
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for GrpcRateLimitService<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    ResBody: Default + Send + 'static,
    ReqBody: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        // Extract IP from TcpConnectInfo (set by tonic's TCP acceptor).
        let ip = req
            .extensions()
            .get::<TcpConnectInfo>()
            .and_then(|info| info.remote_addr())
            .map(|addr| addr.ip().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        if !self.limiter.check_and_record(&ip) {
            let status = Status::resource_exhausted("rate limit exceeded");
            let response = status.into_http();

            return Box::pin(async move { Ok(response) });
        }

        let mut inner = self.inner.clone();

        Box::pin(async move { inner.call(req).await })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        convert::Infallible,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        pin::Pin,
        sync::Arc,
        task::{Context, Poll},
    };

    use axum::{
        body::Body,
        http::{Request, Response, StatusCode},
    };
    use tonic::transport::server::TcpConnectInfo;
    use tower::{Layer, Service, ServiceExt};

    use super::{GrpcRateLimitLayer, GrpcRateLimitService};
    use crate::core::rate_limit::GrpcRateLimiter;

    // ── gRPC status code constants ─────────────────────────────────────────
    //
    // gRPC-over-HTTP always responds with HTTP 200 OK.  The actual status is
    // communicated via the `grpc-status` header (a numeric string). Code 8 is
    // ResourceExhausted.  See https://grpc.github.io/grpc/core/md_doc_statuscodes.html
    const GRPC_STATUS_OK: &str = "0";
    const GRPC_STATUS_RESOURCE_EXHAUSTED: &str = "8";

    /// A minimal passthrough service that always returns 200 OK.
    #[derive(Clone)]
    struct OkService;

    impl Service<Request<Body>> for OkService {
        type Response = Response<Body>;
        type Error = Infallible;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request<Body>) -> Self::Future {
            // A successful gRPC response: HTTP 200 with grpc-status: 0.
            Box::pin(async {
                Ok(Response::builder()
                    .status(200)
                    .header("grpc-status", GRPC_STATUS_OK)
                    .body(Body::empty())
                    .unwrap())
            })
        }
    }

    /// Build a request that carries a `TcpConnectInfo` extension so the
    /// middleware can extract the client IP.
    fn request_with_ip(ip: Ipv4Addr) -> Request<Body> {
        let addr = SocketAddr::new(IpAddr::V4(ip), 12345);
        let connect_info = TcpConnectInfo {
            local_addr: None,
            remote_addr: Some(addr),
        };
        let mut req = Request::new(Body::empty());
        req.extensions_mut().insert(connect_info);
        req
    }

    /// Build a request with NO `TcpConnectInfo` extension (simulates an
    /// in-process or test call where the TCP acceptor hasn't run).
    fn request_no_ip() -> Request<Body> {
        Request::new(Body::empty())
    }

    fn make_service(limiter: Arc<GrpcRateLimiter>) -> GrpcRateLimitService<OkService> {
        GrpcRateLimitLayer::new(limiter).layer(OkService)
    }

    /// Return the value of the `grpc-status` response header (as a `&str`).
    fn grpc_status(resp: &Response<Body>) -> &str {
        resp.headers()
            .get("grpc-status")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("missing")
    }

    // ── IP extraction ──────────────────────────────────────────────────────

    /// When `TcpConnectInfo` is present the middleware extracts the remote IP
    /// and uses it as the rate-limit key.  Two distinct IPs should be tracked
    /// independently, confirming extraction is working.
    ///
    /// gRPC always uses HTTP 200 for transport; the real status is in the
    /// `grpc-status` header.  Code 0 = OK, code 8 = ResourceExhausted.
    #[tokio::test]
    async fn ip_extracted_from_tcp_connect_info() {
        // Limit of 2 requests per IP.  ip_a will exhaust its budget on its
        // second call; ip_b only uses one request so it stays within the limit.
        // This confirms each IP has its own independent counter.
        let limiter = Arc::new(GrpcRateLimiter::new(2, 60));
        let mut svc = make_service(limiter);

        let ip_a = Ipv4Addr::new(10, 0, 0, 1);
        let ip_b = Ipv4Addr::new(10, 0, 0, 2);

        // First request from ip_a — OK (count for ip_a: 1/2).
        let resp_a1 = svc
            .ready()
            .await
            .unwrap()
            .call(request_with_ip(ip_a))
            .await
            .unwrap();
        assert_eq!(
            grpc_status(&resp_a1),
            GRPC_STATUS_OK,
            "ip_a request 1 should pass"
        );

        // Second request from ip_a — OK (count for ip_a: 2/2).
        let resp_a2 = svc
            .ready()
            .await
            .unwrap()
            .call(request_with_ip(ip_a))
            .await
            .unwrap();
        assert_eq!(
            grpc_status(&resp_a2),
            GRPC_STATUS_OK,
            "ip_a request 2 should pass"
        );

        // Third request from ip_a — rate-limited (count would be 3/2).
        let resp_a3 = svc
            .ready()
            .await
            .unwrap()
            .call(request_with_ip(ip_a))
            .await
            .unwrap();
        assert_eq!(
            resp_a3.status(),
            StatusCode::OK,
            "HTTP status is always 200 in gRPC"
        );
        assert_eq!(
            grpc_status(&resp_a3),
            GRPC_STATUS_RESOURCE_EXHAUSTED,
            "ip_a should be rate-limited on its third request"
        );

        // First (and only) request from ip_b — still OK because ip_b is tracked separately.
        let resp_b1 = svc
            .ready()
            .await
            .unwrap()
            .call(request_with_ip(ip_b))
            .await
            .unwrap();
        assert_eq!(
            grpc_status(&resp_b1),
            GRPC_STATUS_OK,
            "ip_b has its own bucket and is not affected by ip_a's limit"
        );
    }

    /// When there is no `TcpConnectInfo` the middleware falls back to the key
    /// `"unknown"`.  All such requests share the same bucket, so they should
    /// be rate-limited as a group.
    #[tokio::test]
    async fn missing_connect_info_uses_unknown_key() {
        let limiter = Arc::new(GrpcRateLimiter::new(2, 60));
        let mut svc = make_service(limiter);

        // First two requests pass.
        let r1 = svc
            .ready()
            .await
            .unwrap()
            .call(request_no_ip())
            .await
            .unwrap();
        assert_eq!(grpc_status(&r1), GRPC_STATUS_OK);
        let r2 = svc
            .ready()
            .await
            .unwrap()
            .call(request_no_ip())
            .await
            .unwrap();
        assert_eq!(grpc_status(&r2), GRPC_STATUS_OK);

        // Third request exceeds the limit for the "unknown" bucket.
        let r3 = svc
            .ready()
            .await
            .unwrap()
            .call(request_no_ip())
            .await
            .unwrap();
        assert_eq!(
            grpc_status(&r3),
            GRPC_STATUS_RESOURCE_EXHAUSTED,
            "requests without IP info share the 'unknown' bucket"
        );
    }

    // ── Within-limit passthrough ───────────────────────────────────────────

    /// Requests that stay within the configured limit must be forwarded to the
    /// inner service and receive its response unchanged.
    #[tokio::test]
    async fn within_limit_passes_through() {
        let limiter = Arc::new(GrpcRateLimiter::new(5, 60));
        let mut svc = make_service(limiter);
        let ip = Ipv4Addr::new(1, 2, 3, 4);

        for i in 0..5 {
            let resp = svc
                .ready()
                .await
                .unwrap()
                .call(request_with_ip(ip))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            assert_eq!(
                grpc_status(&resp),
                GRPC_STATUS_OK,
                "request {i} should pass through to the inner service"
            );
        }
    }

    // ── Limit exceeded → gRPC ResourceExhausted ───────────────────────────

    /// The (max_requests + 1)-th request from the same IP must be rejected
    /// with gRPC status 8 (ResourceExhausted).  The HTTP status remains 200
    /// because gRPC embeds its status in headers, not the HTTP status line.
    #[tokio::test]
    async fn exceeding_limit_returns_resource_exhausted() {
        let max = 3u32;
        let limiter = Arc::new(GrpcRateLimiter::new(max, 60));
        let mut svc = make_service(limiter);
        let ip = Ipv4Addr::new(192, 168, 1, 1);

        for _ in 0..max {
            let resp = svc
                .ready()
                .await
                .unwrap()
                .call(request_with_ip(ip))
                .await
                .unwrap();
            assert_eq!(grpc_status(&resp), GRPC_STATUS_OK);
        }

        // One over the limit.
        let resp = svc
            .ready()
            .await
            .unwrap()
            .call(request_with_ip(ip))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "HTTP status is always 200 in gRPC"
        );
        assert_eq!(
            grpc_status(&resp),
            GRPC_STATUS_RESOURCE_EXHAUSTED,
            "request exceeding the limit should return grpc-status 8 (ResourceExhausted)"
        );
    }

    /// Consecutive over-limit requests must all be rejected (not just the
    /// first overflow).
    #[tokio::test]
    async fn repeated_over_limit_requests_all_rejected() {
        let limiter = Arc::new(GrpcRateLimiter::new(1, 60));
        let mut svc = make_service(limiter);
        let ip = Ipv4Addr::new(10, 10, 10, 10);

        // Consume the single allowed request.
        let r1 = svc
            .ready()
            .await
            .unwrap()
            .call(request_with_ip(ip))
            .await
            .unwrap();
        assert_eq!(grpc_status(&r1), GRPC_STATUS_OK);

        for i in 0..5 {
            let resp = svc
                .ready()
                .await
                .unwrap()
                .call(request_with_ip(ip))
                .await
                .unwrap();
            assert_eq!(
                grpc_status(&resp),
                GRPC_STATUS_RESOURCE_EXHAUSTED,
                "over-limit request {i} should be rejected"
            );
        }
    }

    // ── Rate limiting disabled (max_requests = 0) ─────────────────────────

    /// Setting `max_requests = 0` disables rate limiting entirely.  Any number
    /// of requests from any IP must be forwarded to the inner service.
    #[tokio::test]
    async fn disabled_when_max_requests_zero() {
        let limiter = Arc::new(GrpcRateLimiter::new(0, 60));
        let mut svc = make_service(limiter);
        let ip = Ipv4Addr::new(5, 5, 5, 5);

        for _ in 0..1000 {
            let resp = svc
                .ready()
                .await
                .unwrap()
                .call(request_with_ip(ip))
                .await
                .unwrap();
            assert_eq!(
                grpc_status(&resp),
                GRPC_STATUS_OK,
                "rate limiting disabled: all requests must pass through"
            );
        }
    }

    /// Disabled rate limiting also works for the "unknown" IP case.
    #[tokio::test]
    async fn disabled_allows_unknown_ip() {
        let limiter = Arc::new(GrpcRateLimiter::new(0, 60));
        let mut svc = make_service(limiter);

        for _ in 0..100 {
            let resp = svc
                .ready()
                .await
                .unwrap()
                .call(request_no_ip())
                .await
                .unwrap();
            assert_eq!(grpc_status(&resp), GRPC_STATUS_OK);
        }
    }

    // ── Layer construction ─────────────────────────────────────────────────

    /// `GrpcRateLimitLayer::new` + `Layer::layer` must produce a working
    /// service (smoke test for the `Layer` impl).
    #[tokio::test]
    async fn layer_wraps_inner_service() {
        let limiter = Arc::new(GrpcRateLimiter::new(10, 60));
        let layer = GrpcRateLimitLayer::new(limiter);
        let mut svc: GrpcRateLimitService<OkService> = layer.layer(OkService);

        let resp = svc
            .ready()
            .await
            .unwrap()
            .call(request_with_ip(Ipv4Addr::new(1, 1, 1, 1)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(grpc_status(&resp), GRPC_STATUS_OK);
    }
}
