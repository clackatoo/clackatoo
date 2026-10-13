//! Installing language servers clackatoo knows where to get, each into a
//! directory of its own in `servers/` beside the language packs, as
//! Neovim's Mason does, and finding a server's program: on `PATH`, or else
//! where clackatoo installed it.
//!
//! A server with a release built for Linux is downloaded and checked
//! against the SHA-256 here, as grammars are. One published to npm is
//! installed with npm into its directory, at the version here, without
//! running any package's install scripts; the packages it depends on
//! aren't pinned, unless a lockfile comes with it, as with agents'
//! adapters, which `npm ci` installs exactly. One that needs a toolchain
//! to build, such as gopls, isn't installed: what to run instead is said.
//!
//! Installs into one directory hold a lock on its `.lock`, so two daemons
//! never install there at once. Each installs beside where it goes, under
//! a name no other install, of this build or another, ever has, and swaps
//! it into place. What an install cut off by an exit or an upgrade left
//! beside is removed by the next install of the same program.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::install::{Download, InstallError, download};

/// How a known server is installed.
#[derive(Debug)]
pub enum Source {
    /// A release asset for each processor it's built for.
    Release {
        /// The release's version.
        version: &'static str,
        /// The assets, by processor, as Rust names them (`x86_64`).
        assets: &'static [(&'static str, Download)],
        /// How the asset is packed.
        packed: Packed,
        /// The program, from the directory it's unpacked into.
        program: &'static str,
    },
    /// npm packages, at these versions.
    Npm {
        /// The packages, as `name@version`.
        packages: &'static [&'static str],
    },
    /// An npm package, and every package it depends on, at exactly the
    /// versions and checksums a lockfile gives.
    NpmLocked {
        /// The package, as `name@version`.
        package: &'static str,
        /// The `package.json` that asks for it.
        manifest: &'static str,
        /// The `package-lock.json` that pins everything.
        lock: &'static str,
    },
    /// Not installed by clackatoo: run this to install it.
    Manual(&'static str),
}

/// How a release asset is packed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Packed {
    /// The program itself.
    Plain,
    /// The program, gzipped.
    Gzip,
    /// A gzipped tar of a directory the program needs all of.
    TarGz,
    /// A zip of a directory the program needs all of.
    Zip,
}

/// A language server clackatoo knows where to get.
#[derive(Debug)]
pub struct KnownServer {
    /// Its program's name, as a language's server command starts.
    pub program: &'static str,
    /// Where it's from.
    pub source: Source,
}

const fn asset(url: &'static str, sha256: &'static str) -> Download {
    Download { url, sha256 }
}

/// Every server clackatoo knows where to get, checked on 2026-10-12.
pub const KNOWN_SERVERS: &[KnownServer] = &[
    KnownServer {
        program: "rust-analyzer",
        source: Source::Release {
            version: "2026-09-28",
            assets: &[
                (
                    "x86_64",
                    asset(
                        "https://github.com/rust-lang/rust-analyzer/releases/download/2026-09-28/rust-analyzer-x86_64-unknown-linux-gnu.gz",
                        "23f711d86b5f826e22886f01d7355dc01e0f4c1357dafa29710a95b903b48c85",
                    ),
                ),
                (
                    "aarch64",
                    asset(
                        "https://github.com/rust-lang/rust-analyzer/releases/download/2026-09-28/rust-analyzer-aarch64-unknown-linux-gnu.gz",
                        "03bad9c3dabb0f07a2678d5f9f8f1575a3742ea141506e14b3a26b42a1f896f3",
                    ),
                ),
            ],
            packed: Packed::Gzip,
            program: "rust-analyzer",
        },
    },
    KnownServer {
        program: "taplo",
        source: Source::Release {
            version: "0.10.0",
            assets: &[
                (
                    "x86_64",
                    asset(
                        "https://github.com/tamasfe/taplo/releases/download/0.10.0/taplo-linux-x86_64.gz",
                        "8fe196b894ccf9072f98d4e1013a180306e17d244830b03986ee5e8eabeb6156",
                    ),
                ),
                (
                    "aarch64",
                    asset(
                        "https://github.com/tamasfe/taplo/releases/download/0.10.0/taplo-linux-aarch64.gz",
                        "033681d01eec8376c3fd38fa3703c79316f5e14bb013d859943b60a07bccdcc3",
                    ),
                ),
            ],
            packed: Packed::Gzip,
            program: "taplo",
        },
    },
    KnownServer {
        program: "lua-language-server",
        source: Source::Release {
            version: "3.19.1",
            assets: &[
                (
                    "x86_64",
                    asset(
                        "https://github.com/LuaLS/lua-language-server/releases/download/3.19.1/lua-language-server-3.19.1-linux-x64.tar.gz",
                        "e9235d2d72ef55bc41cf8c99cda2ed64777682024b4bb81f5dea425060c5cbb8",
                    ),
                ),
                (
                    "aarch64",
                    asset(
                        "https://github.com/LuaLS/lua-language-server/releases/download/3.19.1/lua-language-server-3.19.1-linux-arm64.tar.gz",
                        "abd2572e8fc929dc838a81ffb8473c5bce0bf39bfe8edb4b120b3b623176ce83",
                    ),
                ),
            ],
            packed: Packed::TarGz,
            program: "bin/lua-language-server",
        },
    },
    KnownServer {
        program: "clangd",
        source: Source::Release {
            version: "23.1.0",
            // No build for ARM Linux.
            assets: &[(
                "x86_64",
                asset(
                    "https://github.com/clangd/clangd/releases/download/23.1.0/clangd-linux-23.1.0.zip",
                    "e53b1a96196095faedb7642cf64964f7fb9ad4a0c1f00dd2c172a3d9dcbafdfd",
                ),
            )],
            packed: Packed::Zip,
            program: "clangd_23.1.0/bin/clangd",
        },
    },
    KnownServer {
        program: "pyright-langserver",
        source: Source::Npm {
            packages: &["pyright@1.1.414"],
        },
    },
    KnownServer {
        program: "typescript-language-server",
        source: Source::Npm {
            // TypeScript 7 has no tsserver, which the server needs.
            packages: &["typescript-language-server@6.0.1", "typescript@6.0.3"],
        },
    },
    KnownServer {
        program: "vscode-css-language-server",
        source: Source::Npm {
            packages: &["vscode-langservers-extracted@4.10.0"],
        },
    },
    KnownServer {
        program: "vscode-html-language-server",
        source: Source::Npm {
            packages: &["vscode-langservers-extracted@4.10.0"],
        },
    },
    KnownServer {
        program: "vscode-json-language-server",
        source: Source::Npm {
            packages: &["vscode-langservers-extracted@4.10.0"],
        },
    },
    KnownServer {
        program: "yaml-language-server",
        source: Source::Npm {
            packages: &["yaml-language-server@1.24.0"],
        },
    },
    KnownServer {
        program: "bash-language-server",
        source: Source::Npm {
            packages: &["bash-language-server@5.8.1"],
        },
    },
    KnownServer {
        program: "gopls",
        source: Source::Manual("go install golang.org/x/tools/gopls@v0.23.0"),
    },
];

/// The server whose program is called `program`, if clackatoo knows it.
#[must_use]
pub fn known_server(program: &str) -> Option<&'static KnownServer> {
    KNOWN_SERVERS.iter().find(|known| known.program == program)
}

impl KnownServer {
    /// Whether clackatoo can install it on this machine: it isn't to be
    /// installed by hand, and has a release for this processor, or npm is
    /// here to install it with.
    #[must_use]
    pub fn installable(&self) -> bool {
        match &self.source {
            Source::Release { assets, .. } => assets
                .iter()
                .any(|(arch, _)| *arch == std::env::consts::ARCH),
            Source::Npm { .. } | Source::NpmLocked { .. } => on_path("npm").is_some(),
            Source::Manual(_) => false,
        }
    }

    /// Where it's installed in `servers`, the directory servers are
    /// installed in: its directory.
    fn dir(&self, servers: &Path) -> PathBuf {
        servers.join(self.program)
    }

    /// Its program, installed in `servers`, whether or not it's there.
    #[must_use]
    pub fn installed_program(&self, servers: &Path) -> PathBuf {
        let dir = self.dir(servers);
        match &self.source {
            Source::Release { program, .. } => dir.join(program),
            Source::Npm { .. } | Source::NpmLocked { .. } | Source::Manual(_) => {
                dir.join("node_modules/.bin").join(self.program)
            }
        }
    }

    /// Installs it in `servers`, replacing any there. It appears whole or
    /// not at all: it's installed beside where it goes, then moved there.
    ///
    /// # Errors
    ///
    /// If it can't be installed here, can't be downloaded or unpacked, or
    /// isn't what it should be.
    pub fn install(&self, servers: &Path) -> Result<(), InstallError> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        fs::create_dir_all(servers)?;
        let _locked = lock(servers)?;
        let dir = self.dir(servers);
        // Left by installs cut off before they finished: none is under
        // way, as this one holds the lock.
        let hidden = format!(".{}.", self.program);
        for entry in fs::read_dir(servers)?.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&hidden) {
                log::info!("removing {}, left by an install", entry.path().display());
                let _ = fs::remove_dir_all(entry.path());
            }
        }
        // Hidden, and named for this install alone: when it started, so
        // the build an upgrade starts never takes the last one's name.
        let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let started = (SystemTime::now().duration_since(UNIX_EPOCH)).map_or(0, |at| at.as_nanos());
        let partial = servers.join(format!(
            ".{}.{}.{started}.{next}",
            self.program,
            std::process::id()
        ));
        fs::create_dir(&partial)?;
        let installed = self.install_into(&partial);
        if let Err(err) = installed {
            let _ = fs::remove_dir_all(&partial);
            return Err(err);
        }
        put_in_place(&partial, &dir)?;
        Ok(())
    }

    fn install_into(&self, dir: &Path) -> Result<(), InstallError> {
        match &self.source {
            Source::Release {
                assets,
                packed,
                program,
                ..
            } => {
                let arch = std::env::consts::ARCH;
                let Some((_, file)) = assets.iter().find(|(asset, _)| *asset == arch) else {
                    return Err(InstallError::Unavailable(format!(
                        "{} isn't built for {arch} Linux",
                        self.program
                    )));
                };
                let bytes = download(file)?;
                unpack(&bytes, *packed, dir, program)?;
                let program = dir.join(program);
                make_executable(&program)?;
                Ok(())
            }
            Source::Npm { packages } => {
                let output = Command::new("npm")
                    .args(["install", "--no-audit", "--no-fund", "--ignore-scripts"])
                    .arg("--prefix")
                    .arg(dir)
                    .args(*packages)
                    .output()
                    .map_err(|err| {
                        InstallError::Unavailable(format!("npm couldn't be run: {err}"))
                    })?;
                if output.status.success() {
                    Ok(())
                } else {
                    Err(InstallError::Unavailable(format!(
                        "npm couldn't install {}: {}",
                        packages.join(" "),
                        String::from_utf8_lossy(&output.stderr).trim()
                    )))
                }
            }
            Source::NpmLocked {
                package,
                manifest,
                lock,
            } => {
                fs::write(dir.join("package.json"), manifest)?;
                fs::write(dir.join("package-lock.json"), lock)?;
                let output = Command::new("npm")
                    .args(["ci", "--no-audit", "--no-fund", "--ignore-scripts"])
                    .current_dir(dir)
                    .output()
                    .map_err(|err| {
                        InstallError::Unavailable(format!("npm couldn't be run: {err}"))
                    })?;
                if output.status.success() {
                    Ok(())
                } else {
                    Err(InstallError::Unavailable(format!(
                        "npm couldn't install {package}: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    )))
                }
            }
            Source::Manual(command) => Err(InstallError::Unavailable(format!(
                "{} is installed by hand: {command}",
                self.program
            ))),
        }
    }
}

/// Locks `dir` against installs by other threads and processes, waiting
/// for any under way, until the file returned is dropped.
///
/// # Errors
///
/// If its lock file can't be opened or locked.
pub fn lock(dir: &Path) -> io::Result<File> {
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(".lock"))?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)?;
    Ok(file)
}

/// Puts the directory `new` where `dir` is, in one step: swapped with the
/// one there, which then goes, so `dir` is never missing.
fn put_in_place(new: &Path, dir: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    match renameat_with(CWD, new, CWD, dir, RenameFlags::EXCHANGE) {
        // Swapped: the old one is where the new one was.
        Ok(()) => fs::remove_dir_all(new),
        Err(rustix::io::Errno::NOENT) => fs::rename(new, dir),
        Err(err) => Err(err.into()),
    }
}

/// Unpacks `bytes`, packed as `packed`, into `dir`: a plain or gzipped
/// program as `program`, an archive as it is.
fn unpack(bytes: &[u8], packed: Packed, dir: &Path, program: &str) -> Result<(), InstallError> {
    let archive = dir.join(".download");
    fs::write(&archive, bytes)?;
    let unpacked = match packed {
        Packed::Plain => {
            fs::rename(&archive, dir.join(program))?;
            return Ok(());
        }
        Packed::Gzip => {
            let output = Command::new("gzip").arg("-dc").arg(&archive).output();
            let output = output.map_err(|err| tool("gzip", &err))?;
            if output.status.success() {
                fs::write(dir.join(program), output.stdout)?;
            }
            output.status.success()
        }
        Packed::TarGz => Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(dir)
            .status()
            .map_err(|err| tool("tar", &err))?
            .success(),
        Packed::Zip => Command::new("unzip")
            .args(["-q", "-o"])
            .arg(&archive)
            .arg("-d")
            .arg(dir)
            .status()
            .map_err(|err| tool("unzip", &err))?
            .success(),
    };
    fs::remove_file(&archive)?;
    if unpacked {
        Ok(())
    } else {
        Err(InstallError::Unavailable(
            "the download couldn't be unpacked".into(),
        ))
    }
}

fn tool(name: &str, err: &io::Error) -> InstallError {
    InstallError::Unavailable(format!("{name}, which unpacks it, couldn't be run: {err}"))
}

fn make_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(permissions.mode() | 0o755);
    fs::set_permissions(path, permissions)
}

/// The program called `name` on `PATH`, if it's there.
#[must_use]
pub fn on_path(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| {
            fs::metadata(candidate)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
}

/// The program a server's command names, found: as given if it's a path,
/// or on `PATH`, or else installed by clackatoo in `servers`. `None` if
/// it's nowhere.
#[must_use]
pub fn find_program(program: &str, servers: &Path) -> Option<PathBuf> {
    if program.contains('/') {
        return Some(PathBuf::from(program));
    }
    on_path(program).or_else(|| {
        let installed = known_server(program)?.installed_program(servers);
        installed.is_file().then_some(installed)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_language_server_known_is_findable_by_its_program() {
        for server in KNOWN_SERVERS {
            assert_eq!(
                known_server(server.program).unwrap().program,
                server.program
            );
            if let Source::Release { assets, .. } = &server.source {
                for (_, file) in *assets {
                    assert_eq!(file.sha256.len(), 64, "{}", file.url);
                }
            }
        }
        // Every language's usual server is known, or on purpose not.
        for (language, program) in [
            ("rust", "rust-analyzer"),
            ("go", "gopls"),
            ("tsx", "typescript-language-server"),
        ] {
            let server = crate::Server::of(language, None, None).unwrap();
            assert_eq!(server.command[0], program);
            assert!(known_server(program).is_some());
        }
    }

    #[test]
    fn a_program_is_found_on_path_or_where_clackatoo_installed_it() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            find_program("/opt/ra", dir.path()),
            Some(PathBuf::from("/opt/ra"))
        );
        assert_eq!(
            find_program("sh", dir.path()).map(|found| found.is_file()),
            Some(true)
        );
        assert_eq!(find_program("not-a-program-anyone-has", dir.path()), None);
        // Installed by clackatoo, as an install leaves it.
        let installed = known_server("taplo").unwrap().installed_program(dir.path());
        fs::create_dir_all(installed.parent().unwrap()).unwrap();
        fs::write(&installed, "").unwrap();
        if on_path("taplo").is_none() {
            assert_eq!(find_program("taplo", dir.path()), Some(installed));
        }
    }

    #[test]
    fn a_gzipped_program_is_unpacked_as_itself() {
        let dir = tempfile::tempdir().unwrap();
        let mut gzip = Command::new("gzip")
            .arg("-c")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        {
            use std::io::Write;
            gzip.stdin
                .take()
                .unwrap()
                .write_all(b"#!/bin/sh\n")
                .unwrap();
        }
        let packed = gzip.wait_with_output().unwrap().stdout;
        unpack(&packed, Packed::Gzip, dir.path(), "server").unwrap();
        assert_eq!(fs::read(dir.path().join("server")).unwrap(), b"#!/bin/sh\n");
        assert!(!dir.path().join(".download").exists());
    }

    #[test]
    fn a_new_install_is_swapped_into_place_whole() {
        let dir = tempfile::tempdir().unwrap();
        let (new, old) = (dir.path().join(".new"), dir.path().join("server"));
        fs::create_dir(&new).unwrap();
        fs::write(new.join("v"), "2").unwrap();
        // Nothing there yet.
        put_in_place(&new, &old).unwrap();
        assert_eq!(fs::read_to_string(old.join("v")).unwrap(), "2");
        fs::create_dir(&new).unwrap();
        fs::write(new.join("v"), "3").unwrap();
        put_in_place(&new, &old).unwrap();
        assert_eq!(fs::read_to_string(old.join("v")).unwrap(), "3");
        assert!(!new.exists());
    }

    #[test]
    fn an_install_clears_what_cut_off_ones_left_and_holds_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let server = KnownServer {
            program: "manual",
            source: Source::Manual("make install"),
        };
        let left = dir.path().join(".manual.1234.5.0");
        fs::create_dir(&left).unwrap();
        let other = dir.path().join(".other.1234.5.0");
        fs::create_dir(&other).unwrap();
        // A program installed by hand fails to install, after clearing up.
        assert!(server.install(dir.path()).is_err());
        assert!(!left.exists());
        assert!(other.exists(), "another program's is left for it");

        // Held, the lock keeps another install waiting.
        let held = lock(dir.path()).unwrap();
        let file = fs::File::open(dir.path().join(".lock")).unwrap();
        let try_lock =
            || rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive);
        assert!(try_lock().is_err());
        drop(held);
        assert!(try_lock().is_ok());
    }

    #[test]
    #[ignore = "downloads taplo from GitHub: run with --ignored"]
    fn taplo_installs_and_runs() {
        let dir = tempfile::tempdir().unwrap();
        let taplo = known_server("taplo").unwrap();
        taplo.install(dir.path()).unwrap();
        let program = taplo.installed_program(dir.path());
        let output = Command::new(&program).arg("--version").output().unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("0.10.0"));
    }
}
