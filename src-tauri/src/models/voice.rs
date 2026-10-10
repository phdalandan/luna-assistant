//! Downloads and installs the speech models voice needs, as one download the user starts.
//! Each file is verified against its catalogue SHA-256 before it counts as installed.
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;
use tokio_util::sync::CancellationToken;

use super::download::{self, DownloadError};
use super::speech::{KeywordModel, SpeechCatalog};
use super::store::ModelStore;
use super::{
    DownloadPhase, DownloadProgress, DownloadStatus, ModelEvents, PROGRESS_INTERVAL,
    download_message, lock,
};
use crate::voice::SpeechFiles;

/// The progress event id for the voice download.
const VOICE_DOWNLOAD_ID: &str = "voice";
const WAKE_WORD_DIR: &str = "wake-word";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct VoiceModelsInfo {
    pub installed: bool,
    #[cfg_attr(test, ts(type = "number"))]
    pub size: u64,
    pub download: Option<DownloadStatus>,
}

enum Activity {
    Idle,
    Running {
        stop: CancellationToken,
        downloaded: u64,
    },
    Failed(String),
}

pub struct VoiceModels {
    speech: &'static SpeechCatalog,
    store: ModelStore,
    http: reqwest::Client,
    activity: Mutex<Activity>,
    events: Arc<dyn ModelEvents>,
}

impl VoiceModels {
    pub fn new(
        speech: &'static SpeechCatalog,
        store: ModelStore,
        http: reqwest::Client,
        events: Arc<dyn ModelEvents>,
    ) -> Self {
        Self {
            speech,
            store,
            http,
            activity: Mutex::new(Activity::Idle),
            events,
        }
    }

    fn total_size(&self) -> u64 {
        self.speech.files().iter().map(|file| file.size).sum()
    }

    fn wake_word_dir(&self) -> PathBuf {
        self.store.dir().join(WAKE_WORD_DIR)
    }

    /// The installed model files, or `None` until the user has downloaded them.
    pub fn files(&self) -> Option<SpeechFiles> {
        let verified = self
            .speech
            .files()
            .iter()
            .all(|file| self.store.is_installed(*file));
        let wake_word_dir = self.wake_word_dir();
        let extracted = self
            .speech
            .wake_word
            .extracted_files()
            .iter()
            .all(|name| wake_word_dir.join(name).is_file());
        (verified && extracted).then(|| SpeechFiles {
            wake_word_dir,
            speech_detection: self.store.model_path(&self.speech.speech_detection.file),
            transcription: self.store.model_path(&self.speech.transcription.file),
        })
    }

    pub fn info(&self) -> VoiceModelsInfo {
        let size = self.total_size();
        let download = match &*lock(&self.activity) {
            Activity::Idle => None,
            Activity::Running { downloaded, .. } => Some(DownloadStatus {
                phase: if *downloaded >= size {
                    DownloadPhase::Verifying
                } else {
                    DownloadPhase::Downloading
                },
                downloaded: *downloaded,
                error: None,
            }),
            Activity::Failed(message) => Some(DownloadStatus {
                phase: DownloadPhase::Failed,
                downloaded: 0,
                error: Some(message.clone()),
            }),
        };
        VoiceModelsInfo {
            installed: self.files().is_some(),
            size,
            download,
        }
    }

    /// Starts the download the user asked for. Interrupted files resume where they stopped.
    pub fn start_download(self: &Arc<Self>) {
        let stop = CancellationToken::new();
        {
            let mut activity = lock(&self.activity);
            if matches!(*activity, Activity::Running { .. }) {
                return;
            }
            *activity = Activity::Running {
                stop: stop.clone(),
                downloaded: 0,
            };
        }
        self.events.changed();
        let models = self.clone();
        tauri::async_runtime::spawn(async move {
            let result = models.download_all(&stop).await;
            let mut activity = lock(&models.activity);
            *activity = match result {
                Ok(()) => {
                    log::info!("installed the voice models");
                    Activity::Idle
                }
                Err(DownloadError::Stopped) => Activity::Idle,
                Err(error) => {
                    log::error!("voice model download failed: {error}");
                    Activity::Failed(download_message(&error))
                }
            };
            drop(activity);
            models.events.changed();
        });
    }

    pub fn cancel_download(&self) {
        if let Activity::Running { stop, .. } = &*lock(&self.activity) {
            stop.cancel();
        }
    }

    async fn download_all(&self, stop: &CancellationToken) -> Result<(), DownloadError> {
        let mut finished = 0;
        let mut last_report = Instant::now();
        for file in self.speech.files() {
            download::download(&self.http, file, &self.store, stop, |downloaded| {
                let total = finished + downloaded;
                if let Activity::Running { downloaded, .. } = &mut *lock(&self.activity) {
                    *downloaded = total;
                }
                if last_report.elapsed() >= PROGRESS_INTERVAL {
                    last_report = Instant::now();
                    self.events.progress(DownloadProgress {
                        id: VOICE_DOWNLOAD_ID.into(),
                        downloaded: total,
                    });
                }
            })
            .await?;
            finished += file.size;
        }
        let archive = self.store.model_path(&self.speech.wake_word.archive);
        let target = self.wake_word_dir();
        let wake_word = &self.speech.wake_word;
        tauri::async_runtime::spawn_blocking({
            let wake_word = wake_word.clone();
            move || extract(&archive, &target, &wake_word)
        })
        .await
        .map_err(|error| DownloadError::Io(error.to_string()))??;
        Ok(())
    }
}

/// Copies the model files named in the catalogue out of the verified archive. Paths inside
/// the archive are never used, only the file names the catalogue lists.
fn extract(archive: &Path, target: &Path, model: &KeywordModel) -> Result<(), DownloadError> {
    fs::create_dir_all(target)?;
    let wanted = model.extracted_files();
    let reader = bzip2::read::BzDecoder::new(File::open(archive)?);
    let mut entries = tar::Archive::new(reader);
    let mut found = 0;
    for entry in entries.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !entry.header().entry_type().is_file() || !wanted.contains(&name) {
            continue;
        }
        let partial = target.join(format!("{name}.part"));
        io::copy(&mut entry, &mut File::create(&partial)?)?;
        fs::rename(&partial, target.join(name))?;
        found += 1;
    }
    if found != wanted.len() {
        return Err(DownloadError::Verification);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::catalog;

    fn archive(dir: &Path, files: &[(&str, &[u8])]) -> PathBuf {
        let path = dir.join("model.tar.bz2");
        let encoder =
            bzip2::write::BzEncoder::new(File::create(&path).unwrap(), bzip2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (name, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, *content).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
        path
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("luna-voice-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn extracts_only_the_catalogue_files_by_name() {
        let model = &catalog::speech().wake_word;
        let dir = temp_dir("extract");
        let mut files: Vec<(String, &[u8])> = model
            .extracted_files()
            .iter()
            .map(|name| (format!("model-dir/{name}"), b"model".as_slice()))
            .collect();
        files.push(("model-dir/test_wavs/0.wav".into(), b"audio"));
        let named: Vec<(&str, &[u8])> = files.iter().map(|(n, c)| (n.as_str(), *c)).collect();
        let archive = archive(&dir, &named);
        let target = dir.join("wake-word");

        extract(&archive, &target, model).unwrap();

        let mut extracted: Vec<String> = fs::read_dir(&target)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        extracted.sort();
        let mut expected: Vec<String> = model
            .extracted_files()
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        expected.sort();
        assert_eq!(extracted, expected);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_archive_missing_a_model_file_is_rejected() {
        let model = &catalog::speech().wake_word;
        let dir = temp_dir("incomplete");
        let archive = archive(&dir, &[("model-dir/tokens.txt", b"tokens")]);
        assert_eq!(
            extract(&archive, &dir.join("wake-word"), model),
            Err(DownloadError::Verification)
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
