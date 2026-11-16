//! Embedded pictures: the files a picture recipe draws for documentation to embed, whether each
//! still matches what the model draws, and writing them. An SVG is fresh when its bytes are the
//! render's, line endings aside. Rasterizers differ between versions and machines, so a PNG
//! carries a `tEXt` chunk naming the SHA-256 of the SVG it was rasterized from, and is fresh when
//! that is the digest of the SVG drawn now; checking it needs no rasterizer.
//!
//! Boundrail overwrites only files it drew: an SVG with its generator mark on the root element,
//! or a PNG with its source chunk. Any other file at an output blocks the picture.
//!
//! PNGs are rasterized by the first of `RASTERIZERS` found by bare name in the absolute folders
//! of `PATH` outside the checkout, so the repository never chooses the program. It runs in a
//! private temporary folder holding only the SVG, with the tool timeout.

use std::fs::{self, File};
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use super::render::GENERATOR;
use super::vault::output_problems;
use crate::collect::{self, TempDir};
use crate::files::write_atomic;

/// The `tEXt` keyword whose text is the SHA-256 of the SVG a PNG was rasterized from.
const SOURCE_KEY: &[u8] = b"boundrail-source";
/// The rasterizers tried, in order, with their arguments for the files `in.svg` and `out.png`.
const RASTERIZERS: [(&str, &[&str]); 3] = [
    ("resvg", &["in.svg", "out.png"]),
    ("rsvg-convert", &["-o", "out.png", "in.svg"]),
    ("magick", &["in.svg", "out.png"]),
];
pub(crate) const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
/// A chunk's length and type, before its data.
const CHUNK_HEAD: usize = 8;
/// A chunk's CRC, after its data.
const CHUNK_CRC: usize = 4;
const IHDR: &[u8] = b"IHDR";
const IDAT: &[u8] = b"IDAT";
const IEND: &[u8] = b"IEND";
const TEXT: &[u8] = b"tEXt";
/// How much of a PNG a check reads: the source chunk sits right after `IHDR`.
const PNG_PREFIX: u64 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Svg,
    Png,
}

impl Format {
    /// The format a validated output names by its extension.
    pub fn of(output: &str) -> Self {
        if output.ends_with(".png") {
            Self::Png
        } else {
            Self::Svg
        }
    }
}

/// A picture drawn now: its SVG and the SVG's digest, computed once.
pub struct Drawing {
    svg: String,
    digest: String,
}

impl Drawing {
    pub fn new(svg: String) -> Self {
        let digest = crate::files::hash(svg.as_bytes());
        Self { svg, digest }
    }

    pub fn svg(&self) -> &str {
        &self.svg
    }
}

/// What a picture's output holds against what it draws now.
#[derive(Debug, PartialEq, Eq)]
pub enum Found {
    Fresh,
    /// A file Boundrail drew from another model.
    Stale,
    Missing,
    /// The output cannot be read or written safely, or holds a file Boundrail did not draw: why.
    Blocked(String),
}

/// Why a tool made no file: `reason` is `no-rasterizer` or `rasterizer-failed` for a PNG, and
/// names the missing or failing speaker or encoder for a recording (`super::record`).
#[derive(Debug)]
pub struct Unrendered {
    pub reason: &'static str,
    pub message: String,
}

/// Compares the file at `output`, relative to `checkout`, with `drawing`. No symbolic link is
/// followed, and a PNG is read only as far as its source chunk.
pub fn compare(checkout: &Path, output: &str, drawing: &Drawing) -> Found {
    let path = match locate(checkout, output, false) {
        Ok(Some(path)) => path,
        Ok(None) => return Found::Missing,
        Err(why) => return Found::Blocked(why),
    };
    let format = Format::of(output);
    let bytes = match read(&path, format) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Found::Missing,
        Err(error) => return Found::Blocked(format!("{output} cannot be read: {error}")),
    };
    let fresh = match format {
        Format::Svg if same_text(&bytes, &drawing.svg) => return Found::Fresh,
        Format::Svg => marked(&bytes).then_some(false),
        Format::Png => source(&bytes).map(|s| s == drawing.digest),
    };
    match fresh {
        Some(true) => Found::Fresh,
        Some(false) => Found::Stale,
        None => Found::Blocked(not_drawn(output)),
    }
}

/// The file at `path`: all of an SVG, or a PNG as far as its source chunk can sit.
fn read(path: &Path, format: Format) -> std::io::Result<Vec<u8>> {
    let limit = match format {
        Format::Svg => u64::MAX,
        Format::Png => PNG_PREFIX,
    };
    let mut bytes = Vec::new();
    File::open(path)?.take(limit).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn not_drawn(output: &str) -> String {
    format!("{output} exists and was not drawn by Boundrail; delete it to draw it here")
}

/// Why the file at `path` may not be replaced: it exists and carries neither the generator mark
/// nor a valid source chunk, or cannot be read.
fn refuse_foreign(path: &Path, output: &str) -> Result<(), String> {
    let format = Format::of(output);
    let bytes = match read(path, format) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("{output} cannot be read: {error}")),
    };
    let drawn = match format {
        Format::Svg => marked(&bytes),
        Format::Png => source(&bytes).is_some(),
    };
    if drawn {
        Ok(())
    } else {
        Err(not_drawn(output))
    }
}

/// Whether `bytes` are `svg`, or `svg` with CRLF line endings, as a checkout may store text.
fn same_text(bytes: &[u8], svg: &str) -> bool {
    if bytes == svg.as_bytes() {
        return true;
    }
    bytes.windows(2).any(|w| w == b"\r\n")
        && String::from_utf8_lossy(bytes).replace("\r\n", "\n") == svg
}

/// Whether the root element of the SVG `bytes` carries Boundrail's generator mark.
fn marked(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let root = text.find("<svg").and_then(|at| {
        let tag = &text[at..];
        tag.find('>').map(|end| &tag[..end])
    });
    root.is_some_and(|tag| tag.contains(GENERATOR))
}

/// Writes `bytes` to `output` atomically, creating its missing folders, all inside `checkout`;
/// why not otherwise. Refuses an output a picture may not name, a symbolic link or a non-folder
/// on the way, an output that is not a regular file, and a file Boundrail did not draw, judged
/// again right before the write since the file may have changed after the comparison.
pub fn write(checkout: &Path, output: &str, bytes: &[u8]) -> Result<(), String> {
    let path = locate(checkout, output, true)?.ok_or("a created path is located")?;
    refuse_foreign(&path, output)?;
    write_atomic(&path, bytes).map_err(|error| format!("writing {output}: {error:#}"))
}

/// The path of `output` below `checkout`, checking the output's rules, then each folder on the
/// way and the file without following links; `None` when a folder is missing and `create` is not
/// set. A valid output has no `..`, so every folder created lies inside the checkout.
// ponytail: checked then used, not opened relative to a folder handle; a link swapped in between
// is followed. Open with O_NOFOLLOW per segment if outputs land where others write.
fn locate(checkout: &Path, output: &str, create: bool) -> Result<Option<PathBuf>, String> {
    let problems = output_problems(output);
    if !problems.is_empty() {
        return Err(problems.join("; "));
    }
    let segments: Vec<&str> = output.split('/').collect();
    let (file, folders) = segments.split_last().ok_or("the output is empty")?;
    let mut path = checkout.to_path_buf();
    for (n, folder) in folders.iter().enumerate() {
        path.push(folder);
        let shown = segments[..=n].join("/");
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!("{shown} is a symbolic link"));
            }
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(format!("{shown} is not a folder")),
            Err(error) if error.kind() == ErrorKind::NotFound && create => {
                fs::create_dir(&path).map_err(|e| format!("creating {shown}: {e}"))?;
            }
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("{shown} cannot be read: {error}")),
        }
    }
    path.push(file);
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(format!("{output} is a symbolic link")),
        Ok(meta) if !meta.is_file() => Err(format!("{output} is not a regular file")),
        Ok(_) => Ok(Some(path)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Some(path)),
        Err(error) => Err(format!("{output} cannot be read: {error}")),
    }
}

/// `svg`, a Boundrail render, scaled by `scale` and cut to `region` (x, y, width and height in
/// picture units from its top-left corner) by rewriting the size and viewBox of its root element,
/// which every rasterizer honors; why not when the region is empty or leaves the picture.
pub fn reframe(svg: &str, scale: f64, region: Option<[f64; 4]>) -> Result<String, String> {
    let malformed = || "the drawing has no viewBox".to_owned();
    let root = &svg[..svg.find('>').ok_or_else(malformed)?];
    let start = root.find(" width=\"").ok_or_else(malformed)?;
    let view = root.find("viewBox=\"").ok_or_else(malformed)? + "viewBox=\"".len();
    let end = view + root[view..].find('"').ok_or_else(malformed)?;
    let numbers: Vec<f64> = (root[view..end].split(' '))
        .filter_map(|n| n.parse().ok())
        .collect();
    let [left, top, width, height] = numbers[..] else {
        return Err(malformed());
    };
    let [x, y, w, h] = region.unwrap_or([0.0, 0.0, width, height]);
    if !(w > 0.0 && h > 0.0 && x >= 0.0 && y >= 0.0 && x + w <= width && y + h <= height) {
        return Err(format!(
            "--region {x},{y},{w},{h} is not inside the picture, 0,0,{width},{height}"
        ));
    }
    let size = format!(
        " width=\"{:.0}\" height=\"{:.0}\" viewBox=\"{} {} {w} {h}\"",
        (w * scale).ceil().max(1.0),
        (h * scale).ceil().max(1.0),
        left + x,
        top + y,
    );
    Ok(format!("{}{size}{}", &svg[..start], &svg[end + 1..]))
}

/// A rasterizer found on `PATH` outside the checkout.
pub struct Rasterizer {
    pub name: &'static str,
    args: &'static [&'static str],
    program: PathBuf,
}

/// The first of `RASTERIZERS` on `PATH` outside `checkout`; `no-rasterizer` when none is.
pub fn rasterizer(checkout: &Path) -> Result<Rasterizer, Unrendered> {
    let found = collect::first_program(&RASTERIZERS, |(name, _)| name, checkout);
    let found = found.map(|(&(name, args), program)| Rasterizer {
        name,
        args,
        program,
    });
    found.ok_or_else(|| {
        let names: Vec<&str> = RASTERIZERS.iter().map(|(name, _)| *name).collect();
        Unrendered {
            reason: "no-rasterizer",
            message: format!("no rasterizer on PATH; tried {}", names.join(", ")),
        }
    })
}

/// The PNG of `drawing` from the first rasterizer on `PATH` outside `checkout`, carrying the
/// SVG's digest.
pub fn rasterize(drawing: &Drawing, checkout: &Path) -> Result<Vec<u8>, Unrendered> {
    rasterizer(checkout)?.rasterize(drawing, checkout)
}

impl Rasterizer {
    /// The PNG of `drawing`, carrying the SVG's digest; `rasterizer-failed` when the program
    /// fails or writes no valid PNG.
    pub fn rasterize(&self, drawing: &Drawing, checkout: &Path) -> Result<Vec<u8>, Unrendered> {
        let Rasterizer {
            name,
            args,
            program,
        } = self;
        let failed = |message: String| Unrendered {
            reason: "rasterizer-failed",
            message: format!("{name} ({}): {message}", program.display()),
        };
        let dir = TempDir::new("picture").map_err(|e| failed(format!("{e:#}")))?;
        fs::write(dir.path().join("in.svg"), &drawing.svg).map_err(|e| failed(e.to_string()))?;
        let mut command = Command::new(program);
        command
            .args(*args)
            .current_dir(dir.path())
            .env("PATH", collect::path_outside(checkout));
        let out = collect::run(&mut command).map_err(|e| failed(format!("{e:#}")))?;
        if let Some(why) = collect::failure(&out) {
            return Err(failed(why));
        }
        let png = fs::read(dir.path().join("out.png")).map_err(|e| failed(e.to_string()))?;
        let tagged = with_source(&png, &drawing.digest);
        tagged.map_err(|why| failed(format!("wrote no valid PNG: {why}")))
    }
}

/// The CRC-32 (ISO 3309, as PNG uses it) of `parts` in order.
pub fn crc32(parts: &[&[u8]]) -> u32 {
    let mut crc = !0u32;
    for &byte in parts.iter().copied().flatten() {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// One chunk of a PNG: its type, its data, and all its bytes.
struct Chunk<'a> {
    kind: &'a [u8],
    data: &'a [u8],
    raw: &'a [u8],
}

impl Chunk<'_> {
    fn crc_holds(&self) -> bool {
        let crc = &self.raw[self.raw.len() - CHUNK_CRC..];
        crc32(&[self.kind, self.data]).to_be_bytes() == crc
    }

    /// The text of Boundrail's source chunk; `None` for any other chunk.
    fn source(&self) -> Option<&str> {
        let text = (self.kind == TEXT).then_some(self.data)?;
        let rest = text.strip_prefix(SOURCE_KEY)?.strip_prefix(b"\0")?;
        std::str::from_utf8(rest).ok()
    }
}

/// The first chunk of `bytes` and what follows it.
fn next_chunk(bytes: &[u8]) -> Result<(Chunk<'_>, &[u8]), String> {
    let head = bytes.get(..CHUNK_HEAD).ok_or("a chunk is cut short")?;
    let length = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
    let length = usize::try_from(length).map_err(|_| "a chunk is too long")?;
    let end = (CHUNK_HEAD + length)
        .checked_add(CHUNK_CRC)
        .filter(|end| *end <= bytes.len())
        .ok_or("a chunk is cut short")?;
    let chunk = Chunk {
        kind: &head[4..CHUNK_HEAD],
        data: &bytes[CHUNK_HEAD..CHUNK_HEAD + length],
        raw: &bytes[..end],
    };
    Ok((chunk, &bytes[end..]))
}

/// The chunks of `png`, refusing a missing signature, a chunk past the end, a bad CRC, or a
/// stream not starting with `IHDR` and ending with `IEND`.
fn chunks(png: &[u8]) -> Result<Vec<Chunk<'_>>, String> {
    let mut rest = png.strip_prefix(SIGNATURE).ok_or("no PNG signature")?;
    let mut out = Vec::new();
    while !rest.is_empty() {
        let (chunk, tail) = next_chunk(rest)?;
        if !chunk.crc_holds() {
            let kind = String::from_utf8_lossy(chunk.kind);
            return Err(format!("chunk {kind} has a bad CRC"));
        }
        out.push(chunk);
        rest = tail;
    }
    let (first, last) = (out.first().map(|c| c.kind), out.last().map(|c| c.kind));
    if first != Some(IHDR) || last != Some(IEND) {
        return Err("it does not start with IHDR and end with IEND".to_owned());
    }
    Ok(out)
}

/// `png` with one source chunk holding `digest` right after `IHDR`, replacing any it held. The
/// other chunks are copied as they are.
fn with_source(png: &[u8], digest: &str) -> Result<Vec<u8>, String> {
    let mut out = SIGNATURE.to_vec();
    for chunk in chunks(png)? {
        if chunk.source().is_some() {
            continue;
        }
        out.extend(chunk.raw);
        if chunk.kind == IHDR {
            let text = [SOURCE_KEY, b"\0", digest.as_bytes()].concat();
            put(&mut out, TEXT, &text);
        }
    }
    Ok(out)
}

fn put(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
    let length = u32::try_from(data.len()).expect("a chunk Boundrail writes is under 4 GiB");
    out.extend(length.to_be_bytes());
    out.extend(kind);
    out.extend(data);
    out.extend(crc32(&[kind, data]).to_be_bytes());
}

/// The digest the source chunk of `png` names, walking the chunks after `IHDR` until `IDAT`;
/// `None` when it holds none, its CRC fails, or the bytes are not a PNG. `png` may be a prefix.
pub fn source(png: &[u8]) -> Option<String> {
    let (ihdr, mut rest) = next_chunk(png.strip_prefix(SIGNATURE)?).ok()?;
    if ihdr.kind != IHDR {
        return None;
    }
    loop {
        let (chunk, tail) = next_chunk(rest).ok()?;
        if chunk.kind == IDAT || chunk.kind == IEND {
            return None;
        }
        if let Some(text) = chunk.source() {
            return chunk.crc_holds().then(|| text.to_owned());
        }
        rest = tail;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid 1x1 grayscale PNG with `extra` chunks between `IHDR` and `IDAT`.
    fn png(extra: &[(&[u8], &[u8])]) -> Vec<u8> {
        let mut out = SIGNATURE.to_vec();
        put(&mut out, IHDR, &[0, 0, 0, 1, 0, 0, 0, 1, 8, 0, 0, 0, 0]);
        for (kind, data) in extra {
            put(&mut out, kind, data);
        }
        // zlib of one filter byte and one pixel.
        put(
            &mut out,
            IDAT,
            &[0x78, 0x01, 0x63, 0x60, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01],
        );
        put(&mut out, IEND, &[]);
        out
    }

    fn kinds(png: &[u8]) -> Vec<String> {
        let chunks = chunks(png).unwrap();
        (chunks.iter())
            .map(|c| String::from_utf8_lossy(c.kind).into_owned())
            .collect()
    }

    #[test]
    fn crc32_matches_known_vectors() {
        assert_eq!(crc32(&[]), 0);
        assert_eq!(crc32(&[b"123456789"]), 0xCBF4_3926);
        assert_eq!(crc32(&[b"1234", b"", b"56789"]), 0xCBF4_3926);
        let fox: &[u8] = b"The quick brown fox jumps over the lazy dog";
        assert_eq!(crc32(&[fox]), 0x414F_A339);
        assert_eq!(crc32(&[IEND]), 0xAE42_6082);
    }

    #[test]
    fn the_source_chunk_round_trips_and_keeps_the_png_valid() {
        let plain = png(&[(TEXT, b"Comment\0hello")]);
        assert_eq!(source(&plain), None);
        let tagged = with_source(&plain, "abc").unwrap();
        assert_eq!(source(&tagged).as_deref(), Some("abc"));
        assert_eq!(kinds(&tagged), ["IHDR", "tEXt", "tEXt", "IDAT", "IEND"]);
        // The other chunks are copied byte for byte.
        assert!(tagged.ends_with(&plain[plain.len() - 34..]));
        // Tagging again replaces the chunk rather than adding one.
        let again = with_source(&tagged, "def").unwrap();
        assert_eq!(source(&again).as_deref(), Some("def"));
        assert_eq!(kinds(&again), kinds(&tagged));
        assert_eq!(again.len(), tagged.len());
        // A check reads only a prefix; the chunk sits near the start.
        assert_eq!(source(&tagged[..70]).as_deref(), Some("abc"));
    }

    #[test]
    fn a_damaged_png_carries_no_source() {
        let tagged = with_source(&png(&[]), "abc").unwrap();
        let mut bad_crc = tagged.clone();
        // The last byte of the source chunk's text.
        let at = SIGNATURE.len() + 25 + CHUNK_HEAD + SOURCE_KEY.len() + 3;
        bad_crc[at] ^= 1;
        assert_eq!(source(&bad_crc), None);
        assert!(with_source(&bad_crc, "abc").is_err());
        assert!(
            with_source(&tagged[..tagged.len() - 12], "abc").is_err(),
            "IEND missing"
        );
        assert!(with_source(b"GIF89a", "abc").is_err());
        assert!(with_source(&tagged[..tagged.len() - 3], "abc").is_err());
        assert_eq!(source(b"GIF89a"), None);
    }

    #[test]
    fn svgs_match_whatever_their_line_endings_and_carry_the_mark() {
        let svg = format!("<svg {GENERATOR}>\n<rect/>\n</svg>\n");
        assert!(same_text(svg.as_bytes(), &svg));
        assert!(same_text(svg.replace('\n', "\r\n").as_bytes(), &svg));
        assert!(!same_text(b"<svg>\n</svg>\n", &svg));
        assert!(marked(svg.as_bytes()));
        assert!(!marked(
            b"<svg xmlns=\"x\"><g data-generator=\"boundrail\"/></svg>"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn outputs_resolve_without_following_links() {
        let temp = TempDir::new("picture-locate").unwrap();
        let root = temp.path();
        let drawing = |svg: &str| Drawing::new(svg.to_owned());
        assert_eq!(locate(root, "docs/a.svg", false), Ok(None));
        let created = locate(root, "docs/deep/a.svg", true).unwrap().unwrap();
        assert_eq!(created, root.join("docs/deep/a.svg"));
        assert!(root.join("docs/deep").is_dir());
        assert!(locate(root, "../a.svg", true).is_err());
        std::os::unix::fs::symlink(root.join("docs"), root.join("linked")).unwrap();
        assert_eq!(
            locate(root, "linked/a.svg", true),
            Err("linked is a symbolic link".to_owned())
        );
        std::os::unix::fs::symlink(root.join("x.svg"), root.join("docs/a.svg")).unwrap();
        let blocked = Found::Blocked("docs/a.svg is a symbolic link".to_owned());
        assert_eq!(compare(root, "docs/a.svg", &drawing("<svg/>")), blocked);
        let ours = format!("<svg {GENERATOR}/>");
        fs::write(root.join("docs/deep/a.svg"), &ours).unwrap();
        assert_eq!(
            compare(root, "docs/deep/a.svg", &drawing(&ours)),
            Found::Fresh
        );
        let other = drawing("<svg></svg>");
        assert_eq!(compare(root, "docs/deep/a.svg", &other), Found::Stale);
        fs::write(root.join("docs/deep/a.svg"), "<svg/>").unwrap();
        let foreign = compare(root, "docs/deep/a.svg", &other);
        assert!(matches!(foreign, Found::Blocked(why) if why.contains("not drawn by Boundrail")));
        assert_eq!(compare(root, "docs/deep/b.svg", &other), Found::Missing);
    }

    #[test]
    fn a_write_refuses_a_file_that_turned_foreign_after_the_comparison() {
        let temp = TempDir::new("picture-write").unwrap();
        let root = temp.path();
        let ours = format!("<svg {GENERATOR}/>");
        // Compared as missing, then another program writes its own file there.
        assert_eq!(write(root, "docs/a.svg", ours.as_bytes()), Ok(()));
        fs::write(root.join("docs/a.svg"), "<svg/>").unwrap();
        let refused = write(root, "docs/a.svg", ours.as_bytes()).unwrap_err();
        assert!(refused.contains("not drawn by Boundrail"), "{refused}");
        assert_eq!(
            fs::read_to_string(root.join("docs/a.svg")).unwrap(),
            "<svg/>"
        );
        // A PNG is ours only with a valid source chunk.
        let tagged = with_source(&png(&[]), "abc").unwrap();
        fs::write(root.join("docs/a.png"), png(&[])).unwrap();
        assert!(write(root, "docs/a.png", &tagged).is_err());
        fs::write(root.join("docs/a.png"), &tagged).unwrap();
        assert_eq!(write(root, "docs/a.png", &tagged), Ok(()));
    }
}
