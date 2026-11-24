use std::future::Future;

use windows::Media::Core::MediaSource;
use windows::Media::Playback::MediaPlayer;
use windows::Media::SpeechSynthesis::{SpeechSynthesizer, VoiceInformation};
use windows::core::{HSTRING, Result};

/// Text-to-speech with the system's natural voices.
pub struct Speaker {
    engine: Option<(SpeechSynthesizer, MediaPlayer)>,
}

impl Speaker {
    /// Without speech support every call is a no-op.
    pub fn new() -> Self {
        let engine = SpeechSynthesizer::new().and_then(|s| Ok((s, MediaPlayer::new()?)));
        if let Err(error) = &engine {
            log::warn!("speech unavailable: {error}");
        }
        Self {
            engine: engine.ok(),
        }
    }

    /// Interrupts anything still playing, then speaks `text` with a voice for the
    /// BCP 47 `language` if one is installed. Dropping the future before it
    /// completes abandons the synthesis.
    pub fn speak(&self, text: &str, language: &str) -> impl Future<Output = Result<()>> + 'static {
        self.stop();
        let engine = self.engine.clone();
        let text = HSTRING::from(text);
        let language = language.to_owned();
        async move {
            let Some((synthesizer, player)) = engine else {
                return Ok(());
            };
            if let Some(voice) = voice_for(&language)? {
                synthesizer.SetVoice(&voice)?;
            }
            let stream = synthesizer.SynthesizeTextToStreamAsync(&text)?.await?;
            let source = MediaSource::CreateFromStream(&stream, &stream.ContentType()?)?;
            player.SetSource(&source)?;
            player.Play()
        }
    }

    pub fn stop(&self) {
        if let Some((_, player)) = &self.engine {
            let _ = player.Pause();
        }
    }
}

/// Exact region first (`en-US`), then any voice of the same language (`en-GB`).
fn voice_for(language: &str) -> Result<Option<VoiceInformation>> {
    let primary = language.split('-').next().unwrap_or(language);
    let mut fallback = None;
    for voice in SpeechSynthesizer::AllVoices()? {
        let tag = voice.Language()?.to_string();
        if tag.eq_ignore_ascii_case(language) {
            return Ok(Some(voice));
        }
        if fallback.is_none()
            && tag
                .split('-')
                .next()
                .is_some_and(|p| p.eq_ignore_ascii_case(primary))
        {
            fallback = Some(voice);
        }
    }
    Ok(fallback)
}
