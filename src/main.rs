#![feature(lazy_cell)]
#![feature(exit_status_error)]
#![feature(path_file_prefix)]
#![deny(clippy::suspicious)]
#![deny(clippy::perf)]
#![warn(clippy::style)]
#![warn(clippy::pedantic)]
#![allow(clippy::default_trait_access)]

use anyhow::{anyhow, bail, Result};
use clap::Parser;
use itertools::Itertools;
use log::{error, info, warn, LevelFilter};
use rayon::{
    iter::{IntoParallelIterator as _, IntoParallelRefIterator as _, ParallelIterator as _},
    slice::ParallelSliceMut,
};
use std::path::{Path, PathBuf};
use sub::db::{self, SubDB};
use walkdir::{DirEntry, WalkDir};

mod cli;
mod clip;
mod ffmpeg;
mod fzf;
mod sub;
mod util;

/// I think 255 is upper limit for filenames on unix. Besides the components
/// mentioned below, there is a timestamp taking up approx. <15, and separation
/// chars ~5, totalling to <20 chars extra. So keep the sum of these values
/// below 235 (I guess). _Better yet_, leave a buffer, as the escaping taking place
/// could push the length up a few chars (see `crate::util::escape_for_unix_filename()`)
pub const CLIP_FILENAME_TEXT_LEN: usize = 64;
pub const CLIP_FILENAME_PATH_LEN: usize = 128;

static _REGEX_SUBFILE: &str = r"(.\w{2})?.srt";

#[allow(clippy::too_many_lines)]
fn main() -> anyhow::Result<()> {
    env_logger::builder()
        .default_format()
        .filter_level(LevelFilter::Info)
        .init();

    let args = cli::Args::parse();

    info!("Loading or creating DB…");
    let mut db = SubDB::load(args.db_file)?;
    info!("DB loaded with {n} entries", n = db.len());

    info!("Starting scan of {n} video folders…", n = args.paths.len());
    let (_, errors): (Vec<()>, Vec<_>) = populate_db(args.paths.into_iter(), &mut db)
        .into_iter()
        .partition_result();
    for err in errors {
        warn!("Error trying to populate db: {err}");
    }
    info!(
        "Scan finished. DB now consists of {n} entries",
        n = db.len()
    );

    // FIXME here may be a dividing point ("fork") between optimizing for cpu
    // or memory. Either keep all (Vec<(key, str)>, Vec<str>) in memory, or
    // only Vec<str> (but generate every str again when searching for it).
    //
    // Third option is keeping (Vec<key>, Vec<str>) which saves a little memory
    // but destroys the firm connection between key and str and relies on indices
    // (more prone to logic errors).
    //
    // 4) build a reverse HashMap with str -> key; maybe this even has the most
    // even ram:cpu ratio and best performance overall.
    //
    // For starters, go with 1).
    info!("Formatting search strings…");
    let search_map = db.as_identifying_strings().collect::<Vec<_>>();
    let search_strings = search_map.iter().map(|(_, str)| str).collect_vec();

    let mut search_results = fzf::select(&search_strings)?;

    info!(
        "Sorting {count} results (and detecting duplicates)…",
        count = search_results.len()
    );
    search_results.as_mut_slice().par_sort();
    let search_results = search_results.iter().dedup_with_count().collect_vec();

    for (count, too_much) in search_results.iter().filter(|(count, _)| *count > 1) {
        error!("Search string appeared more than once ({count}x) in the result: {too_much}. This is a hard error, because it would lead to files beìng written to multiple times.");
    }

    info!("Looking them up in the map…");
    let search_results = search_results
        .into_par_iter()
        .map(|(_, str)| str)
        .map(|s| {
            search_map
                .iter()
                .find(|(_, str)| str == s)
                .unwrap_or_else(|| {
                    panic!("IMPOSSIBLE: {s}: not found in search entry hash map (LOGIC ERROR)")
                })
        })
        .collect::<Vec<_>>();

    info!("Launching parallel clip creation");
    search_results.par_iter().map(|(key, line)| {
        info!("Preparing \"{line}\"");
        let target_entry = match db.lookup(key)? {
            db::EntryFound::Yes(entry) => entry,
            db::EntryFound::YesButGone |            db::EntryFound::YesButChanged => bail!("While clipping, file changed right under our a$$es ({key:?})"),
            db::EntryFound::No => panic!("LOGIC ERROR: took key directly from map, but map doesn't know about it anymore ({key:?})"),
        };
        // TODO PERF maybe I don't have to recalc every sub string but instead can
        // keep the sub around. OR parallelize.
        let target_sub = target_entry.sub_files().par_iter().flat_map(|(_, subs)|subs).find_any(|sub| &sub.as_identifying_string(&key.video_path, Default::default()) == line).expect("LOGIC ERROR: The entry under $key doesn't have a corresponding sub line ({entry:?})");
        let outfile = target_sub.as_identifying_string(target_entry.meta().video_path(), sub::SubtitleStringFormatOptions::Filename);
        let profile_string = args.profile.to_string();
        let outfile = args.clip_dir.join(if args.subdir_per_profile {&profile_string} else {""}).join(outfile);

        info!("Clipping \"{line}\"");
        ffmpeg::clip(target_entry.meta().video_path(), outfile, target_sub.start_time, target_sub.end_time, args.profile)?;

        info!("\"{line}\" done!");
        Ok(())
    }).for_each(|result| if let Err(e) = result { error!("One of the clips failed: {e}") });
    Ok(())
}

fn populate_db(paths: impl Iterator<Item = PathBuf>, db: &mut sub::db::SubDB) -> Vec<Result<()>> {
    fn only_files(path: PathBuf) -> Option<Result<PathBuf>> {
        (move || {
            if path.metadata()?.is_file() {
                anyhow::Ok(Some(path))
            } else {
                Ok(None)
            }
        })()
        .transpose()
    }
    fn walk(path: impl AsRef<Path>) -> impl Iterator<Item = Result<PathBuf>> {
        WalkDir::new(path.as_ref())
            .min_depth(1)
            .into_iter()
            .map_ok(DirEntry::into_path)
            .filter_map_ok(only_files)
            .flatten_ok()
            .map(to_anyhow)
    }

    #[allow(clippy::ptr_arg)] // otherwise fn sig doesn't match when passing to filter()
    fn has_movie_ext(path: &PathBuf) -> bool {
        let Some(ext) = path.extension() else {
            return false;
        };
        ffmpeg::VIDEO_EXTS.iter().any(|movie_ext| ext == *movie_ext)
    }

    let possible_files = paths
        .map(|entry| {
            if entry.is_symlink() {
                bail!("No symlinks! ({entry:?})");
            }
            if entry.is_file() {
                return Ok(vec![Ok(entry)]);
            }
            if entry.is_dir() {
                return Ok(walk(entry).collect_vec());
            }
            bail!("{entry:?} is neither symlink, file nor dir.")
        })
        .flatten_ok() // Iter<Result<Vec<Result<Path>>>> => Iter<Result<    Result<Path>>>
        .flatten_ok(); // Iter<Result<    Result<Path>>>  => Iter<Result<           Path>>

    let movie_files = possible_files.filter_ok(has_movie_ext);

    movie_files
        .map_ok(|path| {
            db.lookup_or_update(&db::Key { video_path: path })
                .map(|_| ())
        })
        .flatten_ok()
        .collect_vec()
}

/// Converts arbitrary errors to anyhow.
#[allow(clippy::missing_errors_doc)]
pub fn to_anyhow<T, E>(result: Result<T, E>) -> Result<T>
where
    E: std::error::Error,
{
    match result {
        Ok(ok) => Ok(ok),
        Err(e) => Err(anyhow!("{e}")),
    }
}
