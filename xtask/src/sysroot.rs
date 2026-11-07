//! `cargo xtask sysroot`: downloads the pinned linker and C runtime pieces
//! that toolchains bundle (§14.14, §19.2) into `target/sysroot`, laid out as
//! the toolchain's `lib/`, so development builds link as releases do.
//!
//! - `ld/bin/`: `lld` as `ld.lld` and `lld-link`, the names it reads to pick
//!   its flavor, with the `libLLVM` it loads in `ld/lib/` (beside it in
//!   `ld/bin/` on Windows). On Unix both are symlinks to the `lld` file. From
//!   llvm-mingw, built for the host.
//! - `sysroot/x86_64-windows/`, `sysroot/aarch64-windows/`: the MinGW-w64
//!   start files and libraries and the compiler-rt builtins, from the same
//!   llvm-mingw archive.
//! - `sysroot/x86_64-linux/`, `sysroot/aarch64-linux/`: the musl start files,
//!   `libc.a` and `libunwind.a` that Rust's `rust-std` ships for the musl
//!   targets, from the toolchain pinned in `rust-toolchain.toml`.
//! - `LICENSES/`: the repository's `LICENSES/`, which covers every piece,
//!   and Mora's own `LICENSE-MIT` and `LICENSE-APACHE`.
//!
//! Every archive is checked against its SHA-256 before anything is unpacked.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

/// A pinned download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Archive {
    /// The file name, also the name of the directory it unpacks into, less
    /// its extension.
    pub file: String,
    /// Where it is downloaded from.
    pub url: String,
    /// Its SHA-256, in lowercase hex.
    pub sha256: String,
}

/// The pinned llvm-mingw release.
const LLVM_MINGW: &str = "20261006";

/// The llvm-mingw archive for one host.
struct HostBuild {
    /// The host's architecture, as `std::env::consts::ARCH` names it.
    arch: &'static str,
    /// The host's OS, as `std::env::consts::OS` names it.
    os: &'static str,
    /// The end of the archive's file name.
    suffix: &'static str,
    /// The archive's SHA-256.
    sha256: &'static str,
}

/// The llvm-mingw archive of each host.
const LLVM_MINGW_HOSTS: &[HostBuild] = &[
    HostBuild {
        arch: "x86_64",
        os: "linux",
        suffix: "ubuntu-22.04-x86_64.tar.xz",
        sha256: "5f9c6ed95b2d4bdb2869a488c5fd5857fbdabcf288a0aa3eb1da43f6a08d8ab4",
    },
    HostBuild {
        arch: "aarch64",
        os: "linux",
        suffix: "ubuntu-22.04-aarch64.tar.xz",
        sha256: "e77c4acfa44184f6b0c52b814d724ad63b2ecebb3c3f6f842c0846e717cbd23c",
    },
    HostBuild {
        arch: "x86_64",
        os: "macos",
        suffix: "macos-universal.tar.xz",
        sha256: "a7a19b11048e89372637d26279cdb7c430ed3cf8c1e224310421312bf986ebf6",
    },
    HostBuild {
        arch: "aarch64",
        os: "macos",
        suffix: "macos-universal.tar.xz",
        sha256: "a7a19b11048e89372637d26279cdb7c430ed3cf8c1e224310421312bf986ebf6",
    },
    HostBuild {
        arch: "x86_64",
        os: "windows",
        suffix: "x86_64.zip",
        sha256: "317492c456aa27ee607a5919f1d2d38dcdc1112516a24d0bf4b00d078f52d17a",
    },
    HostBuild {
        arch: "aarch64",
        os: "windows",
        suffix: "aarch64.zip",
        sha256: "9a835d5179c9f3a5a783c11a7f14062a249b4765dd11add64a66786864e82ab2",
    },
];

/// The date of the pinned Rust toolchain's release, which names its
/// download directory.
const RUST_STD_DATE: &str = "2026-10-15";

/// The version of the pinned Rust toolchain.
const RUST_VERSION: &str = "1.99.1";

/// A musl target whose pieces come from the pinned toolchain's `rust-std`.
struct MuslTarget {
    /// The Rust target.
    target: &'static str,
    /// The directory under `sysroot/`.
    dir: &'static str,
    /// The SHA-256 of its `rust-std` archive.
    sha256: &'static str,
}

/// The musl targets.
const RUST_STD_MUSL: &[MuslTarget] = &[
    MuslTarget {
        target: "x86_64-unknown-linux-musl",
        dir: "x86_64-linux",
        sha256: "89703fc52323fad35169fcf649e56844b7dd62bf9dd9a4113bbcd96865690e36",
    },
    MuslTarget {
        target: "aarch64-unknown-linux-musl",
        dir: "aarch64-linux",
        sha256: "5202b22bf78e62f95304f6dfafd7e3e10ac839fb808e01cd12d66bbb99e7c1b8",
    },
];

/// The MinGW-w64 targets: llvm-mingw's architecture and the sysroot
/// directory.
const MINGW_TARGETS: &[(&str, &str)] =
    &[("x86_64", "x86_64-windows"), ("aarch64", "aarch64-windows")];

/// MinGW-w64's profiling code is under the GPL (`COPYING.MinGW-w64.txt`) and
/// is never linked by Mora, so it is left out.
const MINGW_EXCLUDED: &[&str] = &["gcrt0.o", "gcrt1.o", "gcrt2.o", "libgmon.a"];

/// The llvm-mingw archive for a host.
pub fn llvm_mingw(arch: &str, os: &str) -> Result<Archive, String> {
    let host = LLVM_MINGW_HOSTS
        .iter()
        .find(|host| host.arch == arch && host.os == os)
        .ok_or_else(|| format!("no llvm-mingw build is pinned for {arch} {os}"))?;
    let file = format!("llvm-mingw-{LLVM_MINGW}-ucrt-{}", host.suffix);
    Ok(Archive {
        url: format!(
            "https://github.com/mstorsjo/llvm-mingw/releases/download/{LLVM_MINGW}/{file}"
        ),
        file,
        sha256: host.sha256.to_owned(),
    })
}

/// The `rust-std` archive for a musl target.
fn rust_std(target: &str, sha256: &str) -> Archive {
    let file = format!("rust-std-{RUST_VERSION}-{target}.tar.gz");
    Archive {
        url: format!("https://static.rust-lang.org/dist/{RUST_STD_DATE}/{file}"),
        file,
        sha256: sha256.to_owned(),
    }
}

/// Every archive the host needs.
pub fn archives(arch: &str, os: &str) -> Result<Vec<Archive>, String> {
    let mut archives = vec![llvm_mingw(arch, os)?];
    archives.extend(
        RUST_STD_MUSL
            .iter()
            .map(|musl| rust_std(musl.target, musl.sha256)),
    );
    Ok(archives)
}

/// The SHA-256 of a file, in lowercase hex.
pub fn sha256(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Downloads `archive` into `dir` with `curl`, or reuses an earlier
/// download, and returns its path. A file whose SHA-256 differs from the
/// pinned one is deleted and refused.
pub fn fetch(archive: &Archive, dir: &Path) -> Result<PathBuf, String> {
    let path = dir.join(&archive.file);
    if path.is_file() && sha256(&path).is_ok_and(|hash| hash == archive.sha256) {
        return Ok(path);
    }
    fs::create_dir_all(dir).map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    let partial = dir.join(format!("{}.part", archive.file));
    let status = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--retry",
            "3",
        ])
        .arg("--output")
        .arg(&partial)
        .arg(&archive.url)
        .status()
        .map_err(|err| format!("cannot run curl: {err}"))?;
    if !status.success() {
        let _ = fs::remove_file(&partial);
        return Err(format!("downloading {} failed", archive.url));
    }
    let actual =
        sha256(&partial).map_err(|err| format!("cannot hash {}: {err}", partial.display()))?;
    if actual != archive.sha256 {
        let _ = fs::remove_file(&partial);
        return Err(format!(
            "{}: SHA-256 is {actual}, expected {}; refusing it",
            archive.file, archive.sha256
        ));
    }
    fs::rename(&partial, &path)
        .map_err(|err| format!("cannot move {}: {err}", partial.display()))?;
    Ok(path)
}

/// Unpacks a `.tar.gz`, `.tar.xz` or `.zip` archive into `dir` and returns
/// the single directory at its top.
fn unpack(archive: &Path, dir: &Path) -> Result<PathBuf, String> {
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir).map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    let status = Command::new(tar())
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dir)
        .status()
        .map_err(|err| format!("cannot run tar: {err}"))?;
    if !status.success() {
        return Err(format!("unpacking {} failed", archive.display()));
    }
    let top: Vec<PathBuf> = fs::read_dir(dir)
        .and_then(|entries| entries.map(|entry| entry.map(|e| e.path())).collect())
        .map_err(|err| format!("cannot read {}: {err}", dir.display()))?;
    match &top[..] {
        [only] if only.is_dir() => Ok(only.clone()),
        _ => Err(format!("{} does not hold one directory", archive.display())),
    }
}

/// The `tar` that reads every archive here. On Windows that is the system's
/// bsdtar, which also reads zip; a GNU tar earlier on `PATH` would not.
fn tar() -> PathBuf {
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        Path::new(&root).join("System32").join("tar.exe")
    } else {
        PathBuf::from("tar")
    }
}

/// Copies the regular files directly in `from` that `keep` accepts into `to`.
fn copy_files(from: &Path, to: &Path, keep: impl Fn(&str) -> bool) -> Result<usize, String> {
    let fail = |err: io::Error| format!("cannot copy from {}: {err}", from.display());
    fs::create_dir_all(to).map_err(fail)?;
    let mut copied = 0;
    for entry in fs::read_dir(from).map_err(fail)? {
        let entry = entry.map_err(fail)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        // `file_type` does not follow symlinks, so links are skipped.
        if entry.file_type().map_err(fail)?.is_file() && keep(&name) {
            fs::copy(entry.path(), to.join(&name)).map_err(fail)?;
            copied += 1;
        }
    }
    Ok(copied)
}

/// Installs `lld` and the MinGW-w64 pieces from an unpacked llvm-mingw.
fn install_llvm_mingw(root: &Path, out: &Path) -> Result<(), String> {
    let bin = out.join("ld/bin");
    if cfg!(windows) {
        // lld and the DLLs it loads, which Windows finds beside it.
        let dlls = ["libLLVM-", "libc++.dll", "libunwind.dll"];
        copy_files(&root.join("bin"), &bin, |name| {
            name == "ld.lld.exe" || dlls.iter().any(|dll| name.starts_with(dll))
        })?;
        fs::copy(bin.join("ld.lld.exe"), bin.join("lld-link.exe"))
            .map_err(|err| format!("cannot copy ld.lld.exe: {err}"))?;
    } else {
        copy_files(&root.join("bin"), &bin, |name| name == "lld")?;
        // lld finds it through `$ORIGIN/../lib` or `@loader_path/../lib`.
        copy_files(&root.join("lib"), &out.join("ld/lib"), |name| {
            name.starts_with("libLLVM")
        })?;
        link_lld(&bin)?;
    }
    for (arch, target) in MINGW_TARGETS {
        let dir = out.join("sysroot").join(target);
        let lib = root.join(format!("{arch}-w64-mingw32/lib"));
        copy_files(&lib, &dir, |name| {
            !MINGW_EXCLUDED.contains(&name) && !name.ends_with(".la")
        })?;
        let builtins = format!("libclang_rt.builtins-{arch}.a");
        let clang = root.join("lib/clang");
        let versions = fs::read_dir(&clang)
            .map_err(|err| format!("cannot read {}: {err}", clang.display()))?;
        let mut found = 0;
        for version in versions.flatten() {
            found += copy_files(&version.path().join("lib/windows"), &dir, |name| {
                name == builtins
            })
            .unwrap_or(0);
        }
        if found != 1 {
            return Err(format!(
                "expected one {builtins} in {}, found {found}",
                clang.display()
            ));
        }
    }
    Ok(())
}

/// `ld.lld` and `lld-link` are the names lld reads to pick its flavor.
#[cfg(unix)]
fn link_lld(bin: &Path) -> Result<(), String> {
    for name in ["ld.lld", "lld-link"] {
        std::os::unix::fs::symlink("lld", bin.join(name))
            .map_err(|err| format!("cannot link {name}: {err}"))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn link_lld(_: &Path) -> Result<(), String> {
    Ok(())
}

/// Installs the musl pieces from an unpacked `rust-std`.
fn install_rust_std(root: &Path, target: &str, dir: &str, out: &Path) -> Result<(), String> {
    let from = root.join(format!(
        "rust-std-{target}/lib/rustlib/{target}/lib/self-contained"
    ));
    let copied = copy_files(&from, &out.join("sysroot").join(dir), |_| true)?;
    if copied == 0 {
        return Err(format!("{} is empty", from.display()));
    }
    Ok(())
}

/// Changes whenever the installed layout does, so that the stamp of an
/// older layout is not trusted.
const LAYOUT: u32 = 1;

/// Every file and symlink an install put under `ld/` and `sysroot/`,
/// relative to `out` with `/` separators, sorted.
fn manifest(out: &Path) -> Vec<String> {
    fn walk(dir: &Path, prefix: &str, files: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = format!("{prefix}/{}", entry.file_name().to_string_lossy());
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                walk(&entry.path(), &path, files);
            } else {
                files.push(path);
            }
        }
    }
    let mut files = Vec::new();
    for top in ["ld", "sysroot"] {
        walk(&out.join(top), top, &mut files);
    }
    files.sort();
    files
}

/// Whether the stamp records an install of the same archives and layout,
/// and every file it lists is still there. `exists` follows symlinks, so a
/// dangling `ld.lld` fails too.
fn stamped(out: &Path, header: &str) -> bool {
    let Ok(stamp) = fs::read_to_string(out.join("stamp")) else {
        return false;
    };
    let Some(files) = stamp.strip_prefix(header) else {
        return false;
    };
    let mut files = files.lines().peekable();
    files.peek().is_some() && files.all(|file| out.join(file).exists())
}

/// Replaces `out/LICENSES` with the repository's `LICENSES/` and Mora's own
/// `LICENSE-*` files beside it, the set the toolchain's `LICENSES/` holds.
fn copy_licenses(licenses: &Path, out: &Path) -> Result<(), String> {
    let dir = out.join("LICENSES");
    let _ = fs::remove_dir_all(&dir);
    copy_files(licenses, &dir, |_| true)?;
    if let Some(repo) = licenses.parent() {
        copy_files(repo, &dir, |name| name.starts_with("LICENSE-"))?;
    }
    Ok(())
}

/// Deletes downloads that no pinned archive names any more, such as the
/// archives of an older pin or an interrupted download.
fn prune(downloads: &Path, archives: &[Archive]) {
    let Ok(entries) = fs::read_dir(downloads) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !archives
            .iter()
            .any(|archive| name.to_string_lossy() == archive.file)
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// What `install` did.
#[derive(Debug, PartialEq, Eq)]
pub enum Installed {
    /// The pinned archives were already installed.
    UpToDate,
    /// The archives were installed, with the first line `ld.lld --version`
    /// printed.
    Fresh(String),
}

/// Installs every archive into `out` (normally `target/sysroot`), keeping
/// downloads in `out/downloads`. `licenses` is the repository's
/// `LICENSES/`; Mora's `LICENSE-*` files beside it are copied too. Only
/// the licenses are copied again when the stamp names the same archives
/// and layout, every file it lists is there and `ld.lld` runs.
pub fn install(archives: &[Archive], licenses: &Path, out: &Path) -> Result<Installed, String> {
    let stamp_file = out.join("stamp");
    let header: String = std::iter::once(format!("layout {LAYOUT}\n"))
        .chain(
            archives
                .iter()
                .map(|archive| format!("{} {}\n", archive.sha256, archive.file)),
        )
        .chain(std::iter::once("files\n".to_owned()))
        .collect();
    if stamped(out, &header) && lld_version(out).is_ok() {
        // Cheap, and keeps them in step with the repository.
        copy_licenses(licenses, out)?;
        return Ok(Installed::UpToDate);
    }
    let downloads = out.join("downloads");
    prune(&downloads, archives);
    let paths = archives
        .iter()
        .map(|archive| fetch(archive, &downloads))
        .collect::<Result<Vec<_>, _>>()?;

    for stale in ["stamp", "ld", "sysroot", "LICENSES"] {
        let path = out.join(stale);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir_all(&path);
    }
    let work = out.join("work");
    for (archive, path) in archives.iter().zip(&paths) {
        let root = unpack(path, &work)?;
        if archive.file.starts_with("llvm-mingw-") {
            install_llvm_mingw(&root, out)?;
        } else if let Some(musl) = RUST_STD_MUSL
            .iter()
            .find(|musl| archive.file.contains(musl.target))
        {
            install_rust_std(&root, musl.target, musl.dir, out)?;
        } else {
            return Err(format!("no install rule for {}", archive.file));
        }
    }
    let _ = fs::remove_dir_all(&work);
    copy_licenses(licenses, out)?;
    let version = lld_version(out)?;
    let stamp: String = std::iter::once(header)
        .chain(manifest(out).into_iter().map(|file| file + "\n"))
        .collect();
    fs::write(&stamp_file, stamp)
        .map_err(|err| format!("cannot write {}: {err}", stamp_file.display()))?;
    Ok(Installed::Fresh(version))
}

/// Runs the installed `ld.lld --version`, which fails when a library it
/// loads is missing.
fn lld_version(out: &Path) -> Result<String, String> {
    let lld = out
        .join("ld/bin")
        .join(format!("ld.lld{}", std::env::consts::EXE_SUFFIX));
    let output = Command::new(&lld)
        .arg("--version")
        .output()
        .map_err(|err| format!("cannot run {}: {err}", lld.display()))?;
    if !output.status.success() {
        return Err(format!(
            "{} --version failed:\n{}",
            lld.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::{self, scratch};

    /// A `file://` URL that curl accepts on every platform.
    fn file_url(path: &Path) -> String {
        let path = path.to_string_lossy().replace('\\', "/");
        if path.starts_with('/') {
            format!("file://{path}")
        } else {
            format!("file:///{path}")
        }
    }

    #[test]
    fn hashes_files() {
        let dir = scratch("hash");
        let file = dir.join("abc");
        fs::write(&file, "abc").unwrap();
        assert_eq!(
            sha256(&file).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_download_with_the_pinned_hash_is_kept() {
        let dir = scratch("good");
        let source = dir.join("source.tar.gz");
        fs::write(&source, "abc").unwrap();
        let archive = Archive {
            file: "a.tar.gz".to_owned(),
            url: file_url(&source),
            sha256: sha256(&source).unwrap(),
        };
        let path = fetch(&archive, &dir.join("downloads")).unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "abc");
    }

    #[test]
    fn a_download_with_another_hash_is_refused_and_deleted() {
        let dir = scratch("bad");
        let source = dir.join("source.tar.gz");
        fs::write(&source, "abc").unwrap();
        let archive = Archive {
            file: "a.tar.gz".to_owned(),
            url: file_url(&source),
            sha256: "0".repeat(64),
        };
        let downloads = dir.join("downloads");
        let error = fetch(&archive, &downloads).unwrap_err();
        assert!(error.contains("refusing it"), "{error}");
        assert_eq!(fs::read_dir(&downloads).unwrap().count(), 0);
    }

    #[test]
    fn a_tampered_earlier_download_is_fetched_again() {
        let dir = scratch("tampered");
        let source = dir.join("source.tar.gz");
        fs::write(&source, "abc").unwrap();
        let downloads = dir.join("downloads");
        fs::create_dir_all(&downloads).unwrap();
        fs::write(downloads.join("a.tar.gz"), "evil").unwrap();
        let archive = Archive {
            file: "a.tar.gz".to_owned(),
            url: file_url(&source),
            sha256: sha256(&source).unwrap(),
        };
        let path = fetch(&archive, &downloads).unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "abc");
    }

    #[test]
    fn rust_std_supplies_the_musl_sysroot() {
        let dir = scratch("rust-std");
        let target = "x86_64-unknown-linux-musl";
        let tarball = pack_rust_std(&dir, target, &dir);

        let out = dir.join("out");
        let root = unpack(&tarball, &out.join("work")).unwrap();
        install_rust_std(&root, target, "x86_64-linux", &out).unwrap();
        assert_eq!(
            fs::read_to_string(out.join("sysroot/x86_64-linux/crt1.o")).unwrap(),
            "crt1"
        );
        assert!(out.join("sysroot/x86_64-linux/libc.a").is_file());
    }

    #[test]
    fn the_stamp_lists_every_installed_file() {
        let out = scratch("stamp");
        for file in [
            "ld/bin/ld.lld",
            "ld/lib/libLLVM.so",
            "sysroot/x86_64-linux/libc.a",
        ] {
            fs::create_dir_all(out.join(file).parent().unwrap()).unwrap();
            fs::write(out.join(file), "").unwrap();
        }
        let header = "layout 1\nfiles\n";
        let files = manifest(&out);
        assert_eq!(
            files,
            [
                "ld/bin/ld.lld",
                "ld/lib/libLLVM.so",
                "sysroot/x86_64-linux/libc.a"
            ]
        );
        let stamp: String = files.iter().map(|file| format!("{file}\n")).collect();
        fs::write(out.join("stamp"), format!("{header}{stamp}")).unwrap();
        assert!(stamped(&out, header));
        assert!(!stamped(&out, "layout 2\nfiles\n"));

        fs::remove_file(out.join("sysroot/x86_64-linux/libc.a")).unwrap();
        assert!(!stamped(&out, header));
    }

    #[test]
    fn old_downloads_are_pruned() {
        let dir = scratch("prune");
        for file in ["keep.tar.gz", "old.tar.gz", "keep.tar.gz.part"] {
            fs::write(dir.join(file), "").unwrap();
        }
        let keep = Archive {
            file: "keep.tar.gz".to_owned(),
            url: String::new(),
            sha256: String::new(),
        };
        prune(&dir, &[keep]);
        let left: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(left, ["keep.tar.gz"]);
    }

    #[test]
    fn every_supported_host_has_an_llvm_mingw_build() {
        for arch in ["x86_64", "aarch64"] {
            for os in ["linux", "macos", "windows"] {
                let archive = llvm_mingw(arch, os).unwrap();
                assert!(archive.url.ends_with(&archive.file));
                assert_eq!(archive.sha256.len(), 64);
            }
        }
        assert!(llvm_mingw("riscv64", "linux").is_err());
    }

    #[test]
    fn each_host_gets_its_own_archives() {
        let files: Vec<_> = archives("x86_64", "linux")
            .unwrap()
            .into_iter()
            .map(|archive| archive.file)
            .collect();
        assert_eq!(
            files,
            [
                format!("llvm-mingw-{LLVM_MINGW}-ucrt-ubuntu-22.04-x86_64.tar.xz"),
                format!("rust-std-{RUST_VERSION}-x86_64-unknown-linux-musl.tar.gz"),
                format!("rust-std-{RUST_VERSION}-aarch64-unknown-linux-musl.tar.gz"),
            ]
        );
        assert_eq!(
            llvm_mingw("aarch64", "windows").unwrap().file,
            format!("llvm-mingw-{LLVM_MINGW}-ucrt-aarch64.zip")
        );
    }

    #[test]
    fn an_archive_without_a_top_directory_is_refused() {
        let dir = scratch("flat");
        fs::write(dir.join("only.txt"), "flat").unwrap();
        let tarball = dir.join("flat.tar.gz");
        pack(&tarball, &dir, "only.txt");
        let error = unpack(&tarball, &dir.join("work")).unwrap_err();
        assert!(error.contains("does not hold one directory"), "{error}");
    }

    /// Packs `entry`, relative to `dir`, into the gzipped tarball `tarball`.
    fn pack(tarball: &Path, dir: &Path, entry: &str) {
        let status = Command::new(tar())
            .arg("-czf")
            .arg(tarball)
            .arg("-C")
            .arg(dir)
            .arg(entry)
            .status()
            .unwrap();
        assert!(status.success());
    }

    /// Packs a `rust-std` for `target` whose self-contained directory holds
    /// `crt1.o` ("crt1") and `libc.a` ("libc"), built under `dir`, into
    /// `into` with its release name, and returns the tarball's path.
    fn pack_rust_std(dir: &Path, target: &str, into: &Path) -> PathBuf {
        let top = format!("rust-std-{RUST_VERSION}-{target}");
        let contained = dir.join("src").join(&top).join(format!(
            "rust-std-{target}/lib/rustlib/{target}/lib/self-contained"
        ));
        fs::create_dir_all(&contained).unwrap();
        fs::write(contained.join("crt1.o"), "crt1").unwrap();
        fs::write(contained.join("libc.a"), "libc").unwrap();
        fs::create_dir_all(into).unwrap();
        let tarball = into.join(format!("{top}.tar.gz"));
        pack(&tarball, &dir.join("src"), &top);
        tarball
    }

    /// Writes `files` under `dir`, creating their directories.
    fn write_files(dir: &Path, files: &[&str]) {
        for file in files {
            let path = dir.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, *file).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn llvm_mingw_supplies_lld_and_the_mingw_sysroot() {
        let dir = scratch("llvm-mingw");
        let root = dir.join("llvm-mingw");
        write_files(
            &root,
            &[
                "bin/lld",
                "bin/clang",
                "lib/libLLVM-21.so",
                "lib/libc++.so",
                "x86_64-w64-mingw32/lib/crt2.o",
                "x86_64-w64-mingw32/lib/gcrt2.o",
                "x86_64-w64-mingw32/lib/libmingwex.a",
                "x86_64-w64-mingw32/lib/libmingwex.la",
                "aarch64-w64-mingw32/lib/crt2.o",
                "lib/clang/21/lib/windows/libclang_rt.builtins-x86_64.a",
                "lib/clang/21/lib/windows/libclang_rt.builtins-aarch64.a",
                "lib/clang/21/lib/windows/libclang_rt.profile-x86_64.a",
            ],
        );
        std::os::unix::fs::symlink(
            "libmingwex.a",
            root.join("x86_64-w64-mingw32/lib/libmingw32.a"),
        )
        .unwrap();
        let out = dir.join("out");
        install_llvm_mingw(&root, &out).unwrap();
        assert_eq!(
            manifest(&out),
            [
                "ld/bin/ld.lld",
                "ld/bin/lld",
                "ld/bin/lld-link",
                "ld/lib/libLLVM-21.so",
                "sysroot/aarch64-windows/crt2.o",
                "sysroot/aarch64-windows/libclang_rt.builtins-aarch64.a",
                "sysroot/x86_64-windows/crt2.o",
                "sysroot/x86_64-windows/libclang_rt.builtins-x86_64.a",
                "sysroot/x86_64-windows/libmingwex.a",
            ]
        );
        for name in ["ld.lld", "lld-link"] {
            assert_eq!(
                fs::read_link(out.join("ld/bin").join(name)).unwrap(),
                Path::new("lld")
            );
        }

        fs::remove_file(root.join("lib/clang/21/lib/windows/libclang_rt.builtins-aarch64.a"))
            .unwrap();
        let error = install_llvm_mingw(&root, &dir.join("out2")).unwrap_err();
        assert!(
            error.contains("expected one libclang_rt.builtins-aarch64.a"),
            "{error}"
        );
    }

    /// A repository's `LICENSES/` with one text, and Mora's own license.
    fn licenses(dir: &Path) -> PathBuf {
        write_files(
            dir,
            &[
                "repo/LICENSES/MIT.txt",
                "repo/LICENSE-MIT",
                "repo/README.md",
            ],
        );
        dir.join("repo/LICENSES")
    }

    #[test]
    fn licenses_replace_the_old_ones() {
        let dir = scratch("licenses");
        let out = dir.join("out");
        write_files(&out, &["LICENSES/old.txt"]);
        copy_licenses(&licenses(&dir), &out).unwrap();
        let mut names: Vec<_> = fs::read_dir(out.join("LICENSES"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["LICENSE-MIT", "MIT.txt"]);
    }

    /// Writes a stamp for no archives that lists only `ld.lld`.
    fn stamp_lld(out: &Path) -> String {
        let lld = format!("ld/bin/ld.lld{}", std::env::consts::EXE_SUFFIX);
        fs::write(
            out.join("stamp"),
            format!("layout {LAYOUT}\nfiles\n{lld}\n"),
        )
        .unwrap();
        lld
    }

    #[test]
    fn a_stamped_install_whose_lld_fails_is_redone() {
        let dir = scratch("broken-lld");
        let licenses = licenses(&dir);
        let out = dir.join("out");
        fs::create_dir_all(&out).unwrap();
        let lld = stamp_lld(&out);
        write_files(&out, &[&lld]);
        let error = install(&[], &licenses, &out).unwrap_err();
        assert!(error.contains("cannot run"), "{error}");
        assert!(!out.join("stamp").exists());
    }

    #[test]
    fn a_musl_archive_installs_its_self_contained_files() {
        let dir = scratch("musl");
        let licenses = licenses(&dir);
        let out = dir.join("out");
        let tarball = pack_rust_std(&dir, "x86_64-unknown-linux-musl", &out.join("downloads"));
        let archive = Archive {
            file: tarball.file_name().unwrap().to_string_lossy().into_owned(),
            url: String::new(),
            sha256: sha256(&tarball).unwrap(),
        };
        // A rust-std archive brings no `ld.lld`, so the check after it fails.
        let error = install(&[archive], &licenses, &out).unwrap_err();
        assert!(error.contains("cannot run"), "{error}");
        assert_eq!(
            fs::read_to_string(out.join("sysroot/x86_64-linux/crt1.o")).unwrap(),
            "crt1"
        );
    }

    /// Runs `f` again while running the fake `ld.lld` fails with "Text file
    /// busy": a process another test thread starts at that moment can still
    /// hold the just-written script open for writing.
    #[cfg(unix)]
    fn retry_busy<T, E: std::fmt::Display>(f: impl Fn() -> Result<T, E>) -> Result<T, E> {
        for _ in 0..10 {
            match f() {
                Err(err) if err.to_string().contains("Text file busy") => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                other => return other,
            }
        }
        f()
    }

    /// Installs a shell script as `ld.lld`.
    #[cfg(unix)]
    fn fake_lld(out: &Path, script: &str) {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir_all(out.join("ld/bin")).unwrap();
        let lld = out.join(stamp_lld(out));
        fs::write(&lld, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&lld, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn lld_version_is_its_first_line() {
        let dir = scratch("lld-version");
        fake_lld(
            &dir,
            "echo 'LLD 21.1.0 (compatible with GNU linkers)'; echo more",
        );
        assert_eq!(
            retry_busy(|| lld_version(&dir)).unwrap(),
            "LLD 21.1.0 (compatible with GNU linkers)"
        );
        fake_lld(&dir, "echo 'libLLVM missing' >&2; exit 1");
        let error = retry_busy(|| lld_version(&dir)).unwrap_err();
        assert!(
            error.contains("--version failed") && error.contains("libLLVM missing"),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_stamped_install_whose_lld_runs_is_up_to_date() {
        let dir = scratch("up-to-date");
        let licenses = licenses(&dir);
        let out = dir.join("out");
        fake_lld(&out, "echo LLD");
        // Wait until the script runs: when `install` finds it busy, it treats
        // the install as broken and removes `ld/`, so retrying `install`
        // itself would find nothing to run.
        retry_busy(|| lld_version(&out)).unwrap();
        assert_eq!(
            retry_busy(|| install(&[], &licenses, &out)),
            Ok(Installed::UpToDate)
        );
        assert!(out.join("LICENSES/MIT.txt").is_file());
    }

    #[test]
    fn the_license_readme_lists_every_text() {
        let licenses = paths::root().join("LICENSES");
        let readme = fs::read_to_string(licenses.join("README.md")).unwrap();
        for entry in fs::read_dir(&licenses).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            if name.ends_with(".txt") {
                assert!(readme.contains(&format!("| `{name}` |")), "{name}");
            }
        }
    }

    #[test]
    fn the_licenses_cover_every_component() {
        let licenses = paths::root().join("LICENSES");
        for file in [
            "LLVM.txt",
            "musl.txt",
            "mingw-w64-runtime.txt",
            "winpthreads.txt",
            "winstorecompat.txt",
        ] {
            assert!(licenses.join(file).is_file(), "{file}");
        }
    }
}
