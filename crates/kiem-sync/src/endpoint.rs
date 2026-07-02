use iroh::{
    endpoint::{presets, BindError, ConnectError, ConnectingError, Connection},
    Endpoint, EndpointAddr, SecretKey,
};

/// ALPN identifying kiem's sync protocol. Bumped only on a wire-incompatible
/// change to what's carried over the stream (see kiem-core's `protocol.rs`).
pub const ALPN: &[u8] = b"kiem-sync/0";

#[derive(thiserror::Error, Debug)]
pub enum EndpointError {
    #[error("binding iroh endpoint: {0}")]
    Bind(#[from] BindError),
    #[error("connecting to peer: {0}")]
    Connect(#[from] ConnectError),
    #[error("accepting an incoming connection: {0}")]
    Accept(#[from] ConnectingError),
}

/// Binds a device-identified iroh `Endpoint` speaking the kiem sync ALPN, using
/// the default n0 discovery + relay preset (dial-by-`EndpointId`, no manual
/// address bookkeeping; falls back to the public relay when a direct QUIC path
/// can't be hole-punched).
pub async fn bind(secret_key: SecretKey) -> Result<Endpoint, EndpointError> {
    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(secret_key)
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await?;
    Ok(endpoint)
}

pub async fn connect(endpoint: &Endpoint, addr: EndpointAddr) -> Result<Connection, EndpointError> {
    let connection = endpoint.connect(addr, ALPN).await?;
    Ok(connection)
}

/// Accepts the next incoming connection for kiem's ALPN. Callers loop this
/// alongside their own shutdown signal; a `None` return means the endpoint
/// closed.
pub async fn accept(endpoint: &Endpoint) -> Result<Option<Connection>, EndpointError> {
    let Some(connecting) = endpoint.accept().await else {
        return Ok(None);
    };
    let connection = connecting.await?;
    Ok(Some(connection))
}
