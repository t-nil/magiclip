use ::serde::{Deserialize, Serialize};
use anyhow::Result;
use srtlib::Subtitles;
use std::collections::HashMap;
use std::path::Path;

mod subdb {

    use anyhow::{anyhow, Context, Result};
    use std::{
        collections::HashMap,
        fs::File,
        io::BufReader,
        os::unix::fs::MetadataExt as _,
        path::{Path, PathBuf},
    };

    use chrono::{DateTime, Utc};
    use serde::{Deserialize, Serialize};

    use super::Subtitle;

    type Subtitles = Vec<Subtitle>;

    #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct Key {
        pub video_path: PathBuf,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    enum SubPath {
        InternalFFmpeg { stream_id: u32 },
        External { path: PathBuf },
    }

    #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
    struct Metadata {
        video_path: PathBuf,
        /// time the entry got indexed, not the vid was modified
        time: DateTime<Utc>,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Entry {
        meta: Metadata,
        sub_files: Vec<(SubPath, Subtitles)>,
    }

    type Val = Entry;
    type InternalDB = HashMap<Key, Val>;

    #[derive(Clone, Default, Debug, Serialize, Deserialize)]
    pub struct SubDB {
        db: InternalDB,
        db_path: Option<PathBuf>,
    }

    impl Key {
        pub fn new(path: impl AsRef<Path>) -> Self {
            Self {
                video_path: path.as_ref().to_owned(),
            }
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum EntryChanged {
        NoLongerExists,
        Yes,
        No,
    }

    impl Entry {
        /// # Important
        /// This compares ctime instead of mtime to detect renames.
        pub fn has_changed(&self) -> Result<EntryChanged> {
            #[allow(clippy::wildcard_imports)]
            use EntryChanged::*;

            // return ctime or mtime, whatever changed more recently
            fn relevant_timestamp(meta: &std::fs::Metadata) -> Result<i64> {
                let ctime = meta.ctime().checked_mul(10i64.pow(9)).and_then(|ts| ts.checked_add(meta.ctime_nsec())).ok_or_else(|| anyhow!("nanos not fitting in i64. either corruption or we have the year ~2540+"))?;
                let mtime = meta.mtime().checked_mul(10i64.pow(9)).and_then(|ts| ts.checked_add(meta.mtime_nsec())).ok_or_else(|| anyhow!("nanos not fitting in i64. either corruption or we have the year ~2540+"))?;
                anyhow!(ctime > 0);
                anyhow!(mtime > 0);
                Ok(Ord::max(ctime, mtime))
            }

            if !self.meta.video_path.exists() || !self.meta.video_path.is_file() {
                return Ok(NoLongerExists);
            }
            let db_scan_nanos = 
            self.meta.time.timestamp_nanos_opt().ok_or_else(|| anyhow!("nsec timestamp not in u64 range. Either corrupt SubDB or more than 580 years have passed.")).
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
    }

    impl SubDB {
        pub fn load(db_path: impl AsRef<Path>) -> Result<Self> {
            let db_path = db_path.as_ref();

            Ok(Self {
                db_path: Some(db_path.to_owned()),
                db: serde_json::from_reader(BufReader::new(File::open(db_path)?))?,
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use anyhow::Result;
        use std::{fs::File, io::Write as _, time::SystemTime};

        use chrono::Utc;
        use tempfile::TempDir;

        use super::{Entry, EntryChanged, Metadata};

        #[test]
        fn test_no_longer_exists() -> Result<()> {
            let temp_dir = TempDir::new()?;
            let video_path = temp_dir.path().join("video.mp4");

            let meta = Metadata {
                video_path: video_path.clone(),
                time: chrono::Utc::now(),
            };
            let entry = Entry {
                meta,
                sub_files: Default::default(),
            };

            assert_eq!(entry.has_changed()?, EntryChanged::NoLongerExists);

            Ok(())
        }

        #[test]
        fn test_no_longer_exists_not_a_file() -> Result<()> {
            let temp_dir = TempDir::new()?;
            let video_path = temp_dir.path();

            let meta = Metadata {
                video_path: video_path.to_path_buf(),
                time: chrono::Utc::now(),
            };
            let entry = Entry {
                meta,
                sub_files: Default::default(),
            };

            assert_eq!(entry.has_changed()?, EntryChanged::NoLongerExists);

            Ok(())
        }

        #[test]
        fn test_has_changed_yes() -> Result<()> {
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
        fn test_has_changed_no() -> Result<()> {
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
struct Subtitle(#[serde(with = "serde::subtitle::Subtitle")] srtlib::Subtitle);

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

            pub fn get(&self) -> (u8, u8, u8, u16) {
                (self.hours, self.minutes, self.seconds, self.milliseconds)
            }

            pub fn get_hours(ts: &srtlib::Timestamp) -> u8 {
                ts.get().0
            }

            pub fn get_minutes(ts: &srtlib::Timestamp) -> u8 {
                ts.get().1
            }

            pub fn get_seconds(ts: &srtlib::Timestamp) -> u8 {
                ts.get().2
            }

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

pub fn parse_from_file(path: impl AsRef<Path>) -> Result<Subtitles> {
    Subtitles::parse_from_file(path, None).map_err(Into::into)
}

fn _index_with_text(subs: Subtitles) -> HashMap<String, Subtitle> {
    /*let mut subs = subs.to_vec();
    let from = subs.drain(..).map(|sub: Subtitle| (sub.text.clone(), sub));

    from.collect::<HashMap<_, _>>()*/
    todo!()
}

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
        let expected = [
            env!("CARGO_MANIFEST_DIR"),
            "test",
            "expected",
            "sub",
            "parse.txt",
        ]
        .iter()
        .collect::<PathBuf>();
        // trim_end() because of unix newline
        assert_eq!(
            format!("{result:#?}"),
            fs::read_to_string(expected).unwrap().trim_end()
        );
    }
}

pub mod old {
    use std::path::Path;

    use srtlib::Subtitle;

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    pub struct SubContainedByFile<'a>(pub Subtitle, pub &'a Path);
}
