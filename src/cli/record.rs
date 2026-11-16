//! `model record`: a walkthrough recorded to an MP4 with a local voice, beside a talk folder from
//! which `--replay` encodes the MP4 again without rendering or speaking.

use super::*;
use boundrail::arch::picture::{Drawing, Unrendered};
use boundrail::arch::record::{self, ENCODER, MANIFEST, TALK_SCHEMA, Talk, TalkBeat};
use boundrail::collect::TempDir;

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const FPS: u32 = 30;
const RATES: std::ops::RangeInclusive<u32> = 80..=450;
const MANIFEST_MAX: u64 = 1 << 20;
const BEAT_FILE_MAX: u64 = 64 << 20;

#[derive(Args)]
pub(super) struct RecordArgs {
    /// The walkthrough to record: its ID or unique name
    #[arg(required_unless_present = "replay", conflicts_with = "replay")]
    walkthrough: Option<String>,
    /// Encode the MP4 again from this talk folder, which an earlier recording left, without
    /// rendering or speaking; needs ffmpeg only
    #[arg(long)]
    replay: Option<PathBuf>,
    /// The MP4 file to write; its folder must exist, outside the model vault and the Git folder.
    /// A recording also writes the talk folder beside it, the file's name with `.talk`
    #[arg(long)]
    output: PathBuf,
    /// The voice the text-to-speech program speaks with: a `say` or `espeak-ng` voice name, or
    /// the voice model file for `piper` [default: the program's]
    #[arg(long, conflicts_with = "replay")]
    voice: Option<String>,
    /// Words per minute, 80 to 450 [default: the program's]
    #[arg(long, conflicts_with = "replay")]
    rate: Option<u32>,
    /// Frame width in pixels, even [default: 1920, or the talk folder's]
    #[arg(long)]
    width: Option<u32>,
    /// Frame height in pixels, even [default: 1080, or the talk folder's]
    #[arg(long)]
    height: Option<u32>,
    /// Frames per second, 1 to 60 [default: 30, or the talk folder's]
    #[arg(long)]
    fps: Option<u32>,
}

fn millis(seconds: f64) -> f64 {
    (seconds * 1000.0).round() / 1000.0
}

fn invalid(message: String) -> anyhow::Error {
    Refusal::new(Code::RequestInvalid, message).into()
}

pub(super) fn run(root: &Path, markdown: bool, args: &RecordArgs) -> Result<()> {
    check_output(root, &args.output, "--output")?;
    boundrail::files::refuse_symlink(&args.output).map_err(|e| invalid(format!("--output {e}")))?;
    if args.output.is_dir() {
        let shown = args.output.display();
        return Err(invalid(format!(
            "--output {shown} is a folder; name an MP4 file"
        )));
    }
    let checkout = checkpoint::checkout(&vault::path(root)?)?;
    let encoder = record::encoder(&checkout).map_err(refused)?;
    let dir = TempDir::new("record")?;
    let (talk, written, tools) = match &args.replay {
        Some(folder) => {
            let talk = replayed(folder, args, dir.path())?;
            let tools = json!({"encoder": ENCODER});
            (talk, folder.clone(), tools)
        }
        None => {
            let talk_dir = args.output.with_extension("talk");
            if talk_dir == args.output {
                let message = "--output ends in .talk, the talk folder's name; name an MP4";
                return Err(invalid(message.to_owned()));
            }
            refuse_foreign(&talk_dir)?;
            let (talk, rasterizer, speaker) = recorded(root, args, &checkout, dir.path())?;
            let tools = json!({"rasterizer": rasterizer, "speaker": speaker, "encoder": ENCODER});
            (talk, talk_dir, tools)
        }
    };
    let mp4 = encoder
        .encode(&talk, dir.path(), &checkout)
        .map_err(refused)?;
    if args.replay.is_none() {
        write_talk(dir.path(), &talk, &written)?;
    }
    write_file(&args.output, &mp4)?;
    let seconds = millis(talk.beats.iter().map(|b| b.seconds).sum());
    if markdown {
        let verb = if args.replay.is_some() {
            "Encoded"
        } else {
            "Recorded"
        };
        let beats = count(talk.beats.len(), "beat");
        println!(
            "{verb} {} ({beats}, {seconds} s) to {} from talk folder {}",
            talk.name,
            args.output.display(),
            written.display()
        );
        return Ok(());
    }
    write(
        None,
        &json!({
            "schema": MODEL_SCHEMA,
            "kind": "record",
            "success": true,
            "replayed": args.replay.is_some(),
            "walkthrough": talk.walkthrough,
            "revision": talk.revision,
            "output": args.output,
            "talk": written,
            "beats": talk.beats.len(),
            "seconds": seconds,
            "tools": tools,
        }),
    )
}

/// The bytes of the regular file at `path`, not a link, pipe or device, of at most `max` bytes.
fn read_regular(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("it is not a regular file".to_owned());
    }
    let too_large = || format!("it is larger than {max} bytes");
    if meta.len() > max {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    (file.take(max + 1).read_to_end(&mut bytes)).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > max {
        return Err(too_large());
    }
    Ok(bytes)
}

fn refuse_foreign(talk: &Path) -> Result<()> {
    let ours = match fs::symlink_metadata(talk) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Ok(meta) => {
            meta.is_dir()
                && read_regular(&talk.join(MANIFEST), MANIFEST_MAX)
                    .is_ok_and(|text| serde_json::from_slice::<Talk>(&text).is_ok())
        }
        Err(_) => false,
    };
    if ours {
        return Ok(());
    }
    Err(invalid(format!(
        "{} exists and is not a talk folder Boundrail wrote; delete it to record here",
        talk.display()
    )))
}

fn recorded(
    root: &Path,
    args: &RecordArgs,
    checkout: &Path,
    dir: &Path,
) -> Result<(Talk, &'static str, &'static str)> {
    let (width, height, fps) = (
        args.width.unwrap_or(WIDTH),
        args.height.unwrap_or(HEIGHT),
        args.fps.unwrap_or(FPS),
    );
    if let Some(problem) = record::frame_problem(width, height, fps) {
        return Err(invalid(problem));
    }
    if args.rate.is_some_and(|rate| !RATES.contains(&rate)) {
        let (low, high) = (RATES.start(), RATES.end());
        return Err(invalid(format!("--rate must be from {low} to {high}")));
    }
    if (args.voice.as_deref()).is_some_and(|v| v.is_empty() || v.starts_with('-')) {
        let message = "--voice names a voice; it is not empty and does not start with -";
        return Err(invalid(message.to_owned()));
    }
    let (model, _) = load_state(root, RenderState::Current)?;
    let subject = args
        .walkthrough
        .as_deref()
        .context("clap requires a walkthrough")?;
    let walkthrough = model.walkthrough(subject)?;
    if walkthrough.beats.is_empty() {
        let id = model.qualify(&walkthrough.id);
        return Err(invalid(format!("walkthrough {id} has no beats to record")));
    }
    let rasterizer = picture::rasterizer(checkout).map_err(refused)?;
    let speaker = record::speaker(checkout).map_err(refused)?;
    let mut states = States::new(&model);
    let mut drawn = Vec::with_capacity(walkthrough.beats.len());
    for beat in &walkthrough.beats {
        let (rendered, _) = draw_reading(root, &beat.reading, None, &mut states)?;
        let scale = f64::min(
            f64::from(width) / rendered.width,
            f64::from(height) / rendered.height,
        );
        drawn.push(picture::reframe(&rendered.svg, scale, None).map_err(invalid)?);
    }
    let mut images = Vec::with_capacity(drawn.len());
    for (n, svg) in (1..).zip(drawn) {
        let image = format!("beat-{n:03}.png");
        let png = (rasterizer.rasterize(&Drawing::new(svg), checkout)).map_err(refused)?;
        fs::write(dir.join(&image), png)?;
        images.push(image);
    }
    let mut beats = Vec::with_capacity(images.len());
    for ((n, beat), image) in (1..).zip(&walkthrough.beats).zip(images) {
        let audio = format!("beat-{n:03}.{}", speaker.audio());
        let (spoken, seconds) = speaker
            .speak(&beat.narration, args.voice.as_deref(), args.rate, checkout)
            .map_err(refused)?;
        fs::write(dir.join(&audio), spoken)?;
        beats.push(TalkBeat {
            id: model.qualify(&beat.id),
            title: beat.title.clone(),
            narration: beat.narration.clone(),
            image,
            audio,
            narration_seconds: millis(seconds),
            seconds: millis(seconds + record::PAD),
        });
    }
    let talk = Talk {
        schema_version: TALK_SCHEMA,
        walkthrough: model.qualify(&walkthrough.id),
        name: walkthrough.name.clone(),
        revision: model.revision.digest.clone(),
        width,
        height,
        fps,
        speaker: speaker.name.to_owned(),
        voice: args.voice.clone(),
        rate: args.rate,
        beats,
    };
    if let Some(problem) = record::talk_problem(&talk) {
        return Err(refused(Unrendered {
            reason: "text-to-speech-failed",
            message: format!(
                "{} wrote audio that cannot be encoded: {problem}",
                speaker.name
            ),
        }));
    }
    Ok((talk, rasterizer.name, speaker.name))
}

fn replayed(folder: &Path, args: &RecordArgs, dir: &Path) -> Result<Talk> {
    let shown = folder.display();
    let manifest = folder.join(MANIFEST);
    let text = read_regular(&manifest, MANIFEST_MAX).map_err(|why| {
        invalid(format!(
            "--replay {shown}: {MANIFEST} cannot be read: {why}"
        ))
    })?;
    let mut talk: Talk = serde_json::from_slice(&text).map_err(|error| {
        invalid(format!(
            "--replay {shown}: {MANIFEST} does not parse: {error}"
        ))
    })?;
    talk.width = args.width.unwrap_or(talk.width);
    talk.height = args.height.unwrap_or(talk.height);
    talk.fps = args.fps.unwrap_or(talk.fps);
    if let Some(problem) = record::talk_problem(&talk) {
        return Err(invalid(format!("--replay {shown}: {problem}")));
    }
    for name in talk.files() {
        let bytes = read_regular(&folder.join(name), BEAT_FILE_MAX)
            .and_then(|bytes| record::file_problem(name, &bytes).map_or(Ok(bytes), Err))
            .map_err(|why| invalid(format!("--replay {shown}: {name} cannot be read: {why}")))?;
        fs::write(dir.join(name), bytes)?;
    }
    Ok(talk)
}

fn write_talk(dir: &Path, talk: &Talk, folder: &Path) -> Result<()> {
    let beside = |what: &str| folder.with_extension(format!("talk-{}-{what}", std::process::id()));
    let staged = beside("new");
    fs::create_dir(&staged).with_context(|| format!("creating {}", staged.display()))?;
    let result = (|| -> Result<()> {
        for name in talk.files() {
            fs::copy(dir.join(name), staged.join(name))?;
        }
        let manifest = serde_json::to_string_pretty(talk)? + "\n";
        fs::write(staged.join(MANIFEST), manifest)?;
        refuse_foreign(folder)?;
        if fs::symlink_metadata(folder).is_ok() {
            let old = beside("old");
            fs::rename(folder, &old)?;
            fs::rename(&staged, folder)?;
            fs::remove_dir_all(old)?;
        } else {
            fs::rename(&staged, folder)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staged);
    }
    result.with_context(|| format!("writing {}", folder.display()))
}
