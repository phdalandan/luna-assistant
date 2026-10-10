//! Microphone capture with CPAL, mixed to mono and kept in memory only. The stream has its own
//! thread because CPAL streams cannot move between threads everywhere. Dropping it stops capture.
use std::sync::mpsc;
use std::thread::JoinHandle;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{DeviceId, ErrorKind, SampleFormat, StreamConfig};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    #[error("no microphone found")]
    NoDevice,
    #[error("the chosen microphone is not connected")]
    ChosenDeviceMissing,
    #[error("microphone access denied")]
    PermissionDenied,
    #[error("microphone disconnected")]
    Disconnected,
    #[error("microphone could not start")]
    Failed,
}

impl From<&cpal::Error> for CaptureError {
    fn from(error: &cpal::Error) -> Self {
        match error.kind() {
            ErrorKind::PermissionDenied => Self::PermissionDenied,
            ErrorKind::DeviceNotAvailable => Self::Disconnected,
            _ => Self::Failed,
        }
    }
}

/// A microphone the user can choose in Settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct MicrophoneOption {
    pub id: String,
    pub name: String,
}

/// The microphones connected now. Devices that cannot be described are left out and logged.
pub fn microphones() -> Result<Vec<MicrophoneOption>, cpal::Error> {
    Ok(cpal::default_host()
        .input_devices()?
        .filter_map(|device| match (device.id(), device.description()) {
            (Ok(id), Ok(description)) => Some(MicrophoneOption {
                id: id.to_string(),
                name: description.name().to_owned(),
            }),
            (Err(error), _) | (_, Err(error)) => {
                log::warn!("could not describe a microphone: {error}");
                None
            }
        })
        .collect())
}

/// Receives mono audio at `sample_rate`, or the error that stopped capture.
pub trait AudioSink: Send + Sync + 'static {
    fn audio(&self, samples: Vec<f32>);
    fn lost(&self, error: CaptureError);
}

pub struct Microphone {
    pub sample_rate: u32,
    stop: mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Microphone {
    /// Starts the chosen microphone, or the system default when `device` is `None`.
    pub fn start(device: Option<String>, sink: impl AudioSink) -> Result<Self, CaptureError> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (stop, stopped) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("luna-microphone".into())
            .spawn(move || match open(device.as_deref(), sink) {
                Ok((stream, sample_rate)) => {
                    let _ = ready_tx.send(Ok(sample_rate));
                    // Blocks until `Microphone` is dropped; the stream stops with it.
                    let _ = stopped.recv();
                    drop(stream);
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                }
            })
            .map_err(|error| {
                log::error!("could not start the microphone thread: {error}");
                CaptureError::Failed
            })?;
        let sample_rate = ready_rx.recv().unwrap_or(Err(CaptureError::Failed))?;
        Ok(Self {
            sample_rate,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Microphone {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn find(device: Option<&str>) -> Result<cpal::Device, CaptureError> {
    let host = cpal::default_host();
    let Some(id) = device else {
        return host.default_input_device().ok_or(CaptureError::NoDevice);
    };
    let id: DeviceId = id.parse().map_err(|error| {
        log::error!("invalid microphone id {id:?}: {error}");
        CaptureError::ChosenDeviceMissing
    })?;
    host.device_by_id(&id)
        .ok_or(CaptureError::ChosenDeviceMissing)
}

fn open(device: Option<&str>, sink: impl AudioSink) -> Result<(cpal::Stream, u32), CaptureError> {
    let device = find(device)?;
    let supported = device.default_input_config().map_err(|error| {
        log::error!("microphone configuration unavailable: {error}");
        CaptureError::from(&error)
    })?;
    let config: StreamConfig = supported.config();
    let channels = usize::from(config.channels.max(1));
    let sample_rate = config.sample_rate;
    let sink = std::sync::Arc::new(sink);
    let on_error = {
        let sink = sink.clone();
        move |error: cpal::Error| match error.kind() {
            // Capture continues after these, for example when audio follows a new default device.
            ErrorKind::DeviceChanged | ErrorKind::Xrun | ErrorKind::RealtimeDenied => {
                log::debug!("microphone: {error}");
            }
            _ => {
                log::warn!("microphone error: {error}");
                sink.lost(CaptureError::from(&error));
            }
        }
    };
    let stream = match supported.sample_format() {
        SampleFormat::F32 => device.build_input_stream(
            config,
            move |data: &[f32], _: &_| sink.audio(mono(data, channels, |sample| sample)),
            on_error,
            None,
        ),
        SampleFormat::I16 => device.build_input_stream(
            config,
            move |data: &[i16], _: &_| {
                sink.audio(mono(data, channels, |sample| f32::from(sample) / 32_768.0));
            },
            on_error,
            None,
        ),
        format => {
            log::error!("unsupported microphone sample format {format:?}");
            return Err(CaptureError::Failed);
        }
    }
    .map_err(|error| {
        log::error!("could not open the microphone: {error}");
        CaptureError::from(&error)
    })?;
    stream.play().map_err(|error| {
        log::error!("could not start the microphone: {error}");
        CaptureError::from(&error)
    })?;
    Ok((stream, sample_rate))
}

fn mono<T: Copy>(data: &[T], channels: usize, convert: impl Fn(T) -> f32) -> Vec<f32> {
    data.chunks(channels)
        .map(|frame| frame.iter().map(|sample| convert(*sample)).sum::<f32>() / frame.len() as f32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_are_mixed_to_mono() {
        assert_eq!(mono(&[1.0, 0.0, 0.5, 0.5], 2, |s| s), [0.5, 0.5]);
        assert_eq!(mono(&[16_384_i16], 1, |s| f32::from(s) / 32_768.0), [0.5]);
    }
}
