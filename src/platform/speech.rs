use windows::Win32::Media::Speech::{
    ISpObjectToken, ISpObjectTokenCategory, ISpVoice, SPCAT_VOICES, SPF_ASYNC, SPF_IS_NOT_XML,
    SPF_PURGEBEFORESPEAK, SpObjectTokenCategory, SpVoice,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::core::{HSTRING, PCWSTR, w};

/// SAPI text-to-speech. Lives on the UI thread, which is already a COM apartment.
pub struct Speaker(Option<ISpVoice>);

impl Speaker {
    /// Without a speech engine every call is a no-op.
    pub fn new() -> Self {
        let voice: Option<ISpVoice> = unsafe { CoCreateInstance(&SpVoice, None, CLSCTX_ALL) }.ok();
        if let (Some(voice), Some(english)) = (&voice, english_voice()) {
            let _ = unsafe { voice.SetVoice(&english) };
        }
        Self(voice)
    }

    /// Interrupts anything still being spoken.
    pub fn speak(&self, text: &str) {
        let text = HSTRING::from(text);
        self.say(PCWSTR(text.as_ptr()));
    }

    pub fn stop(&self) {
        self.say(PCWSTR::null());
    }

    fn say(&self, text: PCWSTR) {
        if let Some(voice) = &self.0 {
            let flags = (SPF_ASYNC.0 | SPF_PURGEBEFORESPEAK.0 | SPF_IS_NOT_XML.0) as u32;
            let _ = unsafe { voice.Speak(text, flags, None) };
        }
    }
}

/// Spoken text is always English; on a Chinese system the default voice reads it poorly.
fn english_voice() -> Option<ISpObjectToken> {
    unsafe {
        let category: ISpObjectTokenCategory =
            CoCreateInstance(&SpObjectTokenCategory, None, CLSCTX_ALL).ok()?;
        category.SetId(SPCAT_VOICES, false).ok()?;
        let tokens = category
            .EnumTokens(w!("Language=409"), PCWSTR::null())
            .ok()?;
        let mut token = None;
        tokens.Next(1, &mut token, None).ok()?;
        token
    }
}
