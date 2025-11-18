use crate::config::Config;
use anyhow::Result;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tracing::{error, info};

pub struct FileTransferServer;

impl FileTransferServer {
    pub async fn run(port: u16, config: Arc<RwLock<Config>>) -> Result<()> {
        let addr = format!("0.0.0.0:{}", port);
        let listener = TcpListener::bind(&addr).await?;
        info!("File transfer server listening on {}", addr);

        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    info!("File transfer connection from {}", addr);
                    let config = config.clone();

                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_transfer(stream, config).await {
                            error!("File transfer error from {}: {}", addr, e);
                        }
                    });
                }
                Err(e) => {
                    error!("Failed to accept file transfer connection: {}", e);
                }
            }
        }
    }

    async fn handle_transfer(
        mut stream: tokio::net::TcpStream,
        config: Arc<RwLock<Config>>,
    ) -> Result<()> {
        // TODO: Implement file transfer protocol
        // This is a placeholder for the full implementation
        info!("File transfer connection established");
        Ok(())
    }
}
