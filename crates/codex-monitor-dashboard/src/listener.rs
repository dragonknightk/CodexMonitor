use std::future::IntoFuture as _;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::{JoinError, JoinHandle};
use tokio::time::Instant;

use super::DashboardRouter;

pub const DASHBOARD_DEFAULT_ADDRESS: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 4319));
const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Owns the selected loopback listener and its supervised Axum server task.
pub struct DashboardListenerHandle {
    local_address: SocketAddr,
    used_fallback: bool,
    shutdown: watch::Sender<bool>,
    stream_shutdown: Option<watch::Sender<bool>>,
    task: Option<JoinHandle<std::io::Result<()>>>,
    shutdown_started: bool,
}

/// Failure before a Dashboard listener has been started.
#[derive(Debug, thiserror::Error)]
pub enum DashboardListenerStartError {
    #[error("Dashboard listener addresses must be IPv4 loopback addresses")]
    NonLoopbackAddress,
    #[error("Dashboard listener could not bind its preferred loopback address")]
    PreferredBind(#[source] std::io::Error),
    #[error(
        "Dashboard listener could not bind its preferred loopback address ({preferred}) or a fallback loopback port"
    )]
    Bind {
        preferred: std::io::Error,
        #[source]
        fallback: std::io::Error,
    },
    #[error("Dashboard listener could not read its bound loopback address")]
    LocalAddress(#[source] std::io::Error),
    #[error("Dashboard listener unexpectedly bound outside IPv4 loopback")]
    UnsafeBoundAddress,
}

/// Failure while stopping or joining the Dashboard server.
#[derive(Debug, thiserror::Error)]
pub enum DashboardListenerShutdownError {
    #[error("Dashboard listener task stopped unexpectedly")]
    Task(#[source] JoinError),
    #[error("Dashboard listener server failed")]
    Server(#[source] std::io::Error),
    #[error("Dashboard listener exceeded its shutdown drain deadline")]
    DrainTimedOut,
}

impl DashboardListenerHandle {
    /// Return the listener address selected and reserved during startup.
    pub const fn local_address(&self) -> SocketAddr {
        self.local_address
    }

    /// Report whether the preferred port was unavailable and an OS-assigned
    /// loopback port is in use.
    pub const fn used_fallback(&self) -> bool {
        self.used_fallback
    }

    /// Stop accepting new connections and join the server after in-flight
    /// requests have drained.
    ///
    /// # Errors
    ///
    /// Returns an error if the server task failed or the drain deadline elapsed.
    /// A deadline error retains ownership of the task so a higher-level
    /// lifecycle can call shutdown again under its overall deadline.
    pub async fn shutdown(&mut self) -> Result<(), DashboardListenerShutdownError> {
        self.shutdown_with_timeout(SHUTDOWN_DRAIN_TIMEOUT).await
    }

    pub fn begin_shutdown(&mut self) {
        if self.shutdown_started {
            return;
        }
        self.shutdown_started = true;
        if let Some(stream_shutdown) = self.stream_shutdown.as_ref() {
            stream_shutdown.send_replace(true);
        }
        let _ = self.shutdown.send(true);
    }

    async fn shutdown_with_timeout(
        &mut self,
        drain_timeout: Duration,
    ) -> Result<(), DashboardListenerShutdownError> {
        self.begin_shutdown();

        let Some(task) = self.task.as_mut() else {
            return Ok(());
        };
        let Ok(result) = tokio::time::timeout(drain_timeout, task).await else {
            return Err(DashboardListenerShutdownError::DrainTimedOut);
        };
        self.task.take();
        result
            .map_err(DashboardListenerShutdownError::Task)?
            .map_err(DashboardListenerShutdownError::Server)
    }

    pub async fn abort_and_join_before(&mut self, deadline: Instant) {
        self.begin_shutdown();
        let Some(task) = self.task.as_mut() else {
            return;
        };
        task.abort();
        if tokio::time::timeout_at(deadline, task).await.is_ok() {
            self.task.take();
        }
    }
}

impl Drop for DashboardListenerHandle {
    fn drop(&mut self) {
        self.begin_shutdown();
        if let Some(task) = self.task.as_ref() {
            task.abort();
        }
    }
}

/// Bind and spawn the Dashboard server on `127.0.0.1:4319`.
///
/// If the preferred port is unavailable, the operating system selects and
/// atomically reserves an available port on the same IPv4 loopback address.
/// The router factory receives the verified bound address so Host validation
/// and bootstrap URL creation can use the exact fallback port without a race.
/// The returned handle exposes that same address to the resident host.
/// The factory must return the opaque result of
/// [`super::DashboardApi::finish`], so no route or layer can be appended after
/// the security boundary.
///
/// # Errors
///
/// Returns an error when the preferred bind fails for any reason other than an
/// address conflict, when the fallback bind also fails, or when the bound
/// address cannot be verified as IPv4 loopback.
#[must_use = "the handle owns the selected listener and ordered shutdown"]
pub async fn spawn_dashboard_listener<F>(
    build_router: F,
) -> Result<DashboardListenerHandle, DashboardListenerStartError>
where
    F: FnOnce(SocketAddr) -> DashboardRouter,
{
    let SocketAddr::V4(preferred_address) = DASHBOARD_DEFAULT_ADDRESS else {
        unreachable!("the Dashboard default is an IPv4 address");
    };
    spawn_dashboard_listener_service_at(
        move |address| build_router(address).into_parts(),
        preferred_address,
    )
    .await
}

#[cfg(test)]
async fn spawn_dashboard_listener_at<F>(
    build_router: F,
    preferred_address: SocketAddrV4,
) -> Result<DashboardListenerHandle, DashboardListenerStartError>
where
    F: FnOnce(SocketAddr) -> Router,
{
    spawn_dashboard_listener_service_at(
        move |address| (build_router(address), None),
        preferred_address,
    )
    .await
}

async fn spawn_dashboard_listener_service_at<F>(
    build_router: F,
    preferred_address: SocketAddrV4,
) -> Result<DashboardListenerHandle, DashboardListenerStartError>
where
    F: FnOnce(SocketAddr) -> (Router, Option<watch::Sender<bool>>),
{
    if !preferred_address.ip().is_loopback() {
        return Err(DashboardListenerStartError::NonLoopbackAddress);
    }

    let (listener, used_fallback) = match TcpListener::bind(preferred_address).await {
        Ok(listener) => (listener, false),
        Err(preferred) if should_use_fallback(&preferred) => {
            let fallback_address = SocketAddrV4::new(*preferred_address.ip(), 0);
            let listener = TcpListener::bind(fallback_address)
                .await
                .map_err(|fallback| DashboardListenerStartError::Bind {
                    preferred,
                    fallback,
                })?;
            (listener, true)
        }
        Err(error) => return Err(DashboardListenerStartError::PreferredBind(error)),
    };

    let local_address = listener
        .local_addr()
        .map_err(DashboardListenerStartError::LocalAddress)?;
    if !matches!(local_address, SocketAddr::V4(address) if address.ip().is_loopback()) {
        return Err(DashboardListenerStartError::UnsafeBoundAddress);
    }

    let (router, stream_shutdown) = build_router(local_address);
    let (shutdown, shutdown_receiver) = watch::channel(false);
    let task = tokio::spawn(
        axum::serve(listener, router)
            .with_graceful_shutdown(wait_for_shutdown(shutdown_receiver))
            .into_future(),
    );

    Ok(DashboardListenerHandle {
        local_address,
        used_fallback,
        shutdown,
        stream_shutdown,
        task: Some(task),
        shutdown_started: false,
    })
}

fn should_use_fallback(error: &std::io::Error) -> bool {
    error.kind() == ErrorKind::AddrInUse
}

async fn wait_for_shutdown(mut shutdown: watch::Receiver<bool>) {
    if *shutdown.borrow() {
        return;
    }
    while shutdown.changed().await.is_ok() {
        if *shutdown.borrow() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener as BlockingTcpListener;
    use std::sync::{Arc, Mutex};

    use axum::http::StatusCode;
    use axum::routing::get;

    use super::*;
    use crate::{DashboardSessions, dashboard_session_router};

    #[test]
    fn public_listener_address_is_the_required_ipv4_loopback_default() {
        assert_eq!(
            DASHBOARD_DEFAULT_ADDRESS,
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 4319))
        );
    }

    fn test_router() -> Router {
        Router::new().route("/healthz", get(|| async { StatusCode::NO_CONTENT }))
    }

    async fn assert_listener_responds(address: SocketAddr) {
        let response = tokio::task::spawn_blocking(move || {
            let mut stream = std::net::TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .write_all(b"GET /healthz HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        })
        .await
        .unwrap();
        assert!(response.starts_with("HTTP/1.1 204"), "{response}");
    }

    async fn raw_request(address: SocketAddr, request: String) -> String {
        tokio::task::spawn_blocking(move || {
            let mut stream = std::net::TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        })
        .await
        .unwrap()
    }

    #[test]
    fn only_address_conflicts_enable_port_fallback() {
        assert!(should_use_fallback(&std::io::Error::from(
            ErrorKind::AddrInUse
        )));
        for kind in [
            ErrorKind::PermissionDenied,
            ErrorKind::AddrNotAvailable,
            ErrorKind::InvalidInput,
        ] {
            assert!(!should_use_fallback(&std::io::Error::from(kind)));
        }
    }

    #[tokio::test]
    async fn available_preferred_loopback_port_is_retained() {
        let preferred_address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0);
        let router_address = Arc::new(Mutex::new(None));
        let captured_address = Arc::clone(&router_address);
        let mut listener = spawn_dashboard_listener_at(
            move |address| {
                *captured_address.lock().unwrap() = Some(address);
                test_router()
            },
            preferred_address,
        )
        .await
        .unwrap();

        assert!(listener.local_address().ip().is_loopback());
        assert_ne!(listener.local_address().port(), 0);
        assert!(!listener.used_fallback());
        assert_eq!(
            *router_address.lock().unwrap(),
            Some(listener.local_address())
        );
        assert_listener_responds(listener.local_address()).await;

        let address = listener.local_address();
        listener.shutdown().await.unwrap();
        listener.shutdown().await.unwrap();
        assert!(std::net::TcpStream::connect(address).is_err());
    }

    #[tokio::test]
    async fn occupied_preferred_port_falls_back_without_releasing_the_selected_port() {
        let blocker = BlockingTcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let SocketAddr::V4(preferred_address) = blocker.local_addr().unwrap() else {
            panic!("test listener must use IPv4");
        };

        let router_address = Arc::new(Mutex::new(None));
        let captured_address = Arc::clone(&router_address);
        let mut listener = spawn_dashboard_listener_at(
            move |address| {
                *captured_address.lock().unwrap() = Some(address);
                test_router()
            },
            preferred_address,
        )
        .await
        .unwrap();
        let selected_address = listener.local_address();

        assert!(listener.used_fallback());
        assert!(selected_address.ip().is_loopback());
        assert_ne!(selected_address.port(), preferred_address.port());
        assert_eq!(*router_address.lock().unwrap(), Some(selected_address));
        assert!(BlockingTcpListener::bind(selected_address).is_err());
        assert_listener_responds(selected_address).await;

        listener.shutdown().await.unwrap();
        drop(blocker);
    }

    #[tokio::test]
    async fn fallback_listener_uses_only_its_selected_authority_for_bootstrap() {
        let blocker = BlockingTcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let SocketAddr::V4(preferred_address) = blocker.local_addr().unwrap() else {
            panic!("test listener must use IPv4");
        };
        let sessions = DashboardSessions::default();
        let token = sessions.issue_bootstrap().unwrap().query_value().to_owned();
        let router_sessions = sessions.clone();
        let mut listener = spawn_dashboard_listener_at(
            move |address| dashboard_session_router(address, router_sessions).into_inner(),
            preferred_address,
        )
        .await
        .unwrap();
        let selected_address = listener.local_address();
        assert!(listener.used_fallback());

        let rejected = raw_request(
            selected_address,
            format!(
                "GET /api/v1/session/bootstrap?token={token} HTTP/1.1\r\nHost: {preferred_address}\r\nConnection: close\r\n\r\n"
            ),
        )
        .await;
        assert!(rejected.starts_with("HTTP/1.1 403"), "{rejected}");

        let accepted = raw_request(
            selected_address,
            format!(
                "GET /api/v1/session/bootstrap?token={token} HTTP/1.1\r\nHost: {selected_address}\r\nConnection: close\r\n\r\n"
            ),
        )
        .await;
        assert!(accepted.starts_with("HTTP/1.1 303"));

        listener.shutdown().await.unwrap();
        drop(blocker);
    }

    #[tokio::test]
    async fn non_loopback_bind_targets_are_rejected_before_socket_creation() {
        let result = spawn_dashboard_listener_at(
            |_| Router::new(),
            SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0),
        )
        .await;

        assert!(matches!(
            result,
            Err(DashboardListenerStartError::NonLoopbackAddress)
        ));
    }

    #[tokio::test]
    async fn drain_timeout_keeps_the_supervised_task_for_a_later_join() {
        let (shutdown, _) = watch::channel(false);
        let (stream_shutdown, stream_shutdown_monitor) = watch::channel(false);
        let mut listener = DashboardListenerHandle {
            local_address: DASHBOARD_DEFAULT_ADDRESS,
            used_fallback: false,
            shutdown,
            stream_shutdown: Some(stream_shutdown),
            task: Some(tokio::spawn(std::future::pending::<std::io::Result<()>>())),
            shutdown_started: false,
        };

        let error = listener
            .shutdown_with_timeout(Duration::from_millis(1))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            DashboardListenerShutdownError::DrainTimedOut
        ));
        assert!(listener.task.is_some());
        assert!(*stream_shutdown_monitor.borrow());

        listener.task.as_ref().unwrap().abort();
        let error = listener
            .shutdown_with_timeout(Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(matches!(error, DashboardListenerShutdownError::Task(_)));
        assert!(listener.task.is_none());
    }

    #[tokio::test]
    async fn dropping_without_shutdown_aborts_the_server_and_releases_the_port() {
        let listener = spawn_dashboard_listener_at(
            |_| Router::new(),
            SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0),
        )
        .await
        .unwrap();
        let address = listener.local_address();

        drop(listener);

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if BlockingTcpListener::bind(address).is_ok() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dropping the owner must not detach a live Dashboard server");
    }
}
