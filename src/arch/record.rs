//! Walkthrough recordings: each beat's picture held on screen while a local text-to-speech
//! program reads its narration, then a short pad, all beats encoded into one MP4 by `ffmpeg`. A
//! recording leaves a talk folder with each beat's PNG, its audio and a manifest (`talk.json`),
//! from which the MP4 can be encoded again without rendering or speaking.
//!
//! Boundrail picks every program, never the repository: the first of `SPEAKERS`, and `ffmpeg`,
//! found by bare name in the absolute folders of `PATH` outside the checkout, as the rasterizers
//! are. Each runs with arguments Boundrail builds, never through a shell, in a private temporary
//! folder holding its inputs, with `PATH` holding only those folders, under the tool timeout.
//! Narration reaches a speaker in a file or on stdin, never as an argument. `say` (macOS) writes
//! AIFF, `espeak-ng` and `piper` WAV; `piper` takes its voice model file as `--voice`, made
//! absolute before it runs. A talk names only plain file names (letters, digits, `.`, `_` and
//! `-`, starting with a letter or digit), a `.png` image and a `.wav` or `.aiff` audio, so ffmpeg
//! reads no folder, protocol, option or image-sequence pattern from it. ffmpeg reads each input
//! with its format forced (`image2` on one file, `wav` or `aiff`) and only the `file` protocol, so
//! a file that is really a playlist or other container reaches nothing outside the folder.

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use super::picture::{SIGNATURE, Unrendered};
use crate::collect::{self, TempDir};

const SPEAKERS: [(&str, Kind); 3] = [
    ("say", Kind::Say),
    ("espeak-ng", Kind::Espeak),
    ("piper", Kind::Piper),
];
/// The encoder.
pub const ENCODER: &str = "ffmpeg";
/// The manifest of a talk folder.
pub const MANIFEST: &str = "talk.json";
/// The schema version of a talk folder's manifest.
pub const TALK_SCHEMA: u32 = 1;
/// Seconds a beat's picture stays after its narration ends.
pub const PAD: f64 = 0.5;
const PIPER_RATE: f64 = 175.0;
const FRAME_MAX: u32 = 7680;
const FRAME_MIN: u32 = 16;
const FPS_MAX: u32 = 60;
const BEAT_SECONDS_MAX: f64 = 3600.0;

#[derive(Debug, Clone, Copy)]
enum Kind {
    Say,
    Espeak,
    Piper,
}

/// A talk folder's manifest: what was recorded, the frame, and each beat's files and lengths.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Talk {
    pub schema_version: u32,
    /// The walkthrough's qualified ID.
    pub walkthrough: String,
    pub name: String,
    /// The revision of the current state recorded.
    pub revision: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// The text-to-speech program that spoke.
    pub speaker: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// Words per minute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<u32>,
    pub beats: Vec<TalkBeat>,
}

impl Talk {
    /// Each beat's image and audio file names, in order.
    pub fn files(&self) -> impl Iterator<Item = &String> {
        self.beats.iter().flat_map(|b| [&b.image, &b.audio])
    }
}

/// One beat of a talk: its files in the talk folder and how long it lasts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TalkBeat {
    /// The beat's qualified ID.
    pub id: String,
    pub title: String,
    pub narration: String,
    pub image: String,
    pub audio: String,
    /// Seconds the narration lasts.
    pub narration_seconds: f64,
    /// Seconds the beat lasts: the narration and the pad.
    pub seconds: f64,
}

/// Why a frame of `width` by `height` pixels at `fps` frames a second cannot be encoded.
pub fn frame_problem(width: u32, height: u32, fps: u32) -> Option<String> {
    let side = FRAME_MIN..=FRAME_MAX;
    if !(side.contains(&width)
        && side.contains(&height)
        && width.is_multiple_of(2)
        && height.is_multiple_of(2))
    {
        return Some(format!(
            "--width and --height must be even numbers from {FRAME_MIN} to {FRAME_MAX}"
        ));
    }
    (!(1..=FPS_MAX).contains(&fps)).then(|| format!("--fps must be from 1 to {FPS_MAX}"))
}

/// Why `talk` cannot be encoded: its version, frame, beat lengths or file names.
pub fn talk_problem(talk: &Talk) -> Option<String> {
    if talk.schema_version != TALK_SCHEMA {
        return Some(format!(
            "{MANIFEST} has schema version {}, not {TALK_SCHEMA}",
            talk.schema_version
        ));
    }
    if talk.beats.is_empty() {
        return Some(format!("{MANIFEST} names no beats"));
    }
    if let Some(problem) = frame_problem(talk.width, talk.height, talk.fps) {
        return Some(problem);
    }
    talk.beats.iter().find_map(|beat| {
        if !(beat.seconds > 0.0 && beat.seconds <= BEAT_SECONDS_MAX) {
            return Some(format!("beat {} lasts {} seconds", beat.id, beat.seconds));
        }
        let named = |name: &str, what: &str, extensions: &[&str]| {
            let fits = plain_name(name) && extensions.iter().any(|e| name.ends_with(e));
            (!fits).then(|| {
                format!(
                    "beat {} names the {what} `{name}`, not a plain file name ending in {}",
                    beat.id,
                    extensions.join(" or ")
                )
            })
        };
        named(&beat.image, "image", &[".png"])
            .or_else(|| named(&beat.audio, "audio", &[".wav", ".aiff"]))
    })
}

/// Why `bytes`, the file `name` of a talk, is not the PNG or the WAV or AIFF its extension says.
pub fn file_problem(name: &str, bytes: &[u8]) -> Option<String> {
    if name.ends_with(".png") {
        return (!bytes.starts_with(SIGNATURE)).then(|| "it is not a PNG".to_owned());
    }
    seconds(bytes)
        .err()
        .map(|why| format!("it is not WAV or AIFF: {why}"))
}

fn plain_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// A text-to-speech program found on `PATH` outside the checkout.
pub struct Speaker {
    pub name: &'static str,
    kind: Kind,
    program: PathBuf,
}

/// The first of `SPEAKERS` on `PATH` outside `checkout`; `no-text-to-speech` when none is.
pub fn speaker(checkout: &Path) -> Result<Speaker, Unrendered> {
    let found = (SPEAKERS.iter()).find_map(|(name, kind)| {
        let program = collect::resolve_program(name, checkout).ok()?;
        Some((*name, *kind, program))
    });
    let (name, kind, program) = found.ok_or_else(|| {
        let names: Vec<&str> = SPEAKERS.iter().map(|(name, _)| *name).collect();
        Unrendered {
            reason: "no-text-to-speech",
            message: format!(
                "no text-to-speech program on PATH; tried {}",
                names.join(", ")
            ),
        }
    })?;
    Ok(Speaker {
        name,
        kind,
        program,
    })
}

impl Speaker {
    /// The extension of the audio files this speaker writes.
    pub fn audio(&self) -> &'static str {
        match self.kind {
            Kind::Say => "aiff",
            Kind::Espeak | Kind::Piper => "wav",
        }
    }

    /// `text` spoken with `voice` at `rate` words per minute, each the program's default when
    /// absent: the audio and how many seconds it lasts. `text-to-speech-failed` when the program
    /// fails or writes no audio Boundrail can read; `piper` needs a voice, its model file.
    pub fn speak(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: Option<u32>,
        checkout: &Path,
    ) -> Result<(Vec<u8>, f64), Unrendered> {
        let failed = |message: String| Unrendered {
            reason: "text-to-speech-failed",
            message: format!("{} ({}): {message}", self.name, self.program.display()),
        };
        let out = format!("out.{}", self.audio());
        let dir = TempDir::new("speak").map_err(|e| failed(format!("{e:#}")))?;
        let mut args: Vec<OsString> = Vec::new();
        let mut input: &[u8] = &[];
        let flags = match self.kind {
            Kind::Say => Some(("-o", "-r")),
            Kind::Espeak => Some(("-w", "-s")),
            Kind::Piper => None,
        };
        if let Some((output, rate_flag)) = flags {
            args.extend([output, &out, "-f", "in.txt"].map(OsString::from));
            if let Some(voice) = voice {
                args.extend(["-v", voice].map(OsString::from));
            }
            if let Some(rate) = rate {
                args.extend([rate_flag.into(), rate.to_string().into()]);
            }
            fs::write(dir.path().join("in.txt"), text).map_err(|e| failed(e.to_string()))?;
        } else {
            let model = voice.ok_or_else(|| {
                failed("piper needs --voice naming its voice model file".to_owned())
            })?;
            let model =
                fs::canonicalize(model).map_err(|e| failed(format!("voice model {model}: {e}")))?;
            args.extend(["--model".into(), model.into_os_string()]);
            args.extend(["--output_file", &out].map(OsString::from));
            if let Some(rate) = rate {
                let scale = PIPER_RATE / f64::from(rate);
                args.extend(["--length_scale".into(), format!("{scale:.3}").into()]);
            }
            input = text.as_bytes();
        }
        run(&self.program, &args, dir.path(), input, checkout).map_err(failed)?;
        let audio = fs::read(dir.path().join(&out)).map_err(|e| failed(e.to_string()))?;
        let seconds =
            seconds(&audio).map_err(|why| failed(format!("wrote no readable audio: {why}")))?;
        Ok((audio, seconds))
    }
}

/// `ffmpeg` found on `PATH` outside the checkout.
pub struct Encoder {
    program: PathBuf,
}

/// `ffmpeg` on `PATH` outside `checkout`; `no-ffmpeg` when it is not there.
pub fn encoder(checkout: &Path) -> Result<Encoder, Unrendered> {
    let program = collect::resolve_program(ENCODER, checkout).map_err(|e| Unrendered {
        reason: "no-ffmpeg",
        message: format!("{e:#}"),
    })?;
    Ok(Encoder { program })
}

impl Encoder {
    /// The MP4 of `talk`, encoded in `dir`, which holds the image and audio of each beat;
    /// `ffmpeg-failed` when ffmpeg fails or writes nothing.
    pub fn encode(&self, talk: &Talk, dir: &Path, checkout: &Path) -> Result<Vec<u8>, Unrendered> {
        let failed = |message: String| Unrendered {
            reason: "ffmpeg-failed",
            message: format!("{ENCODER} ({}): {message}", self.program.display()),
        };
        run(&self.program, &encode_args(talk), dir, &[], checkout).map_err(failed)?;
        fs::read(dir.join("out.mp4")).map_err(|e| failed(e.to_string()))
    }
}

fn encode_args(talk: &Talk) -> Vec<String> {
    let (width, height, fps) = (talk.width, talk.height, talk.fps.to_string());
    let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-loglevel", "error"]
        .map(String::from)
        .into();
    let mut filter = String::new();
    let mut joined = String::new();
    for (n, beat) in talk.beats.iter().enumerate() {
        let seconds = format!("{:.3}", beat.seconds);
        let audio_format = if beat.audio.ends_with(".wav") {
            "wav"
        } else {
            "aiff"
        };
        let inputs = [
            "-protocol_whitelist",
            "file",
            "-f",
            "image2",
            "-pattern_type",
            "none",
            "-loop",
            "1",
            "-framerate",
            &fps,
            "-t",
            &seconds,
            "-i",
            &beat.image,
            "-protocol_whitelist",
            "file",
            "-f",
            audio_format,
            "-i",
            &beat.audio,
        ];
        args.extend(inputs.map(String::from));
        let (video, audio) = (2 * n, 2 * n + 1);
        let _ = write!(
            filter,
            "[{video}:v]scale={width}:{height}:force_original_aspect_ratio=decrease,\
             pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=white,setsar=1,format=yuv420p[v{n}];\
             [{audio}:a]aformat=sample_rates=44100:channel_layouts=mono,apad,atrim=end={seconds}[a{n}];"
        );
        let _ = write!(joined, "[v{n}][a{n}]");
    }
    let _ = write!(
        filter,
        "{joined}concat=n={}:v=1:a=1[v][a]",
        talk.beats.len()
    );
    let outputs = [
        "-filter_complex",
        &filter,
        "-map",
        "[v]",
        "-map",
        "[a]",
        "-r",
        &fps,
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-movflags",
        "+faststart",
        "-f",
        "mp4",
        "out.mp4",
    ];
    args.extend(outputs.map(String::from));
    args
}

fn run(
    program: &Path,
    args: &[impl AsRef<OsStr>],
    dir: &Path,
    input: &[u8],
    checkout: &Path,
) -> Result<(), String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(dir)
        .env("PATH", collect::path_outside(checkout));
    let out =
        collect::run_with_input(&mut command, input.to_vec()).map_err(|e| format!("{e:#}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let last = stderr.lines().rev().find(|l| !l.trim().is_empty());
    Err(format!("{}: {}", out.status, last.unwrap_or_default()))
}

/// How many seconds the WAV or AIFF (AIFF-C too) `audio` lasts, from its header.
pub fn seconds(audio: &[u8]) -> Result<f64, String> {
    let big_endian = match (audio.get(..4), audio.get(8..12)) {
        (Some(b"RIFF"), Some(b"WAVE")) => false,
        (Some(b"FORM"), Some(b"AIFF" | b"AIFC")) => true,
        _ => return Err("neither WAV nor AIFF".to_owned()),
    };
    let number = |bytes: &[u8]| {
        let four = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if big_endian {
            u32::from_be_bytes(four)
        } else {
            u32::from_le_bytes(four)
        }
    };
    let (mut byte_rate, mut data) = (None, None);
    let mut rest = &audio[12..];
    while rest.len() >= 8 {
        let (kind, size) = (&rest[..4], number(&rest[4..8]));
        let size = usize::try_from(size).unwrap_or(usize::MAX);
        let body = &rest[8..];
        let chunk = &body[..size.min(body.len())];
        match kind {
            b"COMM" if chunk.len() >= 18 => {
                let rate = extended(&chunk[8..18]);
                let frames = f64::from(number(&chunk[2..6]));
                return (rate > 0.0)
                    .then(|| frames / rate)
                    .ok_or_else(|| "its sample rate is 0".to_owned());
            }
            b"fmt " if chunk.len() >= 12 => byte_rate = Some(number(&chunk[8..12])),
            b"data" => data = Some(chunk.len()),
            _ => {}
        }
        if let (Some(rate), Some(length)) = (byte_rate, data) {
            if rate == 0 {
                return Err("its byte rate is 0".to_owned());
            }
            let length = u32::try_from(length).unwrap_or(u32::MAX);
            return Ok(f64::from(length) / f64::from(rate));
        }
        let next = (size.saturating_add(size % 2)).min(body.len());
        rest = &body[next..];
    }
    Err("its header names no length".to_owned())
}

fn extended(bytes: &[u8]) -> f64 {
    let exponent = i32::from(u16::from_be_bytes([bytes[0], bytes[1]]) & 0x7fff);
    let mut mantissa = [0; 8];
    mantissa.copy_from_slice(&bytes[2..10]);
    let mantissa = u64::from_be_bytes(mantissa) as f64;
    mantissa * 2f64.powi(exponent - 16383 - 63)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(frames: u32, rate: u32) -> Vec<u8> {
        let data = frames * 2;
        let mut out = b"RIFF".to_vec();
        out.extend((36 + data).to_le_bytes());
        out.extend(b"WAVEfmt ");
        out.extend(16u32.to_le_bytes());
        out.extend([1, 0, 1, 0]);
        out.extend(rate.to_le_bytes());
        out.extend((rate * 2).to_le_bytes());
        out.extend([2, 0, 16, 0]);
        out.extend(b"data");
        out.extend(data.to_le_bytes());
        out.resize(out.len() + data as usize, 0);
        out
    }

    #[test]
    fn audio_lengths_come_from_wav_and_aiff_headers() {
        assert_eq!(seconds(&wav(22050, 22050)), Ok(1.0));
        assert_eq!(seconds(&wav(11025, 22050)), Ok(0.5));
        let mut aiff = b"FORM\0\0\0\x4eAIFCFVER\0\0\0\x04\xa2\x80\x51\x40COMM\0\0\0\x12".to_vec();
        aiff.extend([
            0, 1, 0, 0, 0xa4, 0xad, 0, 16, 0x40, 0x0d, 0xac, 0x44, 0, 0, 0, 0, 0, 0,
        ]);
        let found = seconds(&aiff).unwrap();
        assert!((found - 42157.0 / 22050.0).abs() < 1e-9, "{found}");
        assert!(seconds(b"GIF89a").is_err());
        assert!(seconds(&wav(0, 0)).is_err());
        assert!(seconds(&b"RIFF\0\0\0\0WAVE"[..]).is_err());
        let playlist = b"#EXTM3U\n#EXTINF:1,\n/etc/hosts\n";
        assert!(file_problem("beat-001.png", playlist).is_some());
        assert!(file_problem("beat-001.aiff", playlist).is_some());
        assert_eq!(file_problem("beat-001.png", b"\x89PNG\r\n\x1a\n"), None);
        assert_eq!(file_problem("beat-001.wav", &wav(1, 22050)), None);
    }

    fn talk(beats: usize) -> Talk {
        Talk {
            schema_version: TALK_SCHEMA,
            walkthrough: "M#W".to_owned(),
            name: "Tour".to_owned(),
            revision: "r".to_owned(),
            width: 640,
            height: 360,
            fps: 10,
            speaker: "say".to_owned(),
            voice: None,
            rate: None,
            beats: (1..=beats)
                .map(|n| TalkBeat {
                    id: format!("M#B{n}"),
                    title: format!("Beat {n}"),
                    narration: "Hello".to_owned(),
                    image: format!("beat-{n:03}.png"),
                    audio: format!("beat-{n:03}.aiff"),
                    narration_seconds: 1.0,
                    seconds: 1.5,
                })
                .collect(),
        }
    }

    #[test]
    fn encoding_loops_each_image_for_its_beat_and_joins_them_in_order() {
        let args = encode_args(&talk(2));
        let joined = args.join(" ");
        assert!(joined.contains(
            "-protocol_whitelist file -f image2 -pattern_type none -loop 1 -framerate 10 -t 1.500 \
             -i beat-001.png -protocol_whitelist file -f aiff -i beat-001.aiff \
             -protocol_whitelist file -f image2 -pattern_type none -loop 1 -framerate 10 -t 1.500 \
             -i beat-002.png -protocol_whitelist file -f aiff -i beat-002.aiff -filter_complex"
        ));
        let inputs = args.iter().filter(|a| *a == "-i").count();
        let whitelisted = args
            .windows(2)
            .filter(|w| w == &["-protocol_whitelist", "file"]);
        assert_eq!(whitelisted.count(), inputs);
        let mut wav = talk(1);
        wav.beats[0].audio = "beat-001.wav".to_owned();
        assert!(
            encode_args(&wav)
                .join(" ")
                .contains("-f wav -i beat-001.wav")
        );
        let filter = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        assert!(filter.contains("[2:v]scale=640:360:"), "{filter}");
        assert!(filter.contains("[3:a]aformat="), "{filter}");
        assert!(
            filter.ends_with("[v0][a0][v1][a1]concat=n=2:v=1:a=1[v][a]"),
            "{filter}"
        );
        assert_eq!(args.last().map(String::as_str), Some("out.mp4"));
    }

    #[test]
    fn a_talk_refuses_odd_frames_and_names_that_are_not_plain() {
        assert_eq!(talk_problem(&talk(1)), None);
        assert!(talk_problem(&talk(0)).is_some());
        assert!(frame_problem(641, 360, 10).is_some());
        assert!(frame_problem(640, 360, 0).is_some());
        for name in [
            "../x.png",
            "-i.png",
            "http:x",
            "beat-%03d.png",
            ".png",
            "",
            "beat-001.m3u8",
            "beat-001.aiff",
            "beat.png.m3u8",
        ] {
            let mut bad = talk(1);
            bad.beats[0].image = name.to_owned();
            assert!(talk_problem(&bad).is_some(), "{name}");
        }
        for name in ["beat-001.m3u8", "beat-001.png", "beat-001.mp4", "-i.wav"] {
            let mut bad = talk(1);
            bad.beats[0].audio = name.to_owned();
            assert!(talk_problem(&bad).is_some(), "{name}");
        }
        let mut wav = talk(1);
        wav.beats[0].audio = "beat-001.wav".to_owned();
        assert_eq!(talk_problem(&wav), None);
        let mut newer = talk(1);
        newer.schema_version = TALK_SCHEMA + 1;
        assert!(talk_problem(&newer).is_some());
        let mut long = talk(1);
        long.beats[0].seconds = f64::NAN;
        assert!(talk_problem(&long).is_some());
    }
}
