#![feature(lazy_cell)]
#![feature(exit_status_error)]
#![feature(path_file_prefix)]

use regex::Regex;
use std::cell::LazyCell;
use std::fs;
use std::fs::File;
use std::io::stdin;
use std::path::Path;
use std::sync::LazyLock;

mod clip;
mod fzf;
mod sub;

static REGEX_SUBFILE: &str = r"(.\w{2})?.srt";

fn main() {
    let files = stdin().lines().collect::<Vec<String>>();

    files.iter().map(|f| {
        let p = Path::from(f)
            .file_prefix()
            .expect(format!("no prefix found in {}", f).as_str())
            .;
        let regex = Regex::new(format!("{}{}", regex::escape(p), REGEX_SUBFILE)).unwrap();
        fs::read_dir(p.into())
    });
}
