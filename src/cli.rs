use std::{
    ffi::{OsStr, OsString},
    path::PathBuf,
};

use clap::Parser;
use once_cell::sync::Lazy;

use crate::ffmpeg::EncodingProfile;

static DB_FILE: Lazy<OsString> = Lazy::new(|| {
    std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned()
        .join("db.json")
        .into_os_string()
});

fn db_file() -> &'static OsStr {
    &DB_FILE
}

#[derive(Clone, Debug, PartialEq, Eq, Parser)]
#[command(version, about, long_about = None)]
pub struct Args {
    #[arg(short, long, default_value = db_file())]
    pub db_file: PathBuf,

    #[arg(short, long)]
    pub clip_dir: PathBuf,

    #[arg(long, default_value_t = false)]
    pub subdir_per_profile: bool,

    #[arg(short, long, default_value = "av1")]
    pub profile: EncodingProfile,

    /// Paths to video folders (or files) which get scanned recursively and added to the DB.
    #[arg()]
    pub paths: Vec<PathBuf>,
}
