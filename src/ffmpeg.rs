use std::{
    borrow::Cow,
    default,
    ffi::OsStr,
    fs::create_dir_all,
    io::BufRead,
    ops::Not as _,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{ensure, Result};
use itertools::Itertools as _;
use srtlib::{Subtitle, Timestamp};

pub fn get_sub_files_in_dir(
    p: impl AsRef<Path>,
    output_dir: impl AsRef<Path>,
) -> Result<Vec<PathBuf>> {
    let output_dir = PathBuf::from(output_dir.as_ref()).join(
        p.as_ref()
            .file_name()
            .map(OsStr::to_string_lossy)
            .unwrap_or(Cow::Borrowed("…empty…"))
            .as_ref(),
    );
    create_dir_all(&output_dir)?;

    Ok(match p.as_ref() {
        t if t.is_dir() => std::fs::read_dir(p)?
            .flat_map(|entry| {
                let entry = entry?;
                let output_dir = (&output_dir).clone();
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
        .map(|i| {
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
        .flatten()
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
                .map(|ref s| s.trim() == "[STREAM]")
                .unwrap_or(false)
        })
        .count())
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct VideoEncodingSettings {
    // TODO bounds/type check these params
    pub crf: u8,

    pub preset: u8,
}

impl Default for VideoEncodingSettings {
    fn default() -> Self {
        Self { crf: 30, preset: 6 }
    }
}
pub struct AudioEncodingSettings {
    pub acodec: &'static str,

    pub b_a: &'static str,
}

impl Default for AudioEncodingSettings {
    fn default() -> Self {
        Self {
            acodec: "libopus",
            b_a: "92k",
        }
    }
}

pub enum EncodingSettings {
    AudioAndVideo(VideoEncodingSettings, AudioEncodingSettings),
    AudioOnly(AudioEncodingSettings),
}

// TODO encoding settings
/// Clips `sub` belonging to `file`
/*pub fn clip_one(sub: &Subtitle, file: &Path) {
    sub.start_time
}*/

pub fn clip(start: &Timestamp, end: &Timestamp, infile: &Path, outfile: &Path) -> Result<()> {
    ensure!(end > start);
    let mut duration = end.clone();
    duration.sub(&start);
    std::mem::drop(end);

    let (start, duration) = (ffmpeg_duration(start), ffmpeg_duration(&duration));
    _clip(&start, &duration, infile, outfile)
}

fn _clip(start: &str, duration: &str, infile: &Path, outfile: &Path) -> Result<()> {
    let out = dbg!(Command::new("ffmpeg").args([
        "-ss",
        start,
        "-i",
        infile.to_string_lossy().as_ref(),
        "-t",
        duration,
        "-c:v",
        "libsvtav1",
        "-crf",
        "24",
        "-preset",
        "6",
        "-c:a",
        "libopus",
        "-b:a",
        "92k",
        &(outfile.to_string_lossy().as_ref().to_owned() + ".mkv"),
    ]))
    .output()?;
    ensure!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    Ok(())
}

/// # Examples
///
/// ```
/// assert_eq!(ffmpeg_duration(Timestamp::new(1, 2, 3, 50)), "01:02:03.050");
/// ```
///
/// # From FFmpeg manual
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
fn ffmpeg_duration(t: &Timestamp) -> String {
    let (h, m, s, ms) = t.get();
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

#[cfg(test)]
mod test {
    use srtlib::Timestamp;

    #[test]
    fn ffmpeg_duration() {
        assert_eq!(
            &super::ffmpeg_duration(&Timestamp::new(1, 2, 3, 50)),
            "01:02:03.050"
        );
    }
}

//pub fn get_sub_files_in_dir(p: impl AsRef<Path>) -> Result<Vec<impl AsRef<Path>>> {
//    _get_sub_files_in_dir(p, || Ok(tempfile::tempfile()?))
//}
