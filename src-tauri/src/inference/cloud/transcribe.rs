//! OpenAI speech-to-text, for users who choose cloud speech recognition. Only a request spoken
//! after the wake word is sent, and the transcript is never logged.
use std::io::Write;
use std::time::Instant;

use reqwest::header::CONTENT_TYPE;

use super::{CloudClient, CloudFailure, failed};
use crate::credentials::AccessToken;
use crate::inference::InferenceError;
use crate::models::{CloudProvider, CloudTranscription};

/// Never appears in the form fields, and 16-bit audio matching it is vanishingly unlikely.
const BOUNDARY: &str = "luna-request-audio-5d1f9b37c2e84a60";

impl CloudClient {
    /// Transcribes mono audio. `prompt` lists names from the home so they are spelled as in
    /// Home Assistant. Dropping the future aborts the request.
    pub async fn transcribe(
        &self,
        model: &CloudTranscription,
        key: &AccessToken,
        samples: &[f32],
        sample_rate: u32,
        prompt: &str,
    ) -> Result<String, InferenceError> {
        let started = Instant::now();
        let request = self
            .http
            .post(&self.endpoints.openai_transcription)
            .bearer_auth(key.expose())
            .header(
                CONTENT_TYPE,
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .body(form(model, prompt, &wav(samples, sample_rate)));
        let reply = self.send(CloudProvider::OpenAi, request).await?;
        let text = reply["text"].as_str().ok_or_else(|| {
            failed(
                CloudProvider::OpenAi,
                CloudFailure::InvalidResponse,
                "missing text".into(),
            )
        })?;
        log::info!(
            "cloud transcription: {} transcribed {:.1} s of audio in {} ms",
            model.id,
            samples.len() as f32 / sample_rate as f32,
            started.elapsed().as_millis()
        );
        Ok(text.trim().to_owned())
    }
}

fn form(model: &CloudTranscription, prompt: &str, wav: &[u8]) -> Vec<u8> {
    let mut fields = vec![("model", model.id.as_str()), ("response_format", "json")];
    if !prompt.is_empty() {
        fields.push(("prompt", prompt));
    }
    fields.extend(
        model
            .form
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    );
    let mut body = Vec::with_capacity(wav.len() + 1024);
    for (name, value) in fields {
        let _ = write!(
            body,
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
        );
    }
    let _ = write!(
        body,
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"request.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
    );
    body.extend_from_slice(wav);
    let _ = write!(body, "\r\n--{BOUNDARY}--\r\n");
    body
}

/// 16-bit PCM WAV, the smallest lossless format the API accepts.
fn wav(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::inference::{DEFAULT_TIMEOUT, Endpoints};
    use crate::models::catalog;

    fn client(server: &MockServer) -> CloudClient {
        let endpoints = Endpoints {
            openai_transcription: format!("{}/v1/audio/transcriptions", server.uri()),
            ..Endpoints::default()
        };
        CloudClient::new(endpoints, DEFAULT_TIMEOUT).unwrap()
    }

    fn key() -> AccessToken {
        AccessToken::new("test-key".into()).unwrap()
    }

    #[test]
    fn audio_is_sent_as_16_bit_wav() {
        let bytes = wav(&[0.0, 1.0, -1.0], 16_000);
        assert_eq!(bytes.len(), 44 + 6);
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(
            u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            16_000
        );
        assert_eq!(&bytes[44..], [0, 0, 0xff, 0x7f, 0x01, 0x80]);
    }

    #[tokio::test]
    async fn requests_are_transcribed_with_the_home_s_names() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "text": " What about the AC? Is it on? ",
                "usage": {"type": "duration", "seconds": 2}
            })))
            .mount(&server)
            .await;

        let text = client(&server)
            .transcribe(
                catalog::cloud_transcription(),
                &key(),
                &vec![0.0; 16_000],
                16_000,
                "Front Porch, AC Ziran",
            )
            .await
            .unwrap();

        assert_eq!(text, "What about the AC? Is it on?");
        let request = &server.received_requests().await.unwrap()[0];
        let content_type = request.headers.get("content-type").unwrap();
        assert!(content_type.to_str().unwrap().contains(BOUNDARY));
        let body = String::from_utf8_lossy(&request.body);
        for field in [
            "name=\"model\"\r\n\r\ngpt-transcribe\r\n",
            "name=\"prompt\"\r\n\r\nFront Porch, AC Ziran\r\n",
            "name=\"languages[]\"\r\n\r\nen\r\n",
            "filename=\"request.wav\"\r\nContent-Type: audio/wav",
        ] {
            assert!(body.contains(field), "{field}");
        }
        assert!(body.trim_end().ends_with(&format!("--{BOUNDARY}--")));
    }

    #[tokio::test]
    async fn rejected_keys_and_slow_replies_are_reported() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": {"type": "invalid_request_error", "code": "invalid_api_key",
                    "message": "Incorrect API key provided: test-key"}
            })))
            .mount(&server)
            .await;
        let error = client(&server)
            .transcribe(catalog::cloud_transcription(), &key(), &[0.0], 16_000, "")
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            InferenceError::Cloud {
                failure: CloudFailure::Unauthorized,
                ..
            }
        ));
        assert!(!error.to_string().contains("test-key"));

        let slow = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .mount(&slow)
            .await;
        let endpoints = Endpoints {
            openai_transcription: format!("{}/v1/audio/transcriptions", slow.uri()),
            ..Endpoints::default()
        };
        let error = CloudClient::new(endpoints, Duration::from_millis(100))
            .unwrap()
            .transcribe(catalog::cloud_transcription(), &key(), &[0.0], 16_000, "")
            .await
            .unwrap_err();
        assert_eq!(error, InferenceError::Timeout);
    }
}
