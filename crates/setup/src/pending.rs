//! Windows' list of file operations left for the next restart (`PendingFileRenameOperations`): pairs of a source
//! and a target, an empty target meaning delete. Uninstalling while a file is in use leaves deletes there, which
//! would remove a newer install at the next restart.

/// The list without the deletes of `dir` and the files in it, or `None` when nothing changes. Files ending in
/// `.old` are left to be deleted: setup moves a loaded camera DLL aside under that name.
pub fn without_deletes_in(entries: &[String], dir: &str) -> Option<Vec<String>> {
    if !entries.len().is_multiple_of(2) {
        return None;
    }
    let dir = dir.trim_end_matches('\\').to_ascii_lowercase();
    let ours = |source: &str| {
        // A source may start with `*` and a digit, then `\??\` and the path.
        let path = source
            .trim_start_matches('*')
            .trim_start_matches(|c: char| c.is_ascii_digit());
        let path = path.strip_prefix(r"\??\").unwrap_or(path).to_ascii_lowercase();
        let inside = path == dir || path.strip_prefix(&dir).is_some_and(|rest| rest.starts_with('\\'));
        inside && !path.ends_with(".old")
    };
    let kept: Vec<String> = entries
        .chunks(2)
        .filter(|pair| !(pair[1].is_empty() && ours(&pair[0])))
        .flatten()
        .cloned()
        .collect();
    (kept.len() != entries.len()).then_some(kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn drops_only_deletes_of_the_install() {
        let entries = list(&[
            r"\??\C:\Windows\a.dll",
            r"\??\C:\Windows\b.dll",
            r"*1\??\C:\Program Files\opal-rs\opal_vcam.3560.old",
            "",
            r"*1\??\C:\Program Files\opal-rs\opal_vcam.dll",
            "",
            r"\??\C:\Program Files\OPAL-RS\opal-rs-setup.exe",
            "",
            r"\??\C:\Program Files\opal-rs-other\x.dll",
            "",
            r"*1\??\C:\Program Files\opal-rs",
            "",
        ]);
        let kept = without_deletes_in(&entries, r"C:\Program Files\opal-rs").unwrap();
        assert_eq!(
            kept,
            list(&[
                r"\??\C:\Windows\a.dll",
                r"\??\C:\Windows\b.dll",
                r"*1\??\C:\Program Files\opal-rs\opal_vcam.3560.old",
                "",
                r"\??\C:\Program Files\opal-rs-other\x.dll",
                "",
            ])
        );
    }

    #[test]
    fn leaves_other_lists_alone() {
        let renames = list(&[r"\??\C:\Program Files\opal-rs\a", r"\??\C:\Program Files\opal-rs\b"]);
        assert_eq!(without_deletes_in(&renames, r"C:\Program Files\opal-rs"), None);
        assert_eq!(without_deletes_in(&list(&["odd"]), r"C:\Program Files\opal-rs"), None);
    }
}
