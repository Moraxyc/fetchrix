//! Tarball unpacking with `fetchzip` semantics.

use std::{
    fs::File,
    io::{BufReader, Read, Seek},
    path::{Path, PathBuf},
};

use super::{FetchError, io};

/// An unpacked archive.
pub struct Unpacked {
    /// The single top-level directory.
    pub root: PathBuf,
    /// The commit `git archive` records in the pax global header.
    pub commit: Option<String>,
    /// The UTC date (`YYYY-MM-DD`) of the first entry, set only with `commit`
    /// because `git archive` stamps every entry with the commit time.
    pub date: Option<String>,
}

/// Unpacks `archive` into `out`.
pub fn unpack(archive: &Path, out: &Path) -> Result<Unpacked, FetchError> {
    let mut file = File::open(archive).map_err(io("cannot open the downloaded archive"))?;
    let mut magic = [0; 6];
    let read = file
        .read(&mut magic)
        .map_err(io("cannot read the downloaded archive"))?;
    file.rewind()
        .map_err(io("cannot read the downloaded archive"))?;
    let file = BufReader::new(file);
    let reader: Box<dyn Read> = match &magic[..read] {
        [0x1f, 0x8b, ..] => Box::new(flate2::bufread::MultiGzDecoder::new(file)),
        [0xfd, b'7', b'z', b'X', b'Z', 0] => Box::new(lzma_rust2::XzReader::new(file, true)),
        [b'B', b'Z', b'h', ..] => Box::new(bzip2::bufread::MultiBzDecoder::new(file)),
        [0x28, 0xb5, 0x2f, 0xfd, ..] => Box::new(
            ruzstd::decoding::StreamingDecoder::new(file)
                .map_err(|e| FetchError::Archive(format!("invalid zstd stream: {e}")))?,
        ),
        _ => Box::new(file),
    };
    let mut tar = tar::Archive::new(reader);
    tar.set_preserve_permissions(true);
    let (commit, mtime) = unpack_entries(&mut tar, out).map_err(io(
        "cannot unpack the archive; supported formats are tar with gzip, xz, bzip2, or zstd",
    ))?;
    let date = commit.as_ref().and(mtime).map(utc_date);

    let mut entries = std::fs::read_dir(out)
        .map_err(io("cannot read the unpacked archive"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(io("cannot read the unpacked archive"))?;
    match entries.pop() {
        Some(entry) if entries.is_empty() && entry.file_type().is_ok_and(|t| t.is_dir()) => {
            Ok(Unpacked {
                root: entry.path(),
                commit,
                date,
            })
        }
        _ => Err(FetchError::Archive(
            "archive must contain a single top-level directory".into(),
        )),
    }
}

/// Unpacks like `tar::Archive::unpack`, returning the pax global `comment`
/// and the mtime of the first entry.
fn unpack_entries<R: Read>(
    tar: &mut tar::Archive<R>,
    out: &Path,
) -> std::io::Result<(Option<String>, Option<u64>)> {
    std::fs::create_dir_all(out)?;
    let mut commit = None;
    let mut mtime = None;
    // Directories go last so their permissions do not block their contents.
    let mut directories = Vec::new();
    for entry in tar.entries()? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() {
            if let Some(extensions) = entry.pax_extensions()? {
                for extension in extensions.flatten() {
                    if extension.key() == Ok("comment") {
                        commit = extension.value().ok().map(str::to_owned);
                    }
                }
            }
            continue;
        }
        if mtime.is_none() {
            mtime = entry.header().mtime().ok();
        }
        if kind == tar::EntryType::Directory {
            directories.push(entry);
        } else {
            entry.unpack_in(out)?;
        }
    }
    directories.sort_by(|a, b| b.path_bytes().cmp(&a.path_bytes()));
    for mut dir in directories {
        dir.unpack_in(out)?;
    }
    Ok((commit, mtime))
}

/// Formats Unix time `secs` as a UTC `YYYY-MM-DD` date.
fn utc_date(secs: u64) -> String {
    // Howard Hinnant's `civil_from_days`, shifted so eras start in March.
    let days = secs / 86_400 + 719_468;
    let era = days / 146_097;
    let doe = days % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = era * 400 + yoe + u64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_utc_dates() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(951_782_400), "2000-02-29");
        assert_eq!(utc_date(1_735_689_599), "2024-12-31");
    }

    #[test]
    fn reads_git_archive_commit_and_date() {
        let archive =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src/fetch/fixtures/git-archive.tar.gz");
        let out = tempfile::tempdir().unwrap();
        let unpacked = unpack(&archive, out.path()).unwrap();
        assert_eq!(
            unpacked.commit.as_deref(),
            Some("396f24dc015d8b436146fca2950416e37a60581c")
        );
        assert_eq!(unpacked.date.as_deref(), Some("2024-12-31"));
    }

    #[test]
    fn plain_tarball_has_no_commit_or_date() {
        let archive =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src/fetch/fixtures/sample.tar.gz");
        let out = tempfile::tempdir().unwrap();
        let unpacked = unpack(&archive, out.path()).unwrap();
        assert_eq!((unpacked.commit, unpacked.date), (None, None));
    }

    /// `nix hash path` of the directory inside every `fixtures/sample.tar.*`.
    const SAMPLE: &str = "sha256-YOfEjnDTXtQTPwBh/5K+yBGqcQGBNHSs8lSqesPd/S8=";

    #[test]
    fn matches_nix_for_every_compression() {
        for ext in ["gz", "xz", "bz2", "zst"] {
            let archive = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("src/fetch/fixtures/sample.tar.{ext}"));
            let out = tempfile::tempdir().unwrap();
            let root = unpack(&archive, out.path()).unwrap().root;
            assert_eq!(
                super::super::nar::hash(&root).unwrap().to_sri_string(),
                SAMPLE,
                "{ext}"
            );
        }
    }
}
