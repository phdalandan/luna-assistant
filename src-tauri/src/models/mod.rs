//! Model catalogue, downloads, and local storage. Weights are never bundled with Luna.
pub mod catalog;
mod download;
mod memory;
pub mod speech;
mod store;
mod voice;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio_util::sync::CancellationToken;

pub use catalog::{CatalogModel, ChatOptions, CloudModel, CloudProvider};
use download::DownloadError;
pub use store::ModelStore;
pub use voice::{VoiceModels, VoiceModelsInfo};

const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
/// Downloads fail instead of hanging when the connection stalls this long.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelError {
    #[error("unknown model {0}")]
    Unknown(String),
    #[error("model {0} is not installed")]
    NotInstalled(String),
    #[error("model {0} is already downloading")]
    AlreadyDownloading(String),
    #[error("model {0} is in use")]
    InUse(String),
    #[error("model file error: {0}")]
    Io(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub enum DownloadPhase {
    Downloading,
    Paused,
    Verifying,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct DownloadStatus {
    pub phase: DownloadPhase,
    #[cfg_attr(test, ts(type = "number"))]
    pub downloaded: u64,
    /// User-facing reason when the download failed.
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub quantization: String,
    #[cfg_attr(test, ts(type = "number"))]
    pub size: u64,
    pub recommended: bool,
    pub warning: Option<String>,
    /// The model plus its context may not fit in the memory available right now.
    pub memory_warning: bool,
    pub installed: bool,
    pub active: bool,
    pub download: Option<DownloadStatus>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub id: String,
    pub downloaded: u64,
}

pub trait ModelEvents: Send + Sync + 'static {
    fn changed(&self);
    fn progress(&self, progress: DownloadProgress);
}

enum Activity {
    Running {
        stop: CancellationToken,
        pause_requested: bool,
        downloaded: u64,
    },
    Failed(String),
}

pub struct ModelManager {
    models: Vec<CatalogModel>,
    store: ModelStore,
    http: reqwest::Client,
    activity: Mutex<HashMap<String, Activity>>,
    events: Arc<dyn ModelEvents>,
    voice: Arc<VoiceModels>,
}

impl ModelManager {
    pub fn new(
        models: Vec<CatalogModel>,
        store: ModelStore,
        events: Arc<dyn ModelEvents>,
    ) -> Result<Self, ModelError> {
        crate::tls::install_crypto_provider();
        let http = reqwest::Client::builder()
            .read_timeout(READ_TIMEOUT)
            .build()
            .map_err(|error| ModelError::Io(error.to_string()))?;
        let speech_store = ModelStore::open(store.dir().join("speech")).map_err(io_error)?;
        let voice = Arc::new(VoiceModels::new(
            catalog::speech(),
            speech_store,
            http.clone(),
            events.clone(),
        ));
        Ok(Self {
            models,
            store,
            http,
            activity: Mutex::default(),
            events,
            voice,
        })
    }

    pub fn voice(&self) -> &Arc<VoiceModels> {
        &self.voice
    }

    pub fn store(&self) -> &ModelStore {
        &self.store
    }

    pub fn notify_changed(&self) {
        self.events.changed();
    }

    pub fn list(&self, active: Option<&str>, context_length: u32) -> Vec<ModelInfo> {
        let activity = lock(&self.activity);
        let available_memory = memory::available();
        self.models
            .iter()
            .map(|model| {
                let installed = self.store.is_installed(model);
                ModelInfo {
                    id: model.id.clone(),
                    name: model.name.clone(),
                    quantization: model.quantization.clone(),
                    size: model.size,
                    recommended: model.recommended,
                    warning: model.warning.clone(),
                    memory_warning: memory::required(model, context_length) > available_memory,
                    installed,
                    active: installed && active == Some(model.id.as_str()),
                    download: self.download_status(model, activity.get(&model.id)),
                }
            })
            .collect()
    }

    fn download_status(
        &self,
        model: &CatalogModel,
        activity: Option<&Activity>,
    ) -> Option<DownloadStatus> {
        let partial = self.store.partial_len(model);
        let (phase, downloaded, error) = match activity {
            Some(Activity::Running { downloaded, .. }) if *downloaded >= model.size => {
                (DownloadPhase::Verifying, *downloaded, None)
            }
            Some(Activity::Running { downloaded, .. }) => {
                (DownloadPhase::Downloading, *downloaded, None)
            }
            Some(Activity::Failed(message)) => {
                (DownloadPhase::Failed, partial, Some(message.clone()))
            }
            None if partial > 0 => (DownloadPhase::Paused, partial, None),
            None => return None,
        };
        Some(DownloadStatus {
            phase,
            downloaded,
            error,
        })
    }

    /// Starts or resumes a download the user asked for. Never chooses a model itself.
    pub fn start_download(self: &Arc<Self>, id: &str) -> Result<(), ModelError> {
        let model = self.find(id)?.clone();
        if self.store.is_installed(&model) {
            return Ok(());
        }
        let stop = CancellationToken::new();
        {
            let mut activity = lock(&self.activity);
            if matches!(activity.get(id), Some(Activity::Running { .. })) {
                return Err(ModelError::AlreadyDownloading(id.to_owned()));
            }
            activity.insert(
                id.to_owned(),
                Activity::Running {
                    stop: stop.clone(),
                    pause_requested: false,
                    downloaded: self.store.partial_len(&model),
                },
            );
        }
        self.events.changed();

        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            let result = manager.run_download(&model, &stop).await;
            manager.finish_download(&model, result);
        });
        Ok(())
    }

    async fn run_download(
        &self,
        model: &CatalogModel,
        stop: &CancellationToken,
    ) -> Result<(), DownloadError> {
        let mut last_report = Instant::now();
        download::download(&self.http, model, &self.store, stop, |downloaded| {
            if let Some(Activity::Running {
                downloaded: current,
                ..
            }) = lock(&self.activity).get_mut(&model.id)
            {
                *current = downloaded;
            }
            if downloaded >= model.size {
                self.events.changed();
            } else if last_report.elapsed() >= PROGRESS_INTERVAL {
                last_report = Instant::now();
                self.events.progress(DownloadProgress {
                    id: model.id.clone(),
                    downloaded,
                });
            }
        })
        .await
    }

    fn finish_download(&self, model: &CatalogModel, result: Result<(), DownloadError>) {
        let mut activity = lock(&self.activity);
        let paused = matches!(
            activity.get(&model.id),
            Some(Activity::Running {
                pause_requested: true,
                ..
            })
        );
        activity.remove(&model.id);
        match result {
            Ok(()) => log::info!("installed model {}", model.id),
            Err(DownloadError::Stopped) if paused => log::info!("paused download of {}", model.id),
            Err(DownloadError::Stopped) => {
                if let Err(error) = self.store.discard_partial(model) {
                    log::error!(
                        "failed to remove cancelled download of {}: {error}",
                        model.id
                    );
                }
            }
            Err(error) => {
                log::error!("download of {} failed: {error}", model.id);
                activity.insert(model.id.clone(), Activity::Failed(download_message(&error)));
            }
        }
        drop(activity);
        self.events.changed();
    }

    /// Stops the download but keeps the partial file so it can resume later.
    pub fn pause_download(&self, id: &str) {
        if let Some(Activity::Running {
            stop,
            pause_requested,
            ..
        }) = lock(&self.activity).get_mut(id)
        {
            *pause_requested = true;
            stop.cancel();
        }
    }

    /// Stops the download and removes any partial file.
    pub fn cancel_download(&self, id: &str) -> Result<(), ModelError> {
        let model = self.find(id)?;
        let mut activity = lock(&self.activity);
        match activity.get(id) {
            Some(Activity::Running { stop, .. }) => stop.cancel(),
            _ => {
                activity.remove(id);
                self.store.discard_partial(model).map_err(io_error)?;
                drop(activity);
                self.events.changed();
            }
        }
        Ok(())
    }

    pub fn delete(&self, id: &str, in_use: bool) -> Result<(), ModelError> {
        let model = self.find(id)?;
        if in_use {
            return Err(ModelError::InUse(id.to_owned()));
        }
        if matches!(lock(&self.activity).get(id), Some(Activity::Running { .. })) {
            return Err(ModelError::AlreadyDownloading(id.to_owned()));
        }
        self.store.delete(model).map_err(io_error)?;
        log::info!("deleted model {id}");
        self.events.changed();
        Ok(())
    }

    /// The installed catalogue entry for `id`, or an error explaining why it can't be used.
    pub fn installed(&self, id: &str) -> Result<&CatalogModel, ModelError> {
        let model = self.find(id)?;
        if !self.store.is_installed(model) {
            return Err(ModelError::NotInstalled(id.to_owned()));
        }
        Ok(model)
    }

    fn find(&self, id: &str) -> Result<&CatalogModel, ModelError> {
        self.models
            .iter()
            .find(|model| model.id == id)
            .ok_or_else(|| ModelError::Unknown(id.to_owned()))
    }
}

fn download_message(error: &DownloadError) -> String {
    match error {
        DownloadError::InsufficientSpace { .. } => "Not enough disk space.",
        DownloadError::Verification => "Model verification failed.",
        DownloadError::Io(_) => "Couldn't save the model. Check that your disk is available.",
        DownloadError::Interrupted(_) | DownloadError::Http(_) | DownloadError::Stopped => {
            "Download interrupted. Try again."
        }
    }
    .to_owned()
}

fn io_error(error: std::io::Error) -> ModelError {
    ModelError::Io(error.to_string())
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::Notify;

    use super::download::test_server::{Behaviour, serve};
    use super::store::tests::{TempStore, small_model};
    use super::*;

    #[derive(Default)]
    struct RecordingEvents {
        changes: AtomicUsize,
        changed: Notify,
    }

    impl ModelEvents for RecordingEvents {
        fn changed(&self) {
            self.changes.fetch_add(1, Ordering::SeqCst);
            self.changed.notify_waiters();
        }
        fn progress(&self, _: DownloadProgress) {}
    }

    fn body() -> Vec<u8> {
        (0..100_000u32).map(|value| (value % 241) as u8).collect()
    }

    /// A manager whose only model is served by a local test server.
    async fn manager_with_server(
        temp: &TempStore,
        behaviour: Behaviour,
    ) -> (Arc<ModelManager>, Arc<RecordingEvents>) {
        let (url, _) = serve(body(), behaviour).await;
        let model = CatalogModel {
            url,
            ..small_model(&body())
        };
        let events = Arc::new(RecordingEvents::default());
        let manager = ModelManager::new(vec![model], temp.store.clone(), events.clone()).unwrap();
        (Arc::new(manager), events)
    }

    async fn wait_for(
        manager: &ModelManager,
        events: &RecordingEvents,
        done: impl Fn(&ModelInfo) -> bool,
    ) -> ModelInfo {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let notified = events.changed.notified();
                let info = manager.list(None, 4096).remove(0);
                if done(&info) {
                    return info;
                }
                // Progress ticks are not change events, so also re-check periodically.
                tokio::select! {
                    () = notified => {}
                    () = tokio::time::sleep(Duration::from_millis(50)) => {}
                }
            }
        })
        .await
        .expect("model state not reached")
    }

    fn phase(info: &ModelInfo) -> Option<DownloadPhase> {
        info.download
            .as_ref()
            .map(|download| download.phase.clone())
    }

    fn stalling() -> Behaviour {
        Behaviour {
            cut_first_response_after: Some(1000),
            stall_first_response: true,
            ..Behaviour::default()
        }
    }

    #[test]
    fn lists_the_bundled_catalogue_as_available() {
        let temp = TempStore::new("manager-list");
        let events = Arc::new(RecordingEvents::default());
        let manager =
            ModelManager::new(catalog::models().to_vec(), temp.store.clone(), events).unwrap();
        let models = manager.list(Some("qwen3-8b"), 4096);
        assert_eq!(models.len(), 3);
        assert!(models.iter().all(|model| !model.installed && !model.active));
        assert!(models[0].recommended);
        assert!(models[2].warning.is_some());
    }

    #[tokio::test]
    async fn completed_downloads_are_installed_and_selectable() {
        let temp = TempStore::new("manager-install");
        let (manager, events) = manager_with_server(&temp, Behaviour::default()).await;
        manager.start_download("qwen3-8b").unwrap();
        let info = wait_for(&manager, &events, |info| info.installed).await;
        assert_eq!(info.download, None);
        assert!(manager.installed("qwen3-8b").is_ok());
        assert!(manager.list(Some("qwen3-8b"), 4096)[0].active);
    }

    #[tokio::test]
    async fn duplicate_downloads_are_rejected() {
        let temp = TempStore::new("manager-duplicate");
        let (manager, events) = manager_with_server(&temp, stalling()).await;
        manager.start_download("qwen3-8b").unwrap();
        assert_eq!(
            manager.start_download("qwen3-8b"),
            Err(ModelError::AlreadyDownloading("qwen3-8b".into()))
        );
        manager.cancel_download("qwen3-8b").unwrap();
        wait_for(&manager, &events, |info| info.download.is_none()).await;
        assert_eq!(temp.store.partial_len(&manager.models[0]), 0);
    }

    #[tokio::test]
    async fn paused_downloads_keep_progress_and_resume() {
        let temp = TempStore::new("manager-pause");
        let (manager, events) = manager_with_server(&temp, stalling()).await;
        manager.start_download("qwen3-8b").unwrap();
        wait_for(&manager, &events, |_| {
            temp.store.partial_len(&manager.models[0]) == 1000
        })
        .await;

        manager.pause_download("qwen3-8b");
        let paused = wait_for(&manager, &events, |info| {
            phase(info) == Some(DownloadPhase::Paused)
        })
        .await;
        assert_eq!(paused.download.unwrap().downloaded, 1000);

        manager.start_download("qwen3-8b").unwrap();
        wait_for(&manager, &events, |info| info.installed).await;
    }

    #[tokio::test]
    async fn failed_downloads_show_a_message_and_can_be_retried() {
        let temp = TempStore::new("manager-failed");
        let behaviour = Behaviour {
            status: Some(500),
            ..Behaviour::default()
        };
        let (manager, events) = manager_with_server(&temp, behaviour).await;
        manager.start_download("qwen3-8b").unwrap();
        let failed = wait_for(&manager, &events, |info| {
            phase(info) == Some(DownloadPhase::Failed)
        })
        .await;
        assert_eq!(
            failed.download.unwrap().error.as_deref(),
            Some("Download interrupted. Try again.")
        );
        assert!(!failed.installed);
        manager.start_download("qwen3-8b").unwrap();
    }

    #[test]
    fn cancelling_a_paused_download_removes_the_partial_file() {
        let temp = TempStore::new("manager-cancel");
        let events = Arc::new(RecordingEvents::default());
        let manager = ModelManager::new(
            catalog::models().to_vec(),
            temp.store.clone(),
            events.clone(),
        )
        .unwrap();
        let qwen = catalog::find("qwen3-8b").unwrap();
        std::fs::write(temp.store.partial_path(qwen), vec![0; 10]).unwrap();
        assert_eq!(
            manager.list(None, 4096)[0].download.as_ref().unwrap().phase,
            DownloadPhase::Paused
        );

        manager.cancel_download("qwen3-8b").unwrap();
        assert_eq!(temp.store.partial_len(qwen), 0);
        assert_eq!(manager.list(None, 4096)[0].download, None);
        assert_eq!(events.changes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn models_in_use_cannot_be_deleted() {
        let temp = TempStore::new("manager-delete");
        let (manager, events) = manager_with_server(&temp, Behaviour::default()).await;
        manager.start_download("qwen3-8b").unwrap();
        wait_for(&manager, &events, |info| info.installed).await;

        assert_eq!(
            manager.delete("qwen3-8b", true),
            Err(ModelError::InUse("qwen3-8b".into()))
        );
        assert!(manager.installed("qwen3-8b").is_ok());
        manager.delete("qwen3-8b", false).unwrap();
        assert_eq!(
            manager.installed("qwen3-8b").err(),
            Some(ModelError::NotInstalled("qwen3-8b".into()))
        );
        assert_eq!(
            manager.delete("other", false),
            Err(ModelError::Unknown("other".into()))
        );
    }
}
