use std::collections::HashMap;
use std::path::Path;
use srtlib::{Subtitle, Subtitles};
use anyhow::Result;

fn parse(path: impl AsRef<Path>) -> Result<Subtitles> {
    Subtitles::parse_from_file(path, None).map_err(|e| e.into())
}

fn index_with_text(subs: Subtitles) -> HashMap<String, Subtitle> {
    let mut subs = subs.to_vec();
    let from = subs.drain(..).map(|sub: Subtitle|{
        (sub.text.clone(), sub)
    });

    from.collect::<HashMap<_, _>>()
}



mod test {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::LazyLock;

    static TEST_SUB: LazyLock<PathBuf> = LazyLock::new(|| {[env!("CARGO_MANIFEST_DIR"), "test", "gem_glow.srt"].iter().collect::<PathBuf>()});

    #[test]
    fn parse() {
        let result = super::parse(TEST_SUB.as_path()).unwrap();
        let expected = [env!("CARGO_MANIFEST_DIR"), "test", "expected", "sub", "parse.txt"]
            .iter()
            .collect::<PathBuf>();
        // trim_end() because of unix newline
        assert_eq!(format!("{:#?}", result), fs::read_to_string(expected).unwrap().trim_end());
    }

    #[test]
    fn index_with_text() {
        let subs = super::parse(TEST_SUB.as_path()).unwrap();
        let result = super::index_with_text(subs);
        let expected = [env!("CARGO_MANIFEST_DIR"), "test", "expected", "sub", "index_with_text.txt"]
            .iter()
            .collect::<PathBuf>();

        assert_eq!(format!("{:#?}", dbg!(result)), fs::read_to_string(dbg!(expected)).unwrap().trim_end());
    }
}