//! Speaker output with CPAL. The stream has its own thread, like the microphone, because CPAL
//! streams cannot move between threads everywhere. Dropping `Output` stops it.
use std::sync::Arc;
use std::sync::mpsc;
use std::thread::JoinHandle;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};

use super::VoiceError;

/// Fills a buffer of mono samples; called on the audio thread, so it must be quick.
pub type Source = Arc<dyn Fn(&mut [f32]) + Send + Sync>;

pub struct Output {
    pub sample_rate: u32,
    stop: mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Output {
    pub fn start(source: Source) -> Result<Self, VoiceError> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (stop, stopped) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("luna-speaker".into())
            .spawn(move || match open(source) {
                Ok((stream, sample_rate)) => {
                    let _ = ready_tx.send(Ok(sample_rate));
                    let _ = stopped.recv();
                    drop(stream);
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                }
            })
            .map_err(|error| {
                log::error!("could not start the speaker thread: {error}");
                VoiceError::Speech
            })?;
        let sample_rate = ready_rx.recv().unwrap_or(Err(VoiceError::Speech))?;
        Ok(Self {
            sample_rate,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn open(source: Source) -> Result<(cpal::Stream, u32), VoiceError> {
    let failed = |error: cpal::Error| {
        log::error!("could not open the speaker: {error}");
        VoiceError::Speech
    };
    let device = cpal::default_host()
        .default_output_device()
        .ok_or_else(|| {
            log::error!("no speaker found");
            VoiceError::Speech
        })?;
    let supported = device.default_output_config().map_err(failed)?;
    let config: StreamConfig = supported.config();
    let channels = usize::from(config.channels.max(1));
    let sample_rate = config.sample_rate;
    let on_error = |error: cpal::Error| log::warn!("speaker: {error}");
    let mut mono = Vec::new();
    let stream = match supported.sample_format() {
        SampleFormat::F32 => device.build_output_stream(
            config,
            move |data: &mut [f32], _: &_| {
                fill(&source, &mut mono, data, channels, |sample| sample);
            },
            on_error,
            None,
        ),
        SampleFormat::I16 => device.build_output_stream(
            config,
            move |data: &mut [i16], _: &_| {
                fill(&source, &mut mono, data, channels, |sample| {
                    (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16
                });
            },
            on_error,
            None,
        ),
        format => {
            log::error!("unsupported speaker sample format {format:?}");
            return Err(VoiceError::Speech);
        }
    }
    .map_err(failed)?;
    stream.play().map_err(failed)?;
    Ok((stream, sample_rate))
}

/// Asks the source for one mono sample per frame and copies it to every channel.
fn fill<T: Copy>(
    source: &Source,
    mono: &mut Vec<f32>,
    data: &mut [T],
    channels: usize,
    convert: impl Fn(f32) -> T,
) {
    mono.resize(data.len() / channels, 0.0);
    source(mono);
    for (frame, sample) in data.chunks_mut(channels).zip(mono.iter()) {
        frame.fill(convert(*sample));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_samples_are_copied_to_every_channel() {
        let source: Source = Arc::new(|mono: &mut [f32]| {
            for (index, sample) in mono.iter_mut().enumerate() {
                *sample = index as f32;
            }
        });
        let mut data = [0.0f32; 6];
        fill(&source, &mut Vec::new(), &mut data, 2, |sample| sample);
        assert_eq!(data, [0.0, 0.0, 1.0, 1.0, 2.0, 2.0]);
    }
}
