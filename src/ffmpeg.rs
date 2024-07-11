use std::{
    borrow::Cow,
    collections::HashMap,
    ffi::OsStr,
    fs::create_dir_all,
    io::BufRead,
    ops::Not as _,
    path::{Path, PathBuf},
    process::Command,
    sync::LazyLock,
};

use anyhow::{ensure, Result};
use itertools::Itertools as _;
use scopeguard::ScopeGuard;
use srtlib::Timestamp;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, clap::ValueEnum, strum::Display)]
#[allow(clippy::upper_case_acronyms)]
pub enum EncodingProfile {
    AV1,
    FLAC,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EncodingSettings {
    pub ext: &'static str,
    pub params: Vec<(&'static str, &'static str)>,
}

static ENCODING_PROFILES: LazyLock<HashMap<EncodingProfile, EncodingSettings>> =
    LazyLock::new(|| {
        vec![
            (
                EncodingProfile::AV1,
                EncodingSettings {
                    ext: "mkv",
                    params: vec![
                        ("-c:v", "libsvtav1"),
                        ("-crf:v", "10"),
                        ("-preset:v", "6"),
                        (
                            "-svtav1-params",
                            "tune=0:film-grain=50:film-grain-denoise=0:enable-variance-boost=1",
                        ),
                        ("-c:a", "libopus"),
                        ("-b:a", "92k"),
                        ("-ac", "2"),
                    ],
                },
            ),
            (
                EncodingProfile::FLAC,
                EncodingSettings {
                    ext: "flac",
                    params: vec![("-c:v", "none"), ("-c:a", "flac"), ("-ac", "2")],
                },
            ),
        ]
        .into_iter()
        .collect()
    });

pub fn get_sub_files_in_dir(
    p: impl AsRef<Path>,
    output_dir: impl AsRef<Path>,
) -> Result<Vec<PathBuf>> {
    let output_dir = PathBuf::from(output_dir.as_ref()).join(
        p.as_ref()
            .file_name()
            .map_or(Cow::Borrowed("…empty…"), OsStr::to_string_lossy)
            .as_ref(),
    );
    create_dir_all(&output_dir)?;

    Ok(match p.as_ref() {
        t if t.is_dir() => std::fs::read_dir(p)?
            .flat_map(|entry| {
                let entry = entry?;
                let output_dir = output_dir.clone();
                get_sub_files_in_dir(entry.path(), output_dir)
            })
            .flatten()
            .collect_vec(),
        t if t.is_symlink() => vec![],
        t if t.is_file() => get_sub_files(p, output_dir.clone().as_path())?,
        _ => vec![],
    })

    //Ok(Vec::<PathBuf>::new())

    //Command::new("ffmpeg").args(["-i"])
}

pub fn get_sub_files(p: impl AsRef<Path>, output_dir: impl AsRef<Path>) -> Result<Vec<PathBuf>> {
    Ok((0..(_how_many_subs(p.as_ref())?))
        .flat_map(|i| {
            let outfile = output_dir.as_ref().to_path_buf().join(format!("{i}.srt"));
            if outfile.exists().not() {
                let out = Command::new("ffmpeg")
                    .args(["-i", &p.as_ref().to_string_lossy(), "-map"])
                    .arg(format!("0:s:{i}"))
                    .args(["-f", "srt"])
                    .arg(&outfile)
                    .output()?;

                ensure!(out.status.success());
            }

            Ok(outfile)
        })
        .collect_vec())
}

fn _how_many_subs(p: impl AsRef<Path>) -> Result<usize> {
    let out = Command::new("ffprobe")
        .args("-v error -show_streams -select_streams s".split(' '))
        .arg(p.as_ref().as_os_str())
        .output()?;

    ensure!(out.status.success());

    Ok(out
        .stdout
        .lines()
        .filter(|l| {
            l.as_ref()
                .map(|s| s.trim() == "[STREAM]")
                .unwrap_or(false)
        })
        .count())
}

// TODO encoding settings
/// Clips `sub` belonging to `file`
/*pub fn clip_one(sub: &Subtitle, file: &Path) {
    sub.start_time
}*/

pub fn clip(
    infile: &Path,
    outfile: &Path,
    start: &Timestamp,
    end: &Timestamp,
    profile: EncodingProfile,
) -> Result<()> {
    ensure!(end > start);
    let mut duration = *end;
    duration.sub(start);

    #[allow(dropping_references)]
    std::mem::drop(end);

    let (start, duration) = (timestamp_to_string(start), timestamp_to_string(&duration));
    _clip(infile, outfile, &start, &duration, profile)
}

fn _clip(
    infile: &Path,
    outfile_basename: &Path,
    start: &str,
    duration: &str,
    profile: EncodingProfile,
) -> Result<()> {
    let settings = ENCODING_PROFILES
        .get(&profile)
        .expect("[ASSERT] not all encoding profiles covered");
    let outfile = format!("{}.{}", outfile_basename.to_string_lossy(), settings.ext);

    // delete temp file on failure
    let rm_temp = scopeguard::guard(Path::new(&outfile), |outfile| {
        let _ = std::fs::remove_file(outfile);
    });

    let out = dbg!(Command::new("ffmpeg")
        .args([
            // seek in input to sub start
            "-ss",
            start,
            "-i",
            infile.to_string_lossy().as_ref(),
            // stop encoding after sub duration
            "-t",
            duration,
        ])
        .args(settings_to_args(settings))
        .arg(&outfile))
    .output()?;
    ensure!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // defuse ScopeGuard for deleting temp
    let _ = ScopeGuard::into_inner(rm_temp);
    Ok(())
}

fn settings_to_args(settings: &EncodingSettings) -> Vec<&str> {
    let mut result = Vec::new();
    settings.params.iter().for_each(|(k, v)| {
        result.push(*k);
        if v.is_empty().not() {
            result.push(*v);
        }
    });
    result
}

/// # Examples
///
/// ```
/// assert_eq!(ffmpeg_duration(Timestamp::new(1, 2, 3, 50)), "01:02:03.050");
/// ```
///
/// # From `FFmpeg` manual
///
/// ```doc
/// Time duration
/// There are two accepted syntaxes for expressing time duration.
///
/// [-][<HH>:]<MM>:<SS>[.<m>...]
///
/// HH expresses the number of hours, MM the number of minutes for a maximum of 2 digits, and SS the number of seconds for a maximum of 2 digits. The m at the end expresses decimal  value
/// for SS.
///
/// or
///
/// [-]<S>+[.<m>...][s|ms|us]
///
/// S  expresses  the  number  of  seconds,  with  the  optional  decimal  part  m.   The optional literal suffixes s, ms or us indicate to interpret the value as seconds, milliseconds or
/// microseconds, respectively.
///
/// In both expressions, the optional - indicates negative duration.
///
/// ## Examples
///
/// The following examples are all valid time duration:
///
/// 55  55 seconds
///
/// 0.2 0.2 seconds
///
/// 200ms
/// 200 milliseconds, that's 0.2s
///
/// 200000us
/// 200000 microseconds, that's 0.2s
///
/// 12:03:45
/// 12 hours, 03 minutes and 45 seconds
///
/// 23.189
/// 23.189 seconds
/// ```
fn timestamp_to_string(t: &Timestamp) -> String {
    let (h, m, s, ms) = t.get();
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

#[cfg(test)]
mod test {
    use srtlib::Timestamp;

    #[test]
    fn ffmpeg_duration() {
        assert_eq!(
            &super::timestamp_to_string(&Timestamp::new(1, 2, 3, 50)),
            "01:02:03.050"
        );
    }
}

//pub fn get_sub_files_in_dir(p: impl AsRef<Path>) -> Result<Vec<impl AsRef<Path>>> {
//    _get_sub_files_in_dir(p, || Ok(tempfile::tempfile()?))
//}
