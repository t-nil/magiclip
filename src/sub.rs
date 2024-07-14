use ::serde::{Deserialize, Serialize};
use anyhow::{Context, Result};
use itertools::Itertools as _;
use std::path::Path;

use crate::{util, CLIP_FILENAME_PATH_LEN, CLIP_FILENAME_TEXT_LEN};

// TODO check if module scopes are sufficiently granular, if I could encapsulate
// more and if functions interdepend too much / use private apis/structs which
// break invariants.c

pub mod db {

    use anyhow::{anyhow, ensure, Context, Result};
    use derive_getters::Getters;
    use itertools::Itertools;
    use log::{error, warn};
    use rayon::iter::{IntoParallelRefIterator as _, ParallelIterator};
    use serde_with::serde_as;
    use std::{
        collections::HashMap,
        fs::File,
        io::{BufReader, BufWriter},
        os::unix::fs::MetadataExt as _,
        path::{Path, PathBuf},
        sync::Arc,
    };
    use strum::EnumDiscriminants;

    use chrono::{DateTime, Utc};
    use serde::{Deserialize, Serialize};

    use crate::{ffmpeg, to_anyhow};

    use super::Subtitles;

    #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct Key {
        pub video_path: PathBuf,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum SubPath {
        InternalFFmpeg { stream_id: usize },
        External { path: PathBuf },
    }

    #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Getters)]
    pub struct Metadata {
        video_path: PathBuf,
        /// time the entry got indexed, not the vid was modified
        time: DateTime<Utc>,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Getters)]
    pub struct Entry {
        meta: Metadata,
        sub_files: Vec<(SubPath, Subtitles)>,
    }

    // smart pointers b/c of `lookup()`: Somehow borrow checker denies both returning
    // a ref to db (an existing val) and using a mut ref (inserting a new entry)
    // inside the same function
    type Val = Arc<Entry>;
    type InternalDB = HashMap<Key, Val>;

    #[serde_as]
    #[derive(Clone, Default, Debug, Serialize, Deserialize)]
    pub struct SubDB {
        #[serde_as(as = "Vec<(_, _)>")]
        db: InternalDB,
        db_path: PathBuf,
    }

    #[derive(Clone, Debug, Serialize, Deserialize)]
    enum SubDBVersioned {
        #[serde(rename = "0.2")]
        Current(InternalDB),
        #[serde(other)]
        Unsupported,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum EntryChanged {
        Yes,
        No,
        Gone,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
    pub enum EntryFound {
        YesButGone,
        YesButChanged,
        Yes(Val),
        No,
    }

    impl Entry {
        /// # Important
        /// This compares ctime instead of mtime to detect renames.
        pub fn has_changed(&self) -> Result<EntryChanged> {
            #[allow(clippy::enum_glob_use)]
            use EntryChanged::*;

            // return ctime or mtime, whatever changed more recently
            fn relevant_timestamp(meta: &std::fs::Metadata) -> Result<i64> {
                let ctime = meta.ctime().checked_mul(10i64.pow(9)).and_then(|ts| ts.checked_add(meta.ctime_nsec())).ok_or_else(|| anyhow!("nanos not fitting in i64. either corruption or we have the year ~2540+"))?;
                let mtime = meta.mtime().checked_mul(10i64.pow(9)).and_then(|ts| ts.checked_add(meta.mtime_nsec())).ok_or_else(|| anyhow!("nanos not fitting in i64. either corruption or we have the year ~2540+"))?;
                ensure!(ctime > 0);
                ensure!(mtime > 0);
                Ok(Ord::max(ctime, mtime))
            }

            if !self.meta.video_path.exists() || !self.meta.video_path.is_file() {
                return Ok(Gone);
            }
            let db_scan_nanos = self.meta.time.timestamp_nanos_opt().ok_or_else(|| anyhow!("nsec timestamp not in u64 range. Either corrupt SubDB or more than 580 years have passed.")).
            with_context(|| {
                format!(
                    "trying to parse _SubDB_ metadata for file {file:?}",
                    file=self.meta.video_path
                )
            })?;
            let meta = self.meta.video_path.metadata().with_context(|| {
                format!(
                    "trying to access _file_ for file {file:?}",
                    file = self.meta.video_path
                )
            })?;
            // .ctime_nsec() only returns ns part of timestamp
            // also don't panic if out-of-bounds for i64 (although that would
            // be in ca. 500 years)
            let fs_relevant_timestamp = relevant_timestamp(&meta).with_context(|| {
                format!(
                    "calculating nanosec timestamp from filesystem meta (on {file:?})",
                    file = self.meta.video_path
                )
            })?;

            if dbg!(fs_relevant_timestamp) >= dbg!(db_scan_nanos) {
                Ok(Yes)
            } else {
                Ok(No)
            }
        }

        fn from_path(key: &Key) -> Result<(Self, Vec<anyhow::Error>)> {
            let ctx = |what: &str| {
                let what = what.to_owned();
                move || {
                    format!(
                        "{what} sub files from {file}",
                        file = key.video_path.to_string_lossy()
                    )
                }
            };

            let scan_time = Utc::now();
            let temp_dir = tempfile::tempdir()?;

            let subs = ffmpeg::extract_sub_files(&key.video_path, &temp_dir)
                .with_context(ctx("Extracting"))?;
            let subs = subs.iter().enumerate().map(|(stream_id, sub_file)| {
                Ok((
                    SubPath::InternalFFmpeg { stream_id },
                    super::parse_from_file(sub_file).with_context(ctx("Parsing"))?,
                ))
            });

            let (subs, errors): (Vec<_>, Vec<_>) = subs.partition_result();
            Ok((
                Self {
                    meta: Metadata {
                        video_path: key.video_path.clone(),
                        time: scan_time,
                    },
                    sub_files: subs,
                },
                errors,
            ))
        }

        pub fn as_identifying_strings(&self) -> impl Iterator<Item = String> + '_ {
            self.sub_files
                .iter()
                .flat_map(|(_, subs)| subs)
                .map(|sub| sub.as_identifying_string(&self.meta.video_path, Default::default()))
        }
    }

    impl SubDB {
        pub fn load(db_file: impl AsRef<Path>) -> Result<Self> {
            #[allow(clippy::enum_glob_use)]
            use SubDBVersioned::*;

            let db_file = db_file.as_ref();

            let db = if db_file.exists() {
                let db_version_wrapper =
                    serde_json::from_reader(BufReader::new(File::open(db_file)?))?;
                match db_version_wrapper {
                    Current(db) => db,
                    Unsupported => {
                        panic!("Wrong version in DB file detected (0.1 is the only supported)",)
                    }
                }
            } else {
                HashMap::default()
            };

            Ok(Self {
                db_path: db_file.to_owned(),
                db,
            })
        }

        pub fn save(&self) -> Result<()> {
            // TODO clone is probably overkill, but I cannot use a ref in `SubDBVersioned`
            // because then deserializing gets more complicated. ('d have to investigate tho)
            let db_versioned = SubDBVersioned::Current(self.db.clone());
            to_anyhow(serde_json::to_writer_pretty(
                BufWriter::new(File::create(&self.db_path)?),
                &db_versioned,
            ))
        }

        pub fn lookup(&self, key: &Key) -> Result<EntryFound> {
            #[allow(clippy::enum_glob_use)]
            use EntryFound::*;

            let found = if let Some(entry) = self.db.get(key).map(Clone::clone) {
                match entry.has_changed().with_context(|| {
                    format!("determining if entry with key {key:#?} has changed")
                })? {
                    EntryChanged::Yes => YesButChanged,
                    EntryChanged::No => Yes(entry),
                    EntryChanged::Gone => YesButGone,
                }
            } else {
                No
            };

            Ok(found)
        }

        /// Gets the entry from the DB if it exists and is up-to-date (file hasn't
        /// been modified in between). Otherwise create it (from the file).
        pub fn lookup_or_update(&mut self, key: &Key) -> Result<Option<Val>> {
            fn insert(self_: &mut SubDB, key: &Key) -> Result<Val> {
                // passing up errored sub files gets too complicated; bailing out by logging
                let new_entry = Entry::from_path(key).context("creating DB entry from file")?;
                for error in new_entry.1 {
                    warn!("Error parsing subs:\n{error:#}");
                }

                let _ = self_.db.insert(key.clone(), Val::new(new_entry.0));
                Ok(self_.db.get(key).unwrap().clone())
            }
            match self.lookup(key)? {
                EntryFound::YesButGone => {
                    self.db.remove(key);
                    Ok(None)
                }
                EntryFound::Yes(val) => Ok(Some(val)),
                EntryFound::YesButChanged | EntryFound::No => Some(insert(self, key)).transpose(),
            }
        }

        pub fn as_identifying_strings(&self) -> impl ParallelIterator<Item = (&Key, String)> + '_ {
            self.db
                .par_iter()
                .map(|(key, entry)| entry.as_identifying_strings().map(move |id| (key, id)))
                .flatten_iter()
        }

        pub fn len(&self) -> usize {
            self.db.len()
        }
    }

    impl Drop for SubDB {
        fn drop(&mut self) {
            self.save().unwrap_or_else(|e| error!("Saving failed: {e}"));
        }
    }

    #[cfg(test)]
    mod tests {
        #![allow(non_snake_case)]

        use anyhow::Result;
        use std::{fs::File, io::Write as _, time::SystemTime};

        use chrono::Utc;
        use tempfile::TempDir;

        use super::{Entry, EntryChanged, Metadata};

        #[test]
        fn has_changed__no_longer_exists() -> Result<()> {
            let temp_dir = TempDir::new()?;
            let video_path = temp_dir.path().join("video.mp4");

            let meta = Metadata {
                video_path: video_path.clone(),
                time: chrono::Utc::now(),
            };
            let entry = Entry {
                meta,
                sub_files: Vec::default(),
            };

            assert_eq!(entry.has_changed()?, EntryChanged::Gone);

            Ok(())
        }

        #[test]
        fn has_changed__no_longer_exists_not_a_file() -> Result<()> {
            let temp_dir = TempDir::new()?;
            let video_path = temp_dir.path();

            let meta = Metadata {
                video_path: video_path.to_path_buf(),
                time: chrono::Utc::now(),
            };
            let entry = Entry {
                meta,
                sub_files: Vec::default(),
            };

            assert_eq!(entry.has_changed()?, EntryChanged::Gone);

            Ok(())
        }

        #[test]
        fn has_changed__yes() -> Result<()> {
            let temp_dir = TempDir::new()?;
            let video_path = temp_dir.path().join("video.mp4");

            // IMPORTANT: call `sync_all()`/`sync_data()`, otherwise the timestamp is incorrect
            {
                let mut file = File::create(&video_path)?;
                writeln!(file, "Test content")?;
                file.sync_all()?;
            }
            dbg!(video_path.metadata()?.modified()?);
            dbg!(std::fs::read_to_string(&video_path)?);

            // first emulate scan
            let current_time = Utc::now();
            let meta = Metadata {
                video_path: video_path.clone(),
                time: current_time,
            };
            let entry = Entry {
                meta,
                sub_files: Vec::default(),
            };

            // then change file
            {
                let mut file = File::create(&video_path)?;
                writeln!(file, "More test content")?;
                file.set_modified(SystemTime::now())?;
            }
            dbg!(video_path.metadata()?.modified()?);
            dbg!(std::fs::read_to_string(&video_path)?);

            assert_eq!(entry.has_changed()?, EntryChanged::Yes);

            Ok(())
        }

        #[test]
        fn has_changed__no() -> Result<()> {
            let temp_dir = TempDir::new()?;
            let video_path = temp_dir.path().join("video.mp4");

            let mut file = File::create(&video_path)?;
            writeln!(file, "Test content")?;

            let future_time = chrono::Utc::now() + chrono::Duration::days(1);
            let meta = Metadata {
                video_path: video_path.clone(),
                time: future_time,
            };
            let entry = Entry {
                meta,
                sub_files: Vec::default(),
            };

            assert_eq!(entry.has_changed()?, EntryChanged::No);

            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Subtitle(#[serde(with = "serde::subtitle::Subtitle")] srtlib::Subtitle);

type Subtitles = Vec<Subtitle>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SubtitleStringFormatOptions {
    Filename,
    #[default]
    None,
}

impl Subtitle {
    pub fn as_identifying_string(
        &self,
        path: impl AsRef<Path>,
        format_opts: SubtitleStringFormatOptions,
    ) -> String {
        #[allow(clippy::enum_glob_use)]
        use SubtitleStringFormatOptions::*;
        let line_len = if format_opts == Filename {
            CLIP_FILENAME_TEXT_LEN
        } else {
            usize::MAX
        };

        let path_len = if format_opts == Filename {
            CLIP_FILENAME_PATH_LEN
        } else {
            usize::MAX
        };

        util::escape_for_unix_filename(&format!(
            "{line:.line_len$} [{timestamp}] ({path:.path_len$})",
            line = &self.0.text,
            line_len = line_len,
            timestamp = self.0.start_time,
            path = path.as_ref().to_string_lossy(),
            path_len = path_len,
        ))
    }
}

pub fn parse_from_file(path: impl AsRef<Path>) -> Result<Subtitles> {
    // TODO maybe convert non-UTF8 charsets with crates `encoding_rs` and `chardetng`
    let content =
        std::fs::read(&path).with_context(|| String::from(path.as_ref().to_string_lossy()))?;
    let utf8_content = String::from_utf8_lossy(&content);

    Ok(srtlib::Subtitles::parse_from_str(utf8_content.into_owned())
        .map_err(Into::<anyhow::Error>::into)?
        .to_vec() // get underlying vec
        .into_iter()
        .map(Subtitle) // convert to _our_ subtitle type
        .collect_vec())
}

pub(super) mod serde {
    pub(super) mod subtitle {
        use serde::{Deserialize, Serialize};
        use srtlib::Timestamp;

        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(remote = "srtlib::Subtitle")]
        pub struct Subtitle {
            pub num: usize,
            #[serde(with = "super::timestamp::Timestamp")]
            pub start_time: Timestamp,
            #[serde(with = "super::timestamp::Timestamp")]
            pub end_time: Timestamp,
            pub text: String,
        }
    }

    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub(super) mod timestamp {
        use serde::{Deserialize, Serialize};

        #[derive(
            Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Hash, Serialize, Deserialize,
        )]
        #[serde(remote = "srtlib::Timestamp")]
        pub struct Timestamp {
            #[serde(getter = "Timestamp::get_hours")]
            hours: u8,
            #[serde(getter = "Timestamp::get_minutes")]
            minutes: u8,
            #[serde(getter = "Timestamp::get_seconds")]
            seconds: u8,
            #[serde(getter = "Timestamp::get_milliseconds")]
            milliseconds: u16,
        }
        impl Timestamp {
            /// Constructs a new Timestamp from integers.
            pub fn new(hours: u8, minutes: u8, seconds: u8, milliseconds: u16) -> Timestamp {
                Timestamp {
                    hours,
                    minutes,
                    seconds,
                    milliseconds,
                }
            }

            pub fn get(self) -> (u8, u8, u8, u16) {
                (self.hours, self.minutes, self.seconds, self.milliseconds)
            }

            // getter pass by ref is needed by serde

            #[allow(clippy::trivially_copy_pass_by_ref)]
            pub fn get_hours(ts: &srtlib::Timestamp) -> u8 {
                ts.get().0
            }

            #[allow(clippy::trivially_copy_pass_by_ref)]
            pub fn get_minutes(ts: &srtlib::Timestamp) -> u8 {
                ts.get().1
            }

            #[allow(clippy::trivially_copy_pass_by_ref)]
            pub fn get_seconds(ts: &srtlib::Timestamp) -> u8 {
                ts.get().2
            }

            #[allow(clippy::trivially_copy_pass_by_ref)]
            pub fn get_milliseconds(ts: &srtlib::Timestamp) -> u16 {
                ts.get().3
            }
        }

        impl From<Timestamp> for srtlib::Timestamp {
            fn from(value: Timestamp) -> Self {
                let (h, m, s, ms) = value.get();
                Self::new(h, m, s, ms)
            }
        }
    }

    //pub(crate) mod subtitle {

    // manually implementing serde for structs seems to be horribly hard
    // luckily I found #[serde(remote)]
    //
    //         use serde::{ser::SerializeStruct as _, Serializer};
    //         use srtlib::Subtitle;
    //         use std::iter::once;

    //         pub fn serialize<S>(sub: &Subtitle, serde: S) -> Result<S::Ok, S::Error>
    //         where
    //             S: Serializer,
    //         {
    //             //let iter = once(("num", ))
    //             let mut out = serde.serialize_struct("Subtitle", 4)?;
    //             out.serialize_field("num", &sub.num)?;
    //             out.serialize_field("start_time", &sub.start_time)?;
    //             out.serialize_field("end_time", &sub.end_time)?;
    //             out.serialize_field("text", &sub.text)?;
    //             out.end()
    //         }
    //     }

    //     pub(crate) mod timestamp {
    //         use serde::{ser::SerializeStruct as _, Deserializer, Serializer};
    //         use srtlib::Timestamp;

    //         pub fn serialize<S>(ts: &Timestamp, serde: S) -> Result<S::Ok, S::Error>
    //         where
    //             S: Serializer,
    //         {
    //             //let iter = once(("num", ))
    //             let mut out = serde.serialize_struct("Timestamp", 4)?;
    //             let (hours, minutes, seconds, milliseconds) = &ts.get();
    //             out.serialize_field("hours", hours)?;
    //             out.serialize_field("minutes", minutes)?;
    //             out.serialize_field("seconds", seconds)?;
    //             out.serialize_field("milliseconds", milliseconds)?;
    //             out.end()
    //         }

    //         pub fn deserialize<'de, D>(serde: D) -> Result<Timestamp, D::Error>
    //         where
    //             D: Deserializer<'de>,
    //         {
    //             let in_ = serde.deserialize_struct("Timestamp", &["hours", "minutes", "seconds", "milliseconds"], visitor)
    //             let hours =
    //             Timestamp::new(hours, minutes, seconds, milliseconds)
    //         }
    //}
}

#[cfg(test)]
mod test {

    use std::path::PathBuf;
    use std::sync::LazyLock;

    static TEST_SUB: LazyLock<PathBuf> = LazyLock::new(|| {
        [env!("CARGO_MANIFEST_DIR"), "test", "gem_glow.srt"]
            .iter()
            .collect::<PathBuf>()
    });

    #[test]
    fn parse() {
        let result = super::parse_from_file(TEST_SUB.as_path()).unwrap();
        insta::assert_debug_snapshot!(result);
    }
}

pub mod old {
    use std::{ops::Deref, path::Path};

    use super::Subtitle;

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    pub struct SubContainedByFile<'a>(pub Subtitle, pub &'a Path);

    impl Deref for Subtitle {
        type Target = srtlib::Subtitle;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }
}
