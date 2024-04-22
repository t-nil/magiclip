use std::{
    io::{BufRead, Write as _},
    process::{Command, Stdio},
};

use anyhow::{anyhow, Context, Error, Result};
use itertools::Itertools as _;

pub fn select(strings: &Vec<impl AsRef<str>>) -> Result<Vec<String>> {
    let mut fzf = Command::new("fzf")
        .arg("-m") // multi select
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("trying to spawn fzf")?;

    let stdin = fzf.stdin.take().context("trying to open fzf.stdin");
    let thread_strings = strings.iter().map(|s| s.as_ref().to_owned()).collect_vec();
    let input_thread = std::thread::spawn(move || {
        stdin
            .expect("Could not open stdin")
            .write_all(thread_strings.join("\n").as_bytes())
            .expect("Failed to write to stdin");
    });

    input_thread
        .join()
        .map_err(|e| anyhow!("{:?}", e))
        .context("trying to write to fzf.stdin")?;
    let output = fzf.wait_with_output().context("trying to wait for fzf")?;

    if !output.status.success() {
        return Err(anyhow!(
            "fzf failed (status {})",
            output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or(String::from("-"))
        ));
    }

    output
        .stdout
        .lines()
        .map(|r| match r {
            Ok(s) => Ok(s),
            Err(e) => Err(anyhow::Error::from(e)),
        })
        .collect()
}
