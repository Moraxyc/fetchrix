//! The nixpkgs that `nix` commands take tools from, set by `[nix].nixpkgs`.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

#[derive(Debug, PartialEq, Eq)]
pub enum Nixpkgs {
    /// A flake reference, resolved against the inputs of the flake that
    /// contains the configuration, if any.
    Flake {
        reference: String,
        inputs_from: Option<PathBuf>,
    },
    /// A Nix file, directory, or lookup path such as `<nixpkgs>`.
    File(OsString),
}

impl Nixpkgs {
    /// Interprets `value`, `nixpkgs` when absent; paths are relative to
    /// `config_dir`.
    #[must_use]
    pub fn new(value: Option<&str>, config_dir: &Path) -> Self {
        match value.unwrap_or("nixpkgs") {
            v if v.starts_with('<') && v.ends_with('>') => Self::File(v.into()),
            v if v.starts_with('/') || v.starts_with("./") || v.starts_with("../") => {
                Self::File(config_dir.join(v).into_os_string())
            }
            v => Self::Flake {
                reference: v.to_owned(),
                inputs_from: flake_root(config_dir),
            },
        }
    }

    /// Arguments that select package `attr` for `nix run` or `nix shell`.
    #[must_use]
    pub fn installable(&self, attr: &str) -> Vec<OsString> {
        match self {
            Self::Flake {
                reference,
                inputs_from,
            } => {
                let mut args = Vec::new();
                if let Some(dir) = inputs_from {
                    args.extend(["--inputs-from".into(), dir.clone().into_os_string()]);
                }
                args.push(format!("{reference}#{attr}").into());
                args
            }
            Self::File(file) => vec!["--file".into(), file.clone(), attr.into()],
        }
    }
}

/// The flake containing `dir`, searched as Nix does: upwards until a
/// `flake.nix`, stopping at the root of a Git repository.
fn flake_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .find(|d| d.join("flake.nix").exists() || d.join(".git").exists())
        .filter(|d| d.join("flake.nix").exists())
        .map(Path::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_enclosing_flake() {
        let root = tempfile::tempdir().unwrap();
        let sub = root.path().join("pkgs/foo");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(root.path().join("flake.nix"), "{}").unwrap();
        assert_eq!(
            Nixpkgs::new(None, &sub).installable("go"),
            [
                "--inputs-from".into(),
                root.path().as_os_str().to_owned(),
                "nixpkgs#go".into()
            ]
        );
    }

    #[test]
    fn stops_at_git_root() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(root.path().join("flake.nix"), "{}").unwrap();
        assert_eq!(
            Nixpkgs::new(Some("github:NixOS/nixpkgs"), &repo).installable("go"),
            [OsString::from("github:NixOS/nixpkgs#go")]
        );
    }

    #[test]
    fn evaluates_paths_as_files() {
        let dir = Path::new("/etc/conf");
        assert_eq!(
            Nixpkgs::new(Some("<nixpkgs>"), dir).installable("go"),
            [OsString::from("--file"), "<nixpkgs>".into(), "go".into()]
        );
        assert_eq!(
            Nixpkgs::new(Some("./npins/nixpkgs.nix"), dir),
            Nixpkgs::File("/etc/conf/./npins/nixpkgs.nix".into())
        );
        assert_eq!(
            Nixpkgs::new(Some("/srv/nixpkgs"), dir),
            Nixpkgs::File("/srv/nixpkgs".into())
        );
    }
}
