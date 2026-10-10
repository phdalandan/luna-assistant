//! Downloads and installs the speech models voice needs, as one download the user starts.
//! Each file is verified against its catalogue SHA-256 before it counts as installed.
use std::fs::{self, File};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;
use tokio_util::sync::CancellationToken;

use super::download::{self, DownloadError};
use super::speech::{DownloadFile, SpeechCatalog};
use super::store::ModelStore;
use super::{
    DownloadPhase, DownloadProgress, DownloadStatus, ModelEvents, PROGRESS_INTERVAL,
    download_message, lock,
};
use crate::voice::SpeechFiles;

/// The progress event id for the voice download.
const VOICE_DOWNLOAD_ID: &str = "voice";
const WAKE_WORD_DIR: &str = "wake-word";
const SPEECH_OUTPUT_DIR: &str = "speech-output";
/// Records which verified archive a folder was extracted from; the archive is then deleted.
const EXTRACTED_MARKER: &str = "extracted";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct VoiceModelsInfo {
    pub installed: bool,
    #[cfg_attr(test, ts(type = "number"))]
    pub size: u64,
    pub download: Option<DownloadStatus>,
}

/// An archive and the files and folders taken from it.
struct Unpack<'a> {
    archive: &'a DownloadFile,
    dir: PathBuf,
    files: Vec<&'a str>,
    dirs: Vec<&'a str>,
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

    /// Files used as downloaded, as opposed to archives that are extracted.
    fn plain_files(&self) -> [&DownloadFile; 2] {
        [
            &self.speech.speech_detection.file,
            &self.speech.transcription.file,
        ]
    }

    fn unpacks(&self) -> [Unpack<'_>; 2] {
        let wake_word = &self.speech.wake_word;
        let output = &self.speech.speech_output;
        [
            Unpack {
                archive: &wake_word.archive,
                dir: self.store.dir().join(WAKE_WORD_DIR),
                files: wake_word.extracted_files().to_vec(),
                dirs: Vec::new(),
            },
            Unpack {
                archive: &output.archive,
                dir: self.store.dir().join(SPEECH_OUTPUT_DIR),
                files: output.extracted_files(),
                dirs: vec![&output.data_dir],
            },
        ]
    }

    fn is_unpacked(unpack: &Unpack<'_>) -> bool {
        let marker = fs::read_to_string(unpack.dir.join(EXTRACTED_MARKER));
        marker.is_ok_and(|sha| sha.trim() == unpack.archive.sha256)
            && unpack
                .files
                .iter()
                .all(|name| unpack.dir.join(name).is_file())
            && unpack
                .dirs
                .iter()
                .all(|name| unpack.dir.join(name).is_dir())
    }

    /// The installed model files, or `None` until the user has downloaded them.
    pub fn files(&self) -> Option<SpeechFiles> {
        let verified = self
            .plain_files()
            .iter()
            .all(|file| self.store.is_installed(*file));
        let [wake_word, speech_output] = self.unpacks();
        let unpacked = Self::is_unpacked(&wake_word) && Self::is_unpacked(&speech_output);
        (verified && unpacked).then(|| SpeechFiles {
            wake_word_dir: wake_word.dir,
            speech_detection: self.store.model_path(&self.speech.speech_detection.file),
            transcription: self.store.model_path(&self.speech.transcription.file),
            speech_output_dir: speech_output.dir,
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
        for file in self.plain_files() {
            self.download(file, finished, stop).await?;
            finished += file.size;
        }
        for unpack in self.unpacks() {
            if !Self::is_unpacked(&unpack) {
                self.download(unpack.archive, finished, stop).await?;
                let archive = self.store.model_path(unpack.archive);
                let (dir, sha) = (unpack.dir.clone(), unpack.archive.sha256.clone());
                let files: Vec<String> =
                    unpack.files.iter().map(|name| (*name).to_owned()).collect();
                let dirs: Vec<String> = unpack.dirs.iter().map(|name| (*name).to_owned()).collect();
                tauri::async_runtime::spawn_blocking(move || {
                    extract(&archive, &dir, &files, &dirs)?;
                    fs::write(dir.join(EXTRACTED_MARKER), sha)
                        .map_err(|error| DownloadError::Io(error.to_string()))
                })
                .await
                .map_err(|error| DownloadError::Io(error.to_string()))??;
                self.store.delete(unpack.archive)?;
            }
            finished += unpack.archive.size;
        }
        self.remove_replaced_files();
        Ok(())
    }

    async fn download(
        &self,
        file: &DownloadFile,
        finished: u64,
        stop: &CancellationToken,
    ) -> Result<(), DownloadError> {
        let mut last_report = Instant::now();
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
        .await
    }

    /// Deletes speech models an earlier catalogue used, such as a smaller transcription model.
    fn remove_replaced_files(&self) {
        let current: Vec<String> = self
            .plain_files()
            .iter()
            .flat_map(|file| {
                [
                    file.file_name.clone(),
                    format!("{}.verified", file.file_name),
                ]
            })
            .collect();
        let Ok(entries) = fs::read_dir(self.store.dir()) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_file = entry.file_type().is_ok_and(|kind| kind.is_file());
            if is_file && !current.contains(&name) {
                match fs::remove_file(entry.path()) {
                    Ok(()) => log::info!("removed replaced speech model {name}"),
                    Err(error) => log::warn!("could not remove {name}: {error}"),
                }
            }
        }
    }
}

/// Copies the listed files and folders out of a verified archive, below its top-level folder.
/// Any other entry, and any path that could leave `target`, is skipped.
fn extract(
    archive: &Path,
    target: &Path,
    files: &[String],
    dirs: &[String],
) -> Result<(), DownloadError> {
    if target.exists() {
        fs::remove_dir_all(target)?;
    }
    fs::create_dir_all(target)?;
    let reader = bzip2::read::BzDecoder::new(File::open(archive)?);
    let mut entries = tar::Archive::new(reader);
    let mut found_files = 0;
    let mut found_dirs = vec![false; dirs.len()];
    for entry in entries.entries()? {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path()?.into_owned();
        let mut parts = Vec::new();
        for component in path.components().skip(1) {
            let Component::Normal(part) = component else {
                parts.clear();
                break;
            };
            parts.push(part.to_string_lossy().into_owned());
        }
        let relative = parts.join("/");
        let in_dir = dirs
            .iter()
            .position(|dir| relative.starts_with(&format!("{dir}/")));
        let listed = files.contains(&relative);
        if relative.is_empty() || !(listed || in_dir.is_some()) {
            continue;
        }
        let destination = parts
            .iter()
            .fold(target.to_path_buf(), |path, part| path.join(part));
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let partial = destination.with_extension("part");
        io::copy(&mut entry, &mut File::create(&partial)?)?;
        fs::rename(&partial, &destination)?;
        if listed {
            found_files += 1;
        }
        if let Some(index) = in_dir {
            found_dirs[index] = true;
        }
    }
    if found_files != files.len() || found_dirs.contains(&false) {
        return Err(DownloadError::Verification);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive(dir: &Path, files: &[(&str, &[u8])]) -> PathBuf {
        let path = dir.join("model.tar.bz2");
        let encoder =
            bzip2::write::BzEncoder::new(File::create(&path).unwrap(), bzip2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (name, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            // Written directly so test paths are not normalised by the tar crate.
            header.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name.as_bytes());
            header.set_cksum();
            builder.append(&header, *content).unwrap();
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

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    fn listing(dir: &Path) -> Vec<String> {
        let mut found = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(current) = pending.pop() {
            for entry in fs::read_dir(current).unwrap().flatten() {
                if entry.file_type().unwrap().is_dir() {
                    pending.push(entry.path());
                } else {
                    let relative = entry.path().strip_prefix(dir).unwrap().to_owned();
                    found.push(relative.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        found.sort();
        found
    }

    #[test]
    fn extracts_only_listed_files_and_folders() {
        let dir = temp_dir("extract");
        let archive = archive(
            &dir,
            &[
                ("model-dir/model.onnx", b"model"),
                ("model-dir/tokens.txt", b"tokens"),
                ("model-dir/espeak-ng-data/en_dict", b"dict"),
                ("model-dir/espeak-ng-data/voices/en", b"voice"),
                ("model-dir/test_wavs/0.wav", b"audio"),
                ("model-dir/lexicon-zh.txt", b"unused"),
            ],
        );
        let target = dir.join("out");
        extract(
            &archive,
            &target,
            &names(&["model.onnx", "tokens.txt"]),
            &names(&["espeak-ng-data"]),
        )
        .unwrap();
        assert_eq!(
            listing(&target),
            [
                "espeak-ng-data/en_dict",
                "espeak-ng-data/voices/en",
                "model.onnx",
                "tokens.txt"
            ]
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn paths_that_leave_the_folder_are_never_written() {
        let dir = temp_dir("traversal");
        let archive = archive(
            &dir,
            &[
                ("model-dir/model.onnx", b"model"),
                ("model-dir/../escaped.txt", b"bad"),
                ("model-dir/espeak-ng-data/../../escaped.txt", b"bad"),
                ("model-dir/espeak-ng-data/en_dict", b"dict"),
            ],
        );
        let target = dir.join("out");
        extract(
            &archive,
            &target,
            &names(&["model.onnx"]),
            &names(&["espeak-ng-data"]),
        )
        .unwrap();
        assert!(!dir.join("escaped.txt").exists());
        assert_eq!(listing(&target), ["espeak-ng-data/en_dict", "model.onnx"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_archive_missing_a_listed_file_is_rejected() {
        let dir = temp_dir("incomplete");
        let archive = archive(&dir, &[("model-dir/tokens.txt", b"tokens")]);
        assert_eq!(
            extract(
                &archive,
                &dir.join("out"),
                &names(&["model.onnx", "tokens.txt"]),
                &[]
            ),
            Err(DownloadError::Verification)
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
