use std::process::{Command, Stdio};

use anyhow::{Context, Error, Result};

fn select(strings: impl Iterator<Item=String>) -> Result<Vec<String>> {
    let mut fzf = Command::new("fzf")
        .arg("-m") // multi select
        .stdin(Stdio::piped())
        .stdout(Stdio::piped()).spawn().context("trying to spawn fzf")?;

    let mut stdin = fzf.stdin.take().context("trying to open fzf.stdin");
    let input_thread = std::thread::spawn(move || {
        stdin.write_all(strings.collect::<Vec<_>>().join("\n").as_bytes()).expect("Failed to write to stdin");
    });

    input_thread.join().map_err(|e| Error::new(e)).context("trying to write to fzf.stdin")?;
    let output = fzf.wait_with_output().context("trying to wait for fzf")?;

    if !output.status.success() {
        return Err(format!("fzf failed (status {})", output.status.code().map(|c| c.to_string()).unwrap_or(String::from("-"))).into());
    }

    String::from_utf8(output.stdout).into()
}