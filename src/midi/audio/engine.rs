//! Audio engine for real-time MIDI synthesis.
//!
//! Provides a high-level interface for playing MIDI notes using rustysynth
//! for synthesis and rodio's audio backend (cpal, re-exported by rodio) for
//! output.
//!
//! Differences from miditui: the engine also works without a SoundFont or
//! without an audio device (editing and the sequencer clock keep working,
//! nothing sounds), the synthesizer runs at the device's native sample rate,
//! and the output stream is built with a silent error callback so nothing is
//! ever printed to the terminal while the TUI is active. The stream lives
//! exactly as long as the engine (dropping it stops all sound).

use crate::midi::model::{ticks_to_seconds, Track};
use anyhow::{anyhow, Context, Result};
use rodio::cpal;
use rodio::cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Sample rate for offline synthesis (WAV export) and the fallback rate.
pub const SAMPLE_RATE: u32 = 44100;

/// Represents the current playback state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    /// Not playing, position reset to start.
    Stopped,
    /// Currently playing.
    Playing,
    /// Paused at current position.
    Paused,
}

/// The main audio engine for MIDI synthesis and playback.
///
/// Manages the synthesizer, audio output, and playback state.
/// Supports real-time note playback and project sequencing.
pub struct AudioEngine {
    /// The synthesizer (shared with the audio thread); `None` without a SoundFont.
    synth: Option<Arc<Mutex<Synthesizer>>>,
    /// Audio output stream (must be kept alive); `None` without a device.
    stream: Option<cpal::Stream>,
    /// Why there is no output, for the status bar.
    output_problem: Option<String>,
    /// Whether the sequencer clock is running.
    playing: bool,
    /// Current playback position in ticks.
    position_ticks: u32,
    /// Current playback state.
    playback_state: PlaybackState,
    /// Current tempo for tick calculations.
    tempo: u32,
    /// Instrument names extracted from the loaded SoundFont.
    /// Indexed by program number (0-127). Falls back to "Program N" if not found.
    instrument_names: [String; 128],
}

impl AudioEngine {
    /// An engine with no SoundFont and no output: everything is silent but
    /// playback state and positions still work.
    pub fn silent(reason: &str) -> Self {
        Self {
            synth: None,
            stream: None,
            output_problem: Some(reason.to_string()),
            playing: false,
            position_ticks: 0,
            playback_state: PlaybackState::Stopped,
            tempo: 120,
            instrument_names: std::array::from_fn(|i| format!("Program {}", i)),
        }
    }

    /// Creates a new audio engine with the specified SoundFont.
    ///
    /// When `open_output` is set the default audio device is opened; if that
    /// fails the engine is still returned (silent, see [`Self::output_problem`]).
    ///
    /// # Errors
    ///
    /// Returns error if the SoundFont file cannot be read or is invalid.
    pub fn new<P: AsRef<Path>>(soundfont_path: P, open_output: bool) -> Result<Self> {
        let mut file = BufReader::new(File::open(soundfont_path.as_ref()).with_context(|| {
            format!("Failed to open SoundFont: {}", soundfont_path.as_ref().display())
        })?);
        let soundfont = Arc::new(SoundFont::new(&mut file).map_err(|e| anyhow!("Failed to load SoundFont: {:?}", e))?);
        let instrument_names = Self::extract_instrument_names(&soundfont);

        let output = if open_output { open_device() } else { Err("Audio output disabled".to_string()) };
        let (rate, device) = match output {
            Ok((device, config)) => (config.sample_rate().0, Some((device, config))),
            Err(problem) => {
                let synth = make_synth(&soundfont, SAMPLE_RATE)?;
                return Ok(Self {
                    synth: Some(Arc::new(Mutex::new(synth))),
                    stream: None,
                    output_problem: Some(problem),
                    instrument_names,
                    ..Self::silent("")
                });
            }
        };
        let synth = match make_synth(&soundfont, rate) {
            Ok(s) => s,
            Err(_) => make_synth(&soundfont, SAMPLE_RATE)?,
        };
        let synth = Arc::new(Mutex::new(synth));
        let (stream, output_problem) = match device {
            Some((device, config)) => match build_stream(&device, &config, Arc::clone(&synth)) {
                Ok(stream) => (Some(stream), None),
                Err(e) => (None, Some(e)),
            },
            None => (None, Some("No audio output".to_string())),
        };
        Ok(Self {
            synth: Some(synth),
            stream,
            output_problem,
            instrument_names,
            ..Self::silent("")
        })
    }

    /// `Some(reason)` when notes can't be heard (no SoundFont / no device).
    pub fn output_problem(&self) -> Option<&str> {
        if self.synth.is_none() {
            return Some(self.output_problem.as_deref().unwrap_or("No SoundFont"));
        }
        if self.stream.is_none() {
            return Some(self.output_problem.as_deref().unwrap_or("No audio output"));
        }
        None
    }

    /// Stops all sound and closes the output stream.
    pub fn shutdown(&mut self) {
        self.stop();
        self.stream = None;
        self.output_problem = Some("Audio closed".to_string());
    }

    /// Extracts instrument names from the SoundFont's presets.
    ///
    /// Maps program numbers (0-127) to preset names from bank 0 (General MIDI bank).
    /// If a program number has no preset in the SoundFont, falls back to "Program N".
    fn extract_instrument_names(soundfont: &SoundFont) -> [String; 128] {
        let mut names: [String; 128] = std::array::from_fn(|i| format!("Program {}", i));
        for preset in soundfont.get_presets() {
            let bank = preset.get_bank_number();
            let program = preset.get_patch_number();
            if bank == 0 && (0..128).contains(&program) {
                names[program as usize] = preset.get_name().to_string();
            }
        }
        names
    }

    /// Returns the instrument name for a given program number.
    pub fn get_instrument_name(&self, program: u8) -> &str {
        &self.instrument_names[(program as usize).min(127)]
    }

    fn with_synth(&self, f: impl FnOnce(&mut Synthesizer)) {
        if let Some(synth) = &self.synth {
            if let Ok(mut synth) = synth.lock() {
                f(&mut synth);
            }
        }
    }

    /// Plays a single note immediately.
    pub fn note_on(&self, channel: u8, note: u8, velocity: u8) {
        self.with_synth(|s| s.note_on(channel as i32, note as i32, velocity as i32));
    }

    /// Stops a playing note.
    pub fn note_off(&self, channel: u8, note: u8) {
        self.with_synth(|s| s.note_off(channel as i32, note as i32));
    }

    /// Stops all playing notes (`immediate`: without release).
    pub fn all_notes_off(&self, immediate: bool) {
        self.with_synth(|s| s.note_off_all(immediate));
    }

    /// Sets the instrument (program) for a channel.
    pub fn set_program(&self, channel: u8, program: u8) {
        // Program change is MIDI command 0xC0 (192)
        self.with_synth(|s| s.process_midi_message(channel as i32, 0xC0, program as i32, 0));
    }

    /// Sets the volume (CC 7) for a channel.
    pub fn set_channel_volume(&self, channel: u8, volume: u8) {
        self.with_synth(|s| s.process_midi_message(channel as i32, 0xB0, 7, volume as i32));
    }

    /// Sets the pan (CC 10) for a channel (0=left, 64=center, 127=right).
    pub fn set_channel_pan(&self, channel: u8, pan: u8) {
        self.with_synth(|s| s.process_midi_message(channel as i32, 0xB0, 10, pan as i32));
    }

    /// Alias for set_channel_volume.
    pub fn set_volume(&self, channel: u8, volume: u8) {
        self.set_channel_volume(channel, volume);
    }

    /// Alias for set_channel_pan.
    pub fn set_pan(&self, channel: u8, pan: u8) {
        self.set_channel_pan(channel, pan);
    }

    /// Configures the synth for a track's settings.
    pub fn configure_track(&self, track: &Track) {
        self.set_program(track.channel, track.program);
        self.set_channel_volume(track.channel, track.volume);
        self.set_channel_pan(track.channel, track.pan);
    }

    /// Returns the current playback state.
    pub fn playback_state(&self) -> PlaybackState {
        self.playback_state
    }

    /// Returns whether the sequencer is currently playing.
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Sets the playing state.
    pub fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
        self.playback_state = if playing { PlaybackState::Playing } else { PlaybackState::Paused };
    }

    /// Stops playback and resets position.
    pub fn stop(&mut self) {
        self.set_playing(false);
        self.all_notes_off(true);
        self.position_ticks = 0;
        self.playback_state = PlaybackState::Stopped;
    }

    /// Returns the current playback position in ticks.
    pub fn position_ticks(&self) -> u32 {
        self.position_ticks
    }

    /// Sets the playback position in ticks.
    pub fn set_position_ticks(&mut self, ticks: u32) {
        self.position_ticks = ticks;
    }

    /// Converts the current position to seconds.
    #[allow(dead_code)]
    pub fn position_seconds(&self) -> f64 {
        ticks_to_seconds(self.position_ticks(), self.tempo)
    }

    /// Sets the tempo for position calculations.
    pub fn set_tempo(&mut self, tempo: u32) {
        self.tempo = tempo;
    }
}

fn make_synth(soundfont: &Arc<SoundFont>, rate: u32) -> Result<Synthesizer> {
    let settings = SynthesizerSettings::new(rate as i32);
    Synthesizer::new(soundfont, &settings).map_err(|e| anyhow!("Failed to create synthesizer: {:?}", e))
}

/// The default output device and its default config.
fn open_device() -> std::result::Result<(cpal::Device, cpal::SupportedStreamConfig), String> {
    quiet::install();
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or_else(|| "No audio output".to_string())?;
    let config = device.default_output_config().map_err(|_| "No audio output".to_string())?;
    Ok((device, config))
}

fn build_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    synth: Arc<Mutex<Synthesizer>>,
) -> std::result::Result<cpal::Stream, String> {
    use cpal::SampleFormat as F;
    let cfg = config.config();
    let stream = match config.sample_format() {
        F::F32 => build_typed::<f32>(device, &cfg, synth),
        F::F64 => build_typed::<f64>(device, &cfg, synth),
        F::I8 => build_typed::<i8>(device, &cfg, synth),
        F::I16 => build_typed::<i16>(device, &cfg, synth),
        F::I32 => build_typed::<i32>(device, &cfg, synth),
        F::I64 => build_typed::<i64>(device, &cfg, synth),
        F::U8 => build_typed::<u8>(device, &cfg, synth),
        F::U16 => build_typed::<u16>(device, &cfg, synth),
        F::U32 => build_typed::<u32>(device, &cfg, synth),
        F::U64 => build_typed::<u64>(device, &cfg, synth),
        _ => return Err("Unsupported audio format".to_string()),
    }
    .map_err(|_| "No audio output".to_string())?;
    stream.play().map_err(|_| "No audio output".to_string())?;
    Ok(stream)
}

/// Builds an output stream that renders the synthesizer into the device's
/// interleaved buffer. Errors are swallowed (never printed).
fn build_typed<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    synth: Arc<Mutex<Synthesizer>>,
) -> std::result::Result<cpal::Stream, cpal::BuildStreamError>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = (config.channels as usize).max(1);
    let mut left: Vec<f32> = Vec::new();
    let mut right: Vec<f32> = Vec::new();
    device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            let frames = data.len() / channels;
            if frames == 0 {
                return;
            }
            if left.len() < frames {
                left.resize(frames, 0.0);
                right.resize(frames, 0.0);
            }
            match synth.lock() {
                Ok(mut s) => s.render(&mut left[..frames], &mut right[..frames]),
                Err(_) => {
                    left[..frames].fill(0.0);
                    right[..frames].fill(0.0);
                }
            }
            for (i, frame) in data.chunks_mut(channels).enumerate() {
                let (l, r) = if i < frames { (left[i], right[i]) } else { (0.0, 0.0) };
                for (c, sample) in frame.iter_mut().enumerate() {
                    let v = match (channels, c) {
                        (1, _) => (l + r) * 0.5,
                        (_, 0) => l,
                        (_, 1) => r,
                        _ => 0.0,
                    };
                    *sample = T::from_sample(v);
                }
            }
        },
        |_err| {},
        None,
    )
}

/// Keeps libasound from printing "ALSA lib ..." diagnostics to stderr
/// (they would scribble over the TUI) by installing a no-op error handler.
#[cfg(target_os = "linux")]
pub(in crate::midi) mod quiet {
    use std::os::raw::{c_char, c_int};
    use std::sync::Once;

    /// ALSA's handler type is variadic; the trailing varargs are simply
    /// ignored by this non-variadic callee, which the C ABI permits.
    type Handler = unsafe extern "C" fn(*const c_char, c_int, *const c_char, c_int, *const c_char);

    extern "C" {
        fn snd_lib_error_set_handler(handler: Option<Handler>) -> c_int;
    }

    unsafe extern "C" fn ignore(_: *const c_char, _: c_int, _: *const c_char, _: c_int, _: *const c_char) {}

    pub fn install() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            // SAFETY: registers a callback that does nothing; libasound is
            // linked through rodio -> cpal -> alsa-sys.
            unsafe {
                snd_lib_error_set_handler(Some(ignore));
            }
        });
    }
}

#[cfg(not(target_os = "linux"))]
pub(in crate::midi) mod quiet {
    pub fn install() {}
}
