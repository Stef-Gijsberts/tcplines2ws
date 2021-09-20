use std::env;
use std::io;
use std::fmt;

use futures_util::{future, StreamExt, TryFutureExt};
use log::{error, info};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};

use tokio_util::codec::{Framed, LinesCodec};

use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::WebSocketStream as WsStream;

#[derive(Debug)]
enum Error {
    WebSocketHandshake(WsError),
    ConnectToTarget(io::Error),
    Bind(io::Error),
    ForwardingToTarget(tokio_util::codec::LinesCodecError),
    ForwardingToClient(WsError),
    InvalidArguments
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::WebSocketHandshake(e) => write!(f, "WebSocket handshake failed: {}", e),
            Error::ConnectToTarget(e) => write!(f, "Could not connect to target: {}", e),
            Error::Bind(e) => write!(f, "Could not bind to serve address: {}", e),
            Error::ForwardingToTarget(e) => write!(f, "Could not forward to target: {}", e),
            Error::ForwardingToClient(e) => write!(f, "Could not forward to client: {}", e),
            Error::InvalidArguments => write!(f, "Invalid arguments provided.\n\tExample: tcplines2ws 127.0.0.1:8080 127.0.0.1:6667\n\tUSAGE: tcplines2ws <SERVE ADDRESS> <TARGET ADDRESS>"),
        }
    }
}

fn main() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let result = run().await;

            if let Err(e) = result {
                eprintln!("Error: {}", e);
            }
        })
}

async fn run() -> Result<(), Error> {
    simple_logging::log_to_stderr(log::LevelFilter::Info);

    let (serve_addr, target_addr) = {
        let mut args = env::args().skip(1);
        let serve_addr = args.next().ok_or(Error::InvalidArguments)?;
        let target_addr = args.next().ok_or(Error::InvalidArguments)?;
        (serve_addr, target_addr)
    };

    let listener = TcpListener::bind(&serve_addr).await.map_err(Error::Bind)?;
    info!("Listening on: {}", serve_addr);

    while let Ok((stream, _)) = listener.accept().await {
        let target_addr_clone = target_addr.clone();

        tokio::spawn(async move {
            let result = accept_connection(stream, &target_addr_clone).await;

            if let Err(e) = result {
                error!("{}", e);
            }
        });
    }

    Ok(())
}

async fn accept_connection(incoming_tcp_stream: TcpStream, target_addr: &str) -> Result<(), Error> {
    let outgoing_tcp_stream = TcpStream::connect(target_addr)
        .await
        .map_err(Error::ConnectToTarget)?;

    let ws_stream = make_websocket_stream(incoming_tcp_stream).await?;
    let lines_stream = make_linebased_stream(outgoing_tcp_stream).await?;

    info!("Starting tunnelling");

    tunnel(ws_stream, lines_stream).await
}

async fn tunnel(
    ws_stream: WsStream<impl AsyncRead + AsyncWrite + Unpin>,
    line_stream: Framed<impl AsyncRead + AsyncWrite, LinesCodec>,
) -> Result<(), Error> {
    let (ws_write, ws_read) = ws_stream.split();
    let (line_write, line_read) = line_stream.split::<String>();

    let ws_to_line = ws_read
        .filter_map(|msg| async move {
            match msg {
                Ok(WsMessage::Text(text)) => Some(text),
                Err(e) => {
                    error!("{}", e);
                    None
                },
                _ => None,
            }
        })
        .map(Ok)
        .forward(line_write)
        .map_err(Error::ForwardingToTarget);

    let line_to_ws = line_read
        .filter_map(|msg| async move {
            match msg {
                Ok(text) => Some(text),
                Err(_) => panic!(),
            }
        })
        .map(WsMessage::Text)
        .map(Ok)
        .forward(ws_write)
        .map_err(Error::ForwardingToClient);

    future::try_join(line_to_ws, ws_to_line).await.map(|_| ())
}

async fn make_websocket_stream(
    tcp_stream: TcpStream,
) -> Result<WsStream<impl AsyncRead + AsyncWrite + Unpin>, Error> {
    tokio_tungstenite::accept_async(tcp_stream)
        .await
        .map_err(Error::WebSocketHandshake)
}

async fn make_linebased_stream(
    tcp_stream: TcpStream,
) -> Result<Framed<impl AsyncRead + AsyncWrite, LinesCodec>, Error> {
    Ok(Framed::new(tcp_stream, LinesCodec::new()))
}
