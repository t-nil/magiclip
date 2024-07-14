// gerüst from ChatGTFO
pub fn escape_for_unix_filename(input: &str) -> String {
    // TODO use unicode for / and :
    let mut result: String = input
        .chars()
        .map(|c| match c {
            '/' | '*' | '?' | ':' | '|' | '\'' | '"' | '\0' => '_',
            _ => c,
        })
        .enumerate()
        .map(|(i, c)| if i == 0 && c == '-' { '_' } else { c })
        .collect();

    for pat in ["\r\n", "\n", "\r"] {
        result = result.replace(pat, "___");
    }
    result
}
