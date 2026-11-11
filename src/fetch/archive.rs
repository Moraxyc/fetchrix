//! Tarball unpacking with `fetchzip` semantics.

use std::{
    fs::File,
    io::{BufReader, Read, Seek},
    path::{Path, PathBuf},
};

use super::{FetchError, io};

/// Unpacks `archive` into `out` and returns its single top-level directory.
pub fn unpack(archive: &Path, out: &Path) -> Result<PathBuf, FetchError> {
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
    tar.unpack(out).map_err(io(
        "cannot unpack the archive; supported formats are tar with gzip, xz, bzip2, or zstd",
    ))?;

    let mut entries = std::fs::read_dir(out)
        .map_err(io("cannot read the unpacked archive"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(io("cannot read the unpacked archive"))?;
    match entries.pop() {
        Some(entry) if entries.is_empty() && entry.file_type().is_ok_and(|t| t.is_dir()) => {
            Ok(entry.path())
        }
        _ => Err(FetchError::Archive(
            "archive must contain a single top-level directory".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `nix hash path` of the directory inside every `fixtures/sample.tar.*`.
    const SAMPLE: &str = "sha256-YOfEjnDTXtQTPwBh/5K+yBGqcQGBNHSs8lSqesPd/S8=";

    #[test]
    fn matches_nix_for_every_compression() {
        for ext in ["gz", "xz", "bz2", "zst"] {
            let archive = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("src/fetch/fixtures/sample.tar.{ext}"));
            let out = tempfile::tempdir().unwrap();
            let root = unpack(&archive, out.path()).unwrap();
            assert_eq!(
                super::super::nar::hash(&root).unwrap().to_sri_string(),
                SAMPLE,
                "{ext}"
            );
        }
    }
}
