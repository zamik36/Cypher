//! Turns a peer-supplied file name into a safe single path component.

const MAX_NAME_BYTES: usize = 200;
const FALLBACK: &str = "file";

const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Returns a name that cannot traverse directories, address devices or
/// hide itself, on every platform the clients run on.
pub fn sanitize(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let filtered: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        .collect();
    let mut out = filtered
        .trim_end_matches(['.', ' '])
        .trim_start_matches(['.', ' '])
        .to_owned();

    let stem = out.split('.').next().unwrap_or_default();
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        out.insert(0, '_');
    }

    if out.len() > MAX_NAME_BYTES {
        out = truncate_keep_extension(&out);
    }
    if out.is_empty() {
        FALLBACK.to_owned()
    } else {
        out
    }
}

fn truncate_keep_extension(name: &str) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if name.len() - i <= 16 => name.split_at(i),
        _ => (name, ""),
    };
    let budget = MAX_NAME_BYTES - ext.len();
    let kept: String = stem
        .chars()
        .scan(0, |used, c| {
            *used += c.len_utf8();
            (*used <= budget).then_some(c)
        })
        .collect();
    format!("{kept}{ext}")
}

#[cfg(test)]
mod tests {
    use super::sanitize;

    #[test]
    fn strips_directories_and_traversal() {
        assert_eq!(sanitize("../../etc/passwd"), "passwd");
        assert_eq!(sanitize(r"..\..\AppData\Startup\x.bat"), "x.bat");
        assert_eq!(sanitize(r"C:\Windows\system32\evil.dll"), "evil.dll");
        assert_eq!(sanitize("/abs/path.txt"), "path.txt");
        assert_eq!(sanitize(".."), "file");
        assert_eq!(sanitize(""), "file");
        assert_eq!(sanitize("dir/"), "file");
    }

    #[test]
    fn neutralises_windows_specials() {
        assert_eq!(sanitize("CON"), "_CON");
        assert_eq!(sanitize("nul.txt"), "_nul.txt");
        assert_eq!(sanitize("com1.tar.gz"), "_com1.tar.gz");
        assert_eq!(sanitize("a:b*c?.txt"), "abc.txt");
        assert_eq!(sanitize("trailing. . "), "trailing");
        assert_eq!(sanitize(".hidden"), "hidden");
        assert_eq!(sanitize("tab\tname\u{0}.txt"), "tabname.txt");
    }

    #[test]
    fn keeps_unicode_and_bounds_length() {
        assert_eq!(sanitize("отчёт 2026.pdf"), "отчёт 2026.pdf");
        let long = format!("{}.jpeg", "я".repeat(300));
        let out = sanitize(&long);
        assert!(out.len() <= 200);
        assert!(
            std::path::Path::new(&out)
                .extension()
                .is_some_and(|e| e == "jpeg")
        );
    }
}
