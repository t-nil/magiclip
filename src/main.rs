#![feature(lazy_cell)]
#![feature(exit_status_error)]
#![feature(path_file_prefix)]
#![deny(clippy::suspicious)]
#![deny(clippy::perf)]
#![warn(clippy::style)]
#![warn(clippy::pedantic)]

use clap::Parser;
use itertools::Itertools as _;
use std::{
    collections::HashMap,
    io::stdin,
    path::{Component, Path, PathBuf},
};
use sub::old::SubContainedByFile;

mod cli;
mod clip;
mod ffmpeg;
mod fzf;
mod sub;
mod util;

static _REGEX_SUBFILE: &str = r"(.\w{2})?.srt";
const LINE_BREAK: char = '\n';
const OUTPUT_DIR: &str = "/tmp/tmp.Z3fu02h0P5";
const MAX_FILENAME_LEN: usize = 64;

#[allow(clippy::too_many_lines)]
fn main() -> anyhow::Result<()> {
    let args = cli::Args::parse();

    let (files, file_errors): (Vec<_>, Vec<_>) = stdin()
        .lines()
        .map_ok(|l| {
            let outpath = PathBuf::from(OUTPUT_DIR).join(
                Path::new(&l)
                    .canonicalize()?
                    .components()
                    .map(|c| {
                        if c == std::path::Component::RootDir {
                            Component::Normal("!…root…!".as_ref())
                        } else {
                            c
                        }
                    })
                    .collect::<PathBuf>(),
            );
            ffmpeg::get_sub_files_in_dir(
                &l,
                outpath
                    .parent()
                    .expect("No parent on outfile? Probably something fishy here"),
            )
        })
        .flatten_ok()
        .flatten_ok()
        .partition_result();

    //files.iter().for_each(|f| println!("{f:?}"));
    for e in file_errors {
        println!("Error getting subtitle files: {e:?}");
    }

    let (subs, sub_errors): (Vec<_>, Vec<anyhow::Error>) = files
        .iter()
        .map(|f| {
            let test = sub::parse_from_file(f)?
                .into_iter()
                .map(|s| SubContainedByFile(s, f))
                .collect_vec();
            Ok(test)
        })
        .flatten_ok()
        .partition_result();

    for e in sub_errors {
        println!("Error parsing subtitles: {e:?}");
    }

    let search_strings: HashMap<_, _> = subs
        .iter()
        .map(|s| {
            (
                format!(
                    "{} ({}, [{} - {}])",
                    s.0.text.replace(LINE_BREAK, "↳"),
                    s.1.to_string_lossy(),
                    s.0.start_time,
                    s.0.end_time
                ),
                s,
            )
        })
        .collect();
    let search_results = fzf::select(&(search_strings.keys()).collect_vec())?
        .iter()
        .map(|s| {
            search_strings
                .get(s)
                .unwrap_or_else(|| panic!("IMPOSSIBLE: {s}: not found in search entry hash map"))
        })
        .collect_vec();

    search_results
        .iter()
        .map(|clip| {
            let infile = clip
                .1
                .parent()
                .expect("No parent on infile? Probably something fishy here")
                .strip_prefix(PathBuf::from(OUTPUT_DIR))?
                .components()
                .map(|c| {
                    if c == Component::Normal("!…root…!".as_ref()) {
                        Component::RootDir
                    } else {
                        c
                    }
                })
                .collect::<PathBuf>();
            let outfile =
                PathBuf::from(OUTPUT_DIR)
                    .join("!out")
                    .join(util::escape_for_unix_filename(&format!(
                        "{} ({}, [{}], p={})",
                        &(clip
                            .0
                            .text
                            .chars()
                            .take(MAX_FILENAME_LEN)
                            .collect::<String>()),
                        infile
                            .file_stem()
                            .map_or_else(|| "…empty…".to_owned().into(), |s| s.to_string_lossy()),
                        &clip.0.start_time,
                        args.profile
                    )));
            std::fs::create_dir_all(
                outfile
                    .parent()
                    .expect("No parent on outfile? Probably something fishy here"),
            )?;

            ffmpeg::clip(
                &infile,
                &outfile,
                clip.0.start_time,
                clip.0.end_time,
                args.profile,
            )?;
            anyhow::Ok(outfile)
        })
        .for_each(|f| println!("{f:?}"));

    Ok(())
}
