//! NAR serialization of a directory tree.

use std::{
    fs::{self, File},
    io::{self, BufReader, Write},
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::Path,
};

use nix_compat::{
    nar::writer::{self, Node},
    nixhash::{NixHash, Sha256Digester},
};

/// Computes the SHA-256 NAR hash of `path`, as `nix hash path` does.
pub fn hash(path: &Path) -> io::Result<NixHash> {
    let mut digester = Sha256Digester::new();
    write(writer::open(&mut digester)?, path)?;
    Ok(digester.finalize().into())
}

fn write<W: Write>(node: Node<'_, W>, path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    let kind = meta.file_type();
    if kind.is_symlink() {
        node.symlink(fs::read_link(path)?.as_os_str().as_bytes())
    } else if kind.is_dir() {
        let mut names = fs::read_dir(path)?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<io::Result<Vec<_>>>()?;
        // NAR requires entries in byte order, which is how `OsString` sorts on Unix.
        names.sort();
        let mut dir = node.directory()?;
        for name in names {
            write(dir.entry(name.as_bytes())?, &path.join(&name))?;
        }
        dir.close()
    } else {
        let executable = meta.permissions().mode() & 0o100 != 0;
        node.file(
            executable,
            meta.len(),
            &mut BufReader::new(File::open(path)?),
        )
    }
}
