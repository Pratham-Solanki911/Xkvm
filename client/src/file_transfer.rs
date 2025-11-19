use anyhow::Result;
use kvm_common::{read_envelope, send_envelope, Envelope};
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::TcpStream;
use uuid::Uuid;
use sha2::{Sha256, Digest};

pub async fn send_file(server_addr: &str, file_path: &Path) -> Result<()> {
    let mut stream = TcpStream::connect(server_addr).await?;

    let file_name = file_path.file_name().unwrap().to_str().unwrap().to_string();
    let file_size = tokio::fs::metadata(file_path).await?.len();

    let mut file = File::open(file_path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 65536];

    loop {
        let bytes_read = file.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    let sha256 = hasher.finalize();

    let offer = Envelope::FileOffer {
        id: Uuid::new_v4(),
        name: file_name,
        size: file_size,
        sha256: sha256.into(),
    };

    send_envelope(&mut stream, &offer).await?;

    let accept = read_envelope(&mut stream).await?;

    if let Envelope::FileAccept { id, start_offset } = accept {
        info!("Server accepted file transfer for '{}', starting at offset {}", file_name, start_offset);
        let mut file = File::open(file_path).await?;
        if start_offset > 0 {
            file.seek(std::io::SeekFrom::Start(start_offset)).await?;
        }
        tokio::io::copy(&mut file, &mut stream).await?;
    } else {
        anyhow::bail!("File transfer rejected");
    }

    Ok(())
}
