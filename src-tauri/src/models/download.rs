use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use futures_util::StreamExt;
use reqwest::StatusCode;
use reqwest::header::{CONTENT_RANGE, RANGE};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use super::catalog::CatalogModel;
use super::store::ModelStore;

/// Space kept free beyond the model itself so the disk is never filled completely.
const DISK_MARGIN: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DownloadError {
    #[error("not enough disk space: {needed} bytes needed, {available} available")]
    InsufficientSpace { needed: u64, available: u64 },
    #[error("download interrupted: {0}")]
    Interrupted(String),
    #[error("server returned {0}")]
    Http(u16),
    #[error("checksum mismatch")]
    Verification,
    #[error("file error: {0}")]
    Io(String),
    #[error("download stopped")]
    Stopped,
}

impl From<io::Error> for DownloadError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

pub fn check_space(needed: u64, available: u64) -> Result<(), DownloadError> {
    if available < needed.saturating_add(DISK_MARGIN) {
        return Err(DownloadError::InsufficientSpace { needed, available });
    }
    Ok(())
}

/// Downloads `model` into the store, resuming any partial file. The model is only
/// installed after its SHA-256 matches the catalogue. Stopping keeps the partial file.
pub async fn download(
    http: &reqwest::Client,
    model: &CatalogModel,
    store: &ModelStore,
    stop: &CancellationToken,
    mut progress: impl FnMut(u64),
) -> Result<(), DownloadError> {
    if store.is_installed(model) {
        return Ok(());
    }
    let partial = store.partial_path(model);
    let mut offset = store.partial_len(model);
    if offset > model.size {
        store.discard_partial(model)?;
        offset = 0;
    }
    check_space(model.size - offset, fs4::available_space(store.dir())?)?;

    let mut hasher = hash_prefix(&partial, offset).await?;
    if offset < model.size {
        let mut request = http.get(&model.url);
        if offset > 0 {
            request = request.header(RANGE, format!("bytes={offset}-"));
        }
        let response = tokio::select! {
            () = stop.cancelled() => return Err(DownloadError::Stopped),
            response = request.send() => {
                response.map_err(|error| DownloadError::Interrupted(error.to_string()))?
            }
        };
        let resumed = response.status() == StatusCode::PARTIAL_CONTENT
            && content_range_start(&response) == Some(offset);
        if !resumed {
            if !response.status().is_success() {
                return Err(DownloadError::Http(response.status().as_u16()));
            }
            // The server ignored the range, so the download starts over.
            offset = 0;
            hasher = Sha256::new();
        }

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(resumed)
            .truncate(!resumed)
            .open(&partial)
            .await?;
        progress(offset);
        let mut body = response.bytes_stream();
        loop {
            let chunk = tokio::select! {
                biased;
                () = stop.cancelled() => {
                    file.flush().await?;
                    return Err(DownloadError::Stopped);
                }
                chunk = body.next() => chunk,
            };
            let Some(chunk) = chunk else { break };
            let bytes = chunk.map_err(|error| DownloadError::Interrupted(error.to_string()))?;
            offset += bytes.len() as u64;
            if offset > model.size {
                drop(file);
                store.discard_partial(model)?;
                return Err(DownloadError::Verification);
            }
            file.write_all(&bytes).await?;
            hasher.update(&bytes);
            progress(offset);
        }
        file.sync_all().await?;
        if offset != model.size {
            return Err(DownloadError::Interrupted(format!(
                "received {offset} of {} bytes",
                model.size
            )));
        }
    }

    if hex(&hasher.finalize()) != model.sha256 {
        store.discard_partial(model)?;
        return Err(DownloadError::Verification);
    }
    store.install(model, &partial)?;
    Ok(())
}

fn content_range_start(response: &reqwest::Response) -> Option<u64> {
    let value = response.headers().get(CONTENT_RANGE)?.to_str().ok()?;
    let range = value.strip_prefix("bytes ")?;
    range.split('-').next()?.parse().ok()
}

/// Hashes the bytes already on disk so a resumed download is still fully verified.
async fn hash_prefix(path: &Path, len: u64) -> Result<Sha256, DownloadError> {
    if len == 0 {
        return Ok(Sha256::new());
    }
    let path = path.to_owned();
    let hashing = tokio::task::spawn_blocking(move || -> io::Result<Sha256> {
        let mut hasher = Sha256::new();
        let mut reader = File::open(path)?.take(len);
        let mut buffer = vec![0; 1024 * 1024];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                return Ok(hasher);
            }
            hasher.update(&buffer[..read]);
        }
    });
    Ok(hashing
        .await
        .map_err(|error| DownloadError::Io(error.to_string()))??)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
pub mod test_server {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[derive(Clone, Copy, Default)]
    pub struct Behaviour {
        /// Close the connection after sending this many body bytes on the first request.
        pub cut_first_response_after: Option<usize>,
        /// Keep the first connection open after the cut instead of closing it.
        pub stall_first_response: bool,
        pub ignore_range: bool,
        pub status: Option<u16>,
    }

    /// Minimal HTTP server with range support that counts the requests it receives.
    pub async fn serve(body: Vec<u8>, behaviour: Behaviour) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/model.gguf", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let index = counter.fetch_add(1, Ordering::SeqCst);
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if socket.read(&mut byte).await.unwrap_or(0) == 0 {
                        break;
                    }
                    head.push(byte[0]);
                }
                let head = String::from_utf8_lossy(&head).to_lowercase();
                let start = head
                    .lines()
                    .find_map(|line| line.strip_prefix("range: bytes="))
                    .and_then(|range| range.trim_end_matches('-').parse::<usize>().ok())
                    .filter(|_| !behaviour.ignore_range);
                let (status, slice) = match (behaviour.status, start) {
                    (Some(status), _) => (format!("{status} Error"), &body[..0]),
                    (None, Some(start)) => ("206 Partial Content".to_owned(), &body[start..]),
                    (None, None) => ("200 OK".to_owned(), &body[..]),
                };
                let range = start
                    .map(|start| {
                        format!(
                            "Content-Range: bytes {start}-{}/{}\r\n",
                            body.len() - 1,
                            body.len()
                        )
                    })
                    .unwrap_or_default();
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{range}Connection: close\r\n\r\n",
                    slice.len()
                );
                let _ = socket.write_all(header.as_bytes()).await;
                let sent = match behaviour.cut_first_response_after {
                    Some(limit) if index == 0 => &slice[..limit.min(slice.len())],
                    _ => slice,
                };
                let _ = socket.write_all(sent).await;
                if behaviour.stall_first_response && index == 0 {
                    tokio::spawn(async move {
                        std::future::pending::<()>().await;
                        drop(socket);
                    });
                    continue;
                }
                let _ = socket.shutdown().await;
            }
        });
        (url, requests)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::test_server::{Behaviour, serve};
    use super::*;
    use crate::models::store::tests::{TempStore, small_model};

    fn body() -> Vec<u8> {
        (0..200_000u32).map(|value| (value % 251) as u8).collect()
    }

    fn client() -> reqwest::Client {
        crate::tls::install_crypto_provider();
        reqwest::Client::new()
    }

    async fn fetch(model: &CatalogModel, store: &ModelStore) -> Result<(), DownloadError> {
        download(&client(), model, store, &CancellationToken::new(), |_| {}).await
    }

    #[tokio::test]
    async fn downloads_verifies_and_installs() {
        let temp = TempStore::new("download-ok");
        let (url, _) = serve(body(), Behaviour::default()).await;
        let model = CatalogModel {
            url,
            ..small_model(&body())
        };

        let mut reported = Vec::new();
        download(
            &client(),
            &model,
            &temp.store,
            &CancellationToken::new(),
            |bytes| reported.push(bytes),
        )
        .await
        .unwrap();

        assert!(temp.store.is_installed(&model));
        assert_eq!(
            std::fs::read(temp.store.model_path(&model)).unwrap(),
            body()
        );
        assert_eq!(reported.last(), Some(&model.size));
        assert_eq!(temp.store.partial_len(&model), 0);
    }

    #[tokio::test]
    async fn interrupted_downloads_keep_progress_and_resume_with_a_range_request() {
        let temp = TempStore::new("download-resume");
        let behaviour = Behaviour {
            cut_first_response_after: Some(70_000),
            ..Behaviour::default()
        };
        let (url, requests) = serve(body(), behaviour).await;
        let model = CatalogModel {
            url,
            ..small_model(&body())
        };

        let first = fetch(&model, &temp.store).await;
        assert!(matches!(first, Err(DownloadError::Interrupted(_))));
        assert!(!temp.store.is_installed(&model));
        assert_eq!(temp.store.partial_len(&model), 70_000);

        fetch(&model, &temp.store).await.unwrap();
        assert!(temp.store.is_installed(&model));
        assert_eq!(
            std::fs::read(temp.store.model_path(&model)).unwrap(),
            body()
        );
        assert_eq!(requests.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn servers_without_range_support_restart_from_zero() {
        let temp = TempStore::new("download-no-range");
        let (url, _) = serve(
            body(),
            Behaviour {
                ignore_range: true,
                ..Behaviour::default()
            },
        )
        .await;
        let model = CatalogModel {
            url,
            ..small_model(&body())
        };
        std::fs::write(temp.store.partial_path(&model), &body()[..1000]).unwrap();

        fetch(&model, &temp.store).await.unwrap();
        assert_eq!(
            std::fs::read(temp.store.model_path(&model)).unwrap(),
            body()
        );
    }

    #[tokio::test]
    async fn checksum_mismatch_discards_the_download() {
        let temp = TempStore::new("download-corrupt");
        let (url, _) = serve(body(), Behaviour::default()).await;
        let model = CatalogModel {
            url,
            sha256: "0".repeat(64),
            ..small_model(&body())
        };

        assert_eq!(
            fetch(&model, &temp.store).await,
            Err(DownloadError::Verification)
        );
        assert!(!temp.store.is_installed(&model));
        assert!(!temp.store.model_path(&model).exists());
        assert_eq!(temp.store.partial_len(&model), 0);
    }

    #[tokio::test]
    async fn corrupted_partial_file_fails_verification_after_resume() {
        let temp = TempStore::new("download-bad-partial");
        let (url, _) = serve(body(), Behaviour::default()).await;
        let model = CatalogModel {
            url,
            ..small_model(&body())
        };
        std::fs::write(temp.store.partial_path(&model), vec![9u8; 1000]).unwrap();

        assert_eq!(
            fetch(&model, &temp.store).await,
            Err(DownloadError::Verification)
        );
        assert!(!temp.store.is_installed(&model));
    }

    #[tokio::test]
    async fn stopping_keeps_the_partial_file_and_installs_nothing() {
        let temp = TempStore::new("download-stop");
        let (url, _) = serve(body(), Behaviour::default()).await;
        let model = CatalogModel {
            url,
            ..small_model(&body())
        };
        let stop = CancellationToken::new();
        stop.cancel();

        let result = download(&client(), &model, &temp.store, &stop, |_| {}).await;
        assert_eq!(result, Err(DownloadError::Stopped));
        assert!(!temp.store.is_installed(&model));
    }

    #[tokio::test]
    async fn http_errors_are_reported() {
        let temp = TempStore::new("download-404");
        let (url, _) = serve(
            body(),
            Behaviour {
                status: Some(404),
                ..Behaviour::default()
            },
        )
        .await;
        let model = CatalogModel {
            url,
            ..small_model(&body())
        };
        assert_eq!(
            fetch(&model, &temp.store).await,
            Err(DownloadError::Http(404))
        );
    }

    #[tokio::test]
    async fn installed_models_are_not_downloaded_again() {
        let temp = TempStore::new("download-installed");
        let (url, requests) = serve(body(), Behaviour::default()).await;
        let model = CatalogModel {
            url,
            ..small_model(&body())
        };
        fetch(&model, &temp.store).await.unwrap();
        fetch(&model, &temp.store).await.unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn insufficient_disk_space_is_rejected() {
        let gigabyte = 1024 * 1024 * 1024;
        assert!(check_space(5 * gigabyte, 10 * gigabyte).is_ok());
        assert_eq!(
            check_space(5 * gigabyte, 5 * gigabyte),
            Err(DownloadError::InsufficientSpace {
                needed: 5 * gigabyte,
                available: 5 * gigabyte
            })
        );
    }
}
