//! Microphone capture with CPAL. Audio is mixed to mono and handed over in memory only.
//! The stream lives on its own thread because CPAL streams cannot move between threads on
//! every platform. Dropping `Microphone` stops capture immediately.
use std::sync::mpsc;
use std::thread::JoinHandle;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{ErrorKind, SampleFormat, StreamConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    #[error("no microphone found")]
    NoDevice,
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
    pub fn start(sink: impl AudioSink) -> Result<Self, CaptureError> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (stop, stopped) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("luna-microphone".into())
            .spawn(move || match open(sink) {
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

fn open(sink: impl AudioSink) -> Result<(cpal::Stream, u32), CaptureError> {
    let device = cpal::default_host()
        .default_input_device()
        .ok_or(CaptureError::NoDevice)?;
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
