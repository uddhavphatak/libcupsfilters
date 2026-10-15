//! CI entry flow: main -> run_action -> pipeline -> individual Runner stages.
//! GitHub Actions prepares the VM/container and Rust toolchain before calling this runner.

use std::env;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

struct RepositoryPin {
    url: &'static str,
    commit: &'static str,
}

const PDFIO_1_6_4: RepositoryPin = RepositoryPin {
    url: "https://github.com/michaelrsweet/pdfio.git",
    commit: "48b185c650e2e732eed260d63909f4ec68ac9353",
};
const LIBPPD_2_1_1: RepositoryPin = RepositoryPin {
    url: "https://github.com/OpenPrinting/libppd.git",
    commit: "2b37a73c02126d1ba031270322b6c79034cc0d0f",
};
const CUPS_FILTERS_2_0_1: RepositoryPin = RepositoryPin {
    url: "https://github.com/OpenPrinting/cups-filters.git",
    commit: "5a73330fbd0cde494d984141f9add1565aef8171",
};
const CUPS_25: RepositoryPin = RepositoryPin {
    url: "https://github.com/OpenPrinting/cups.git",
    commit: "112bb79c5386372f337013c6b526d051122e9dfb",
};
const CUPS_3: RepositoryPin = RepositoryPin {
    url: "https://github.com/OpenPrinting/libcups.git",
    commit: "ab203c8bc157a4eb42bd04b2cc9bb69528edba88",
};
const OPTIONS: &str = "PageSize=A4 printer-resolution=600dpi copies=3";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CupsKind {
    System2,
    Source25,
    Source3,
}

impl CupsKind {
    // Convert the workflow's CUPS mode name into a supported configuration.
    fn parse(value: &str) -> Result<Self> {
        match value {
            "system-2x" => Ok(Self::System2),
            "source-2.5.x" => Ok(Self::Source25),
            "source-3.x" => Ok(Self::Source3),
            _ => Err(format!("unknown CUPS_KIND: {value}").into()),
        }
    }

    // Read CUPS_KIND, using distro CUPS 2 when the variable is absent.
    fn from_env() -> Result<Self> {
        match env::var("CUPS_KIND") {
            Ok(value) => Self::parse(&value),
            Err(env::VarError::NotPresent) => Ok(Self::System2),
            Err(error) => Err(error.into()),
        }
    }

    // Check that a detected CUPS version belongs to the requested release family.
    fn accepts_version(self, version: &str) -> bool {
        let version = version.trim();
        match self {
            Self::System2 => version.starts_with("2."),
            Self::Source25 => version.strip_prefix("2.5").is_some_and(|suffix| {
                suffix.is_empty()
                    || suffix.starts_with('.')
                    || suffix.starts_with('b')
                    || suffix.starts_with("rc")
            }),
            Self::Source3 => version.starts_with("3."),
        }
    }
}

// Validate GNU timeout's duration argument, defaulting to 30 minutes.
fn command_timeout(value: Option<&str>) -> Result<&str> {
    let value = value.unwrap_or("30m");
    let number = value
        .strip_suffix(['s', 'm', 'h'])
        .ok_or("CI_COMMAND_TIMEOUT must be a positive integer followed by s, m, or h")?;
    if !number.bytes().all(|byte| byte.is_ascii_digit()) || number.parse::<u32>()? == 0 {
        return Err("CI_COMMAND_TIMEOUT must be positive".into());
    }
    Ok(value)
}

struct Runner {
    root: PathBuf,
    work: PathBuf,
    prefix: PathBuf,
    results: PathBuf,
}

impl Runner {
    // Validate the repository root and locate dependency, installation, and log directories.
    fn new() -> Result<Self> {
        let root = env::current_dir()?.canonicalize()?;
        if !root
            .join("cupsfilters/test_files/test_file_4pg.pdf")
            .is_file()
        {
            return Err("run from the libcupsfilters repository root".into());
        }
        // Dependencies are fetched here and installed privately rather than into /usr.
        let work = root.join(".ci-work");
        let prefix = work.join("install");
        let results = root.join("ci-results");
        fs::create_dir_all(&results)?;
        Ok(Self {
            root,
            work,
            prefix,
            results,
        })
    }

    // Prepare a timed subprocess with non-interactive settings and private dependency paths.
    fn command(&self, program: &str, dir: &Path) -> Result<Command> {
        // GNU timeout owns the command's process group, including build children.
        let mut command = Command::new("timeout");
        let timeout = match env::var("CI_COMMAND_TIMEOUT") {
            Ok(value) => Some(value),
            Err(env::VarError::NotPresent) => None,
            Err(error) => return Err(error.into()),
        };
        // Prefer our tools, linker inputs, and pkg-config metadata over system installations.
        command
            .args([
                "--kill-after=30s",
                command_timeout(timeout.as_deref())?,
                program,
            ])
            .current_dir(dir)
            .env("LC_ALL", "C")
            .env("DEBIAN_FRONTEND", "noninteractive")
            .env("NEEDRESTART_MODE", "a")
            .env("NEEDRESTART_SUSPEND", "1")
            .env(
                "PATH",
                prepend_path(&[self.prefix.join("bin")], env::var_os("PATH"))?,
            )
            .env(
                "LIBRARY_PATH",
                prepend_path(&[self.prefix.join("lib")], env::var_os("LIBRARY_PATH"))?,
            )
            .env(
                "LDFLAGS",
                format!(
                    "-L{} {}",
                    self.prefix.join("lib").display(),
                    env::var("LDFLAGS").unwrap_or_default()
                ),
            )
            .env(
                "PKG_CONFIG_PATH",
                prepend_path(
                    &[self.prefix.join("lib/pkgconfig")],
                    env::var_os("PKG_CONFIG_PATH"),
                )?,
            )
            .env("LD_LIBRARY_PATH", self.library_path()?);
        Ok(command)
    }

    // Prefer build-tree libcupsfilters and privately installed dependencies at runtime.
    fn library_path(&self) -> Result<OsString> {
        prepend_path(
            &[self.root.join(".libs"), self.prefix.join("lib")],
            env::var_os("LD_LIBRARY_PATH"),
        )
    }

    // Capture a command's output in its stage log, then execute it and check its status.
    fn execute(&self, name: &str, command: &mut Command) -> Result<()> {
        let log = self.results.join(format!("{name}.log"));
        println!("=== {name}: {command:?}");
        let file = File::create(&log)?;
        command
            .stdin(Stdio::null())
            .stdout(file.try_clone()?)
            .stderr(file);
        self.finish(name, command)
    }

    // Execute a prepared command, record its exit status, and propagate failures.
    fn finish(&self, name: &str, command: &mut Command) -> Result<()> {
        let status = command.status()?;
        writeln!(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.results.join("stages.log"))?,
            "{name}: {status}"
        )?;
        if !status.success() {
            return Err(
                format!("{name} failed ({status}); see retained logs in ci-results").into(),
            );
        }
        Ok(())
    }

    // Combine command preparation, argument setup, logging, and execution for simple stages.
    fn run(&self, name: &str, dir: &Path, program: &str, args: &[&str]) -> Result<()> {
        self.execute(name, self.command(program, dir)?.args(args))
    }

    // Install build tools, Git, development libraries, and PDF validation tools through apt.
    fn install_dependencies(&self) -> Result<()> {
        // Containers run as root; native runners need passwordless sudo.
        let user_id = Command::new("id").arg("-u").output()?;
        if !user_id.status.success() {
            return Err("cannot determine user ID for dependency installation".into());
        }
        let needs_sudo = String::from_utf8(user_id.stdout)?.trim() != "0";
        let program = if needs_sudo { "sudo" } else { "apt-get" };

        // Refresh package indexes before requesting dependencies.
        let mut update = self.command(program, &self.root)?;
        if needs_sudo {
            update.args(["-n", "env", "DEBIAN_FRONTEND=noninteractive", "apt-get"]);
        }
        update.arg("update");
        self.execute("deps-update", &mut update)?;

        // Explicitly pass non-interactive settings through sudo as well.
        let mut install = self.command(program, &self.root)?;
        if needs_sudo {
            install.args([
                "-n",
                "env",
                "DEBIAN_FRONTEND=noninteractive",
                "NEEDRESTART_MODE=a",
                "NEEDRESTART_SUSPEND=1",
                "apt-get",
            ]);
        }
        install.args([
            "install",
            "-y",
            "build-essential",
            "autoconf",
            "automake",
            "libtool",
            "pkg-config",
            "gettext",
            "autopoint",
            "autotools-dev",
            "git",
            "ca-certificates",
            "libavahi-client-dev",
            "libssl-dev",
            "libpam-dev",
            "libusb-1.0-0-dev",
            "zlib1g-dev",
            "libexif-dev",
            "liblcms2-dev",
            "libfontconfig1-dev",
            "libfreetype6-dev",
            "libjpeg-dev",
            "libpng-dev",
            "libtiff-dev",
            "libjxl-dev",
            "libdbus-1-dev",
            "mupdf-tools",
            "poppler-utils",
            "ghostscript",
        ]);
        // Source CUPS modes supply their own headers and libraries in the next stage.
        if CupsKind::from_env()? == CupsKind::System2 {
            install.args(["libcups2-dev", "libcupsimage2-dev"]);
        }
        self.execute("deps-install", &mut install)
    }

    // Fetch a dependency's pinned commit and return its verified source directory.
    fn checkout(&self, name: &str, pin: RepositoryPin) -> Result<PathBuf> {
        // Initialize a local repository instead of cloning an entire branch.
        fs::create_dir_all(&self.work)?;
        let source = self.work.join(name);
        if !source.exists() {
            self.run(&format!("{name}-clone"), &self.work, "git", &["init", name])?;
        }
        // Reuse the pinned commit if a previous invocation already fetched it.
        let commit_exists = self
            .command("git", &source)?
            .args(["cat-file", "-e", &format!("{}^{{commit}}", pin.commit)])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success();
        if !commit_exists {
            self.run(
                &format!("{name}-fetch"),
                &source,
                "git",
                &["fetch", "--depth", "1", pin.url, pin.commit],
            )?;
        }
        // Detach HEAD so a moving upstream branch cannot change the selected source.
        self.run(
            &format!("{name}-checkout"),
            &source,
            "git",
            &["checkout", "--detach", pin.commit],
        )?;
        self.run(
            &format!("{name}-revision"),
            &source,
            "git",
            &["rev-parse", "HEAD"],
        )?;
        // Verify the checkout before allowing configure or make to use it.
        let revision = fs::read_to_string(self.results.join(format!("{name}-revision.log")))?;
        if revision.trim() != pin.commit {
            return Err(format!(
                "{name}: expected commit {}, found {}",
                pin.commit,
                revision.trim()
            )
            .into());
        }
        Ok(source)
    }

    // Bootstrap, configure, and compile a project; optionally install it into our prefix.
    fn build(
        &self,
        name: &str,
        source: &Path,
        configure_args: &[&str],
        install: bool,
    ) -> Result<()> {
        // Generate configure and Makefile inputs when the project provides a bootstrap script.
        if source.join("autogen.sh").is_file() {
            self.run(&format!("{name}-autogen"), source, "./autogen.sh", &[])?;
        }

        // Keep every source-built dependency in the same private installation tree.
        let mut configure = self.command("./configure", source)?;
        configure
            .arg(format!("--prefix={}", self.prefix.display()))
            .arg(format!("--libdir={}", self.prefix.join("lib").display()))
            .args(configure_args);
        self.execute(&format!("{name}-configure"), &mut configure)?;

        // Compile with plain make rather than requesting parallel build jobs.
        self.run(&format!("{name}-build"), source, "make", &[])?;

        // Consumers such as cups-filters can be tested directly without installation.
        if install {
            let mut command = self.command("make", source)?;
            command.arg("install").arg(format!(
                "CUPS_DATADIR={}",
                self.prefix.join("share/cups").display()
            ));
            if name == "cups" {
                // CUPS 3 skips host ldconfig when BUILDROOT is nonempty.
                command.arg("BUILDROOT=/");
            }
            self.execute(&format!("{name}-install"), &mut command)?;
        }
        Ok(())
    }

    // Fetch pinned PDFio and install its shared library for libcupsfilters.
    fn build_pdfio(&self) -> Result<()> {
        let source = self.checkout("pdfio", PDFIO_1_6_4)?;
        self.build("pdfio", &source, &["--enable-shared"], true)
    }

    // Use distro CUPS 2 or build the selected pinned CUPS source, then verify selection.
    fn prepare_cups(&self) -> Result<()> {
        match CupsKind::from_env()? {
            // The dependency stage already installed the distro development packages.
            CupsKind::System2 => {}
            CupsKind::Source25 => {
                // Build only libcups rather than the full CUPS server.
                let source = self.checkout("cups", CUPS_25)?;
                self.build("cups", &source, &["--with-components=libcups"], true)?;
            }
            CupsKind::Source3 => {
                let source = self.checkout("cups", CUPS_3)?;
                // CUPS 3 also needs the source dependencies recorded in its submodules.
                self.run(
                    "cups-submodules",
                    &source,
                    "git",
                    &[
                        "submodule",
                        "update",
                        "--init",
                        "--recursive",
                        "--depth",
                        "1",
                    ],
                )?;
                self.build("cups", &source, &[], true)?;
            }
        }
        self.verify_cups()
    }

    // Reject a CUPS version or installation prefix that differs from the requested mode.
    fn verify_cups(&self) -> Result<()> {
        let kind = CupsKind::from_env()?;
        let module = if kind == CupsKind::Source3 {
            "cups3"
        } else {
            "cups"
        };
        // Source builds require pkg-config metadata; distro CUPS 2 may use cups-config.
        let exists = self
            .command("pkg-config", &self.root)?
            .args(["--exists", module])
            .status()?;
        let use_pkg_config = exists.success();
        if !use_pkg_config && kind != CupsKind::System2 {
            return Err(format!("selected CUPS package {module} is missing").into());
        }
        // Prevent cups3 metadata from overriding a deliberately selected CUPS 2 build.
        if kind != CupsKind::Source3
            && self
                .command("pkg-config", &self.root)?
                .args(["--exists", "cups3"])
                .status()?
                .success()
        {
            return Err("cups3 would override the selected CUPS 2 installation".into());
        }
        let (program, version_args, prefix_args) = if use_pkg_config {
            (
                "pkg-config",
                vec!["--modversion", module],
                vec!["--variable=prefix", module],
            )
        } else {
            ("cups-config", vec!["--version"], vec!["--prefix"])
        };
        // Validate the release family first, then distinguish system and private prefixes.
        self.run("selected-cups-version", &self.root, program, &version_args)?;
        let version = fs::read_to_string(self.results.join("selected-cups-version.log"))?;
        if !kind.accepts_version(&version) {
            return Err(format!("wrong CUPS version for {kind:?}: {}", version.trim()).into());
        }
        self.run("selected-cups-prefix", &self.root, program, &prefix_args)?;
        let prefix = fs::read_to_string(self.results.join("selected-cups-prefix.log"))?;
        let staged = Path::new(prefix.trim()) == self.prefix;
        if staged != (kind != CupsKind::System2) {
            return Err(format!("wrong CUPS prefix for {kind:?}: {}", prefix.trim()).into());
        }
        Ok(())
    }

    // Build the checked-out libcupsfilters and verify its installed pkg-config selection.
    fn build_libcupsfilters(&self) -> Result<()> {
        self.verify_cups()?;
        self.build("libcupsfilters", &self.root, &[], true)?;
        // Downstream consumers must discover this installation, not a distro copy.
        self.run(
            "selected-libcupsfilters",
            &self.root,
            "pkg-config",
            &["--variable=libdir", "libcupsfilters"],
        )?;
        let selected = fs::read_to_string(self.results.join("selected-libcupsfilters.log"))?;
        if Path::new(selected.trim()) != self.prefix.join("lib") {
            return Err("pkg-config selected a different libcupsfilters installation".into());
        }
        Ok(())
    }

    // Install pinned libppd, then build pinned cups-filters for CUPS 2 integration testing.
    fn build_consumers(&self) -> Result<()> {
        let kind = CupsKind::from_env()?;
        if kind == CupsKind::Source3 {
            return Err("legacy consumer integration is only supported with CUPS 2".into());
        }
        // Older consumers expect cups-config, which source CUPS 2.5 no longer provides.
        // Copy this executable under that name to activate main's compatibility branch.
        let shim = self.prefix.join("bin/cups-config");
        if kind == CupsKind::Source25 {
            fs::create_dir_all(self.prefix.join("bin"))?;
            fs::copy(env::current_exe()?, &shim)?;
        }
        let compatibility = format!("--with-cups-config={}", shim.display());
        let cups_args: &[&str] = if kind == CupsKind::Source25 {
            &[&compatibility]
        } else {
            &[]
        };
        // cups-filters needs the installed libppd headers and library.
        let libppd = self.checkout("libppd", LIBPPD_2_1_1)?;
        self.build("libppd", &libppd, cups_args, true)?;

        // Leave cups-filters in its build tree; the next stage runs its pdftopdf binary.
        let filters = self.checkout("cups-filters", CUPS_FILTERS_2_0_1)?;
        let mut configure_args = cups_args.to_vec();
        configure_args.push("--disable-foomatic");
        self.build("cups-filters", &filters, &configure_args, false)
    }

    // Clear old smoke-test outputs and record PASS or FAIL for the current invocation.
    fn filter(&self) -> Result<()> {
        let case = self.results.join("passthrough_3copies");
        fs::create_dir_all(&case)?;
        clear_case(&case)?;
        // Record failures too, so a previous successful result cannot remain misleadingly.
        let outcome = self.filter_output(&case);
        let result = match &outcome {
            Ok(()) => "PASS: valid nonempty PDF and rendered previews".to_string(),
            Err(error) => format!("FAIL: {error}"),
        };
        fs::write(case.join("result.txt"), format!("{result}\n"))?;
        outcome
    }

    // Run pdftopdf against our library, validate its PDF, and render page previews.
    fn filter_output(&self, case: &Path) -> Result<()> {
        // Libtool wrappers may overwrite LD_LIBRARY_PATH. Use the actual ELF binary.
        let binary = self.work.join("cups-filters/.libs/pdftopdf");
        let binary = if binary.is_file() {
            binary
        } else {
            self.work.join("cups-filters/pdftopdf")
        };
        // Confirm the consumer loads build-tree libcupsfilters rather than a system copy.
        self.run("pdftopdf-linkage", &self.root, "ldd", &[path_str(&binary)?])?;
        let linkage = fs::read_to_string(self.results.join("pdftopdf-linkage.log"))?;
        verify_linkage(&linkage, &self.root.join(".libs"))?;
        fs::copy(
            self.results.join("pdftopdf-linkage.log"),
            case.join("linkage.log"),
        )?;
        // Preserve the exact invocation beside the output to make failures reproducible.
        let input = self.root.join("cupsfilters/test_files/test_file_4pg.pdf");
        let output = case.join("passthrough_3copies.pdf");
        fs::write(
            case.join("invocation.txt"),
            format!(
                "binary={}\nstdin={}\nCONTENT_TYPE=application/pdf\nFINAL_CONTENT_TYPE=application/pdf\nLD_LIBRARY_PATH={:?}\nargv=1 1 1 1 {OPTIONS:?}\n",
                binary.display(), input.display(), self.library_path()?
            ),
        )?;
        // Feed the fixture through stdin and capture PDF output separately from diagnostics.
        // Clear inherited printer settings so this is a printer-independent PDF test.
        let mut command = self.command(path_str(&binary)?, &self.root)?;
        command
            .args(["1", "1", "1", "1", OPTIONS])
            .env_remove("PPD")
            .env_remove("PRINTER")
            .env("CONTENT_TYPE", "application/pdf")
            .env("FINAL_CONTENT_TYPE", "application/pdf")
            .stdin(File::open(&input)?)
            .stdout(File::create(&output)?)
            .stderr(File::create(case.join("filter.log"))?);
        self.finish("pdftopdf", &mut command)?;
        // This smoke test checks nonempty, readable PDF output, not copy count or layout.
        if fs::metadata(&output)?.len() == 0 {
            return Err("pdftopdf produced an empty output".into());
        }
        self.run("pdfinfo", &self.root, "pdfinfo", &[path_str(&output)?])?;
        let info = fs::read_to_string(self.results.join("pdfinfo.log"))?;
        if page_count(&info)? == 0 {
            return Err("pdftopdf produced a PDF without pages".into());
        }
        fs::write(case.join("pdfinfo.txt"), info)?;
        // Render every output page at 72 dpi for downloadable visual inspection.
        self.run(
            "preview",
            &self.root,
            "pdftoppm",
            &[
                "-r",
                "72",
                "-png",
                path_str(&output)?,
                path_str(&case.join("page"))?,
            ],
        )
    }
}

// Remove known smoke-test artifacts while preserving unrelated files in the case directory.
fn clear_case(case: &Path) -> Result<()> {
    for entry in fs::read_dir(case)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if matches!(
            name.as_ref(),
            "passthrough_3copies.pdf"
                | "invocation.txt"
                | "filter.log"
                | "linkage.log"
                | "pdfinfo.txt"
                | "result.txt"
        ) || (name.starts_with("page-") && name.ends_with(".png"))
        {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

// Convert a filesystem path for string-based command arguments, rejecting invalid UTF-8.
fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| "path is not valid UTF-8".into())
}

// Prepend search directories without discarding existing entries or OS-native path encoding.
fn prepend_path(paths: &[PathBuf], existing: Option<OsString>) -> Result<OsString> {
    let mut entries = paths.to_vec();
    if let Some(existing) = existing {
        entries.extend(env::split_paths(&existing));
    }
    Ok(env::join_paths(entries)?)
}

// Extract the page count from pdfinfo output, rejecting missing or malformed values.
fn page_count(info: &str) -> Result<usize> {
    let pages = info
        .lines()
        .find_map(|line| line.strip_prefix("Pages:"))
        .ok_or("pdfinfo did not report a page count")?;
    Ok(pages.trim().parse()?)
}

// Check that ldd resolved libcupsfilters inside the expected library directory.
fn verify_linkage(linkage: &str, expected: &Path) -> Result<()> {
    let actual = linkage
        .lines()
        .find_map(|line| {
            let (name, resolved) = line.trim().split_once(" => ")?;
            if name.starts_with("libcupsfilters.so") {
                resolved.split_whitespace().next()
            } else {
                None
            }
        })
        .ok_or("ldd did not resolve libcupsfilters")?;
    // Resolve symlinks before comparing directories so the actual library location is checked.
    let actual = Path::new(actual).canonicalize()?;
    if actual.parent() != Some(expected.canonicalize()?.as_path()) {
        return Err(format!(
            "pdftopdf loaded the wrong libcupsfilters: {}",
            actual.display()
        )
        .into());
    }
    Ok(())
}

// Run all stages in dependency order, stopping setup failures but collecting test failures.
fn pipeline(kind: CupsKind, mut run: impl FnMut(&str) -> Result<()>) -> Result<()> {
    // Later stages require all dependency and library builds to succeed.
    run("deps")?; // Install Git, build tools, development libraries, and PDF tools.
    run("cups")?; // Provide and verify the selected CUPS installation.
    run("pdfio")?; // Fetch, build, and install PDFio.
    run("library")?; // Build and install this libcupsfilters checkout.

    let mut failures = Vec::new();
    // Legacy consumers support CUPS 2 only; run the filter only if they build successfully.
    let supports_legacy_consumers = kind != CupsKind::Source3;
    if supports_legacy_consumers {
        match run("consumers") {
            Ok(()) => {
                if let Err(error) = run("filter") {
                    failures.push(format!("filter: {error}"));
                }
            }
            Err(error) => failures.push(format!("consumers: {error}")),
        }
    }
    // Retain regression results even when consumer integration fails.
    if let Err(error) = run("check") {
        failures.push(format!("check: {error}"));
    }
    // Exercise the staged installation with downstream tests on supported CUPS 2 modes.
    if supports_legacy_consumers {
        if let Err(error) = run("autopkgtest") {
            failures.push(format!("autopkgtest: {error}"));
        }
    }
    // Return one combined error after all applicable regression stages have been attempted.
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n").into())
    }
}

// Translate legacy cups-config options to pkg-config; ignore the obsolete --image modifier.
fn cups_config_option(option: &OsStr) -> Result<Option<&'static str>> {
    match option.to_str() {
        Some("--image") => Ok(None),
        Some("--cflags") => Ok(Some("--cflags")),
        Some("--libs") => Ok(Some("--libs")),
        Some("--version") => Ok(Some("--modversion")),
        Some("--datadir") => Ok(Some("--variable=cups_datadir")),
        Some("--serverroot") => Ok(Some("--variable=cups_serverroot")),
        Some("--serverbin") => Ok(Some("--variable=cups_serverbin")),
        _ => Err(format!(
            "unsupported cups-config option: {}",
            option.to_string_lossy()
        )
        .into()),
    }
}

// Emulate the cups-config interface required by pinned consumers using CUPS 2.5 metadata.
fn cups_config() -> Result<()> {
    // The pinned legacy consumers predate CUPS 2.5's switch to cups.pc.
    let options = env::args_os()
        .skip(1)
        .map(|arg| cups_config_option(&arg))
        .collect::<Result<Vec<_>>>()?;
    if options.is_empty() {
        return Err("cups-config requires an option".into());
    }
    // Forward each supported query and leave its stdout available to the calling configure.
    for option in options.into_iter().flatten() {
        let status = Command::new("pkg-config").args([option, "cups"]).status()?;
        if !status.success() {
            return Err(format!("pkg-config {option} cups failed: {status}").into());
        }
    }
    Ok(())
}

// Dispatch a CLI action to the full pipeline or a single independently invocable stage.
fn run_action(runner: &Runner, action: &str) -> Result<()> {
    match action {
        "all" => pipeline(CupsKind::from_env()?, |stage| run_action(runner, stage)),
        "deps" => runner.install_dependencies(),
        "cups" => runner.prepare_cups(),
        "pdfio" => runner.build_pdfio(),
        "library" => runner.build_libcupsfilters(),
        "consumers" => runner.build_consumers(),
        "filter" => {
            let outcome = runner.filter();
            // Publish a short artifact pointer when GitHub provides a step-summary file.
            if let Some(summary) = env::var_os("GITHUB_STEP_SUMMARY") {
                let status = if outcome.is_ok() { "PASS" } else { "FAIL" };
                writeln!(
                    OpenOptions::new().create(true).append(true).open(summary)?,
                    "### cups-filters smoke test\n\n{status}: see `ci-results/passthrough_3copies/result.txt` for details.\n\nDownload the printing artifact for the PDF, page previews, invocation, and logs. Copy count/layout correctness is not yet asserted."
                )?;
            }
            outcome
        }
        "check" => runner.run(
            "make-check",
            &runner.root,
            "make",
            &["check", "V=1", "VERBOSE=1"],
        ),
        "autopkgtest" => runner.run(
            "autopkgtest",
            &runner.root,
            "make",
            &["test-autopkgtest", "V=1"],
        ),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown action: {action}"),
        )
        .into()),
    }
}

// Start the CI runner, or act as the cups-config compatibility shim when invoked by that name.
fn main() -> Result<()> {
    // 1. Handle the renamed executable used by legacy consumers of source CUPS 2.5.
    // This mode answers configuration queries without initializing the CI workspace.
    if env::args_os()
        .next()
        .as_deref()
        .and_then(|arg| Path::new(arg).file_name())
        == Some(OsStr::new("cups-config"))
    {
        return cups_config();
    }
    // 2. Reject an unsupported CUPS_KIND before creating any runner directories.
    CupsKind::from_env()?;

    // 3. Validate the repository root and prepare the build/install/log locations.
    let runner = Runner::new()?;

    // 4. Read "all" or a single stage name from the command line.
    let action = env::args().nth(1).ok_or(
        "usage: libcupsfilters-ci <all|deps|cups|pdfio|library|consumers|filter|check|autopkgtest>",
    )?;

    // 5. Dispatch the action; "all" enters pipeline for deps -> cups -> pdfio -> library
    // followed by consumer integration and the tests applicable to the selected CUPS mode.
    run_action(&runner, &action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

    // Verify supported CUPS mode names and release-family matching.
    #[test]
    fn validates_cups_modes_and_versions() {
        assert_eq!(CupsKind::parse("system-2x").unwrap(), CupsKind::System2);
        assert_eq!(CupsKind::parse("source-2.5.x").unwrap(), CupsKind::Source25);
        assert_eq!(CupsKind::parse("source-3.x").unwrap(), CupsKind::Source3);
        assert!(CupsKind::parse("source-4.x").is_err());
        assert!(CupsKind::System2.accepts_version("2.4.16\n"));
        assert!(CupsKind::Source25.accepts_version("2.5b1\n"));
        assert!(CupsKind::Source3.accepts_version("3.0.4\n"));
        assert!(!CupsKind::Source25.accepts_version("2.4.16"));
        assert!(!CupsKind::Source25.accepts_version("2.50"));
        assert!(!CupsKind::Source3.accepts_version("2.5b1"));
    }

    // Verify the complete stage order for every supported CUPS configuration.
    #[test]
    fn pipeline_covers_each_cups_mode() {
        for kind in [CupsKind::System2, CupsKind::Source25, CupsKind::Source3] {
            let mut stages = Vec::new();
            pipeline(kind, |stage| {
                stages.push(stage.to_string());
                Ok(())
            })
            .unwrap();
            let expected = if kind == CupsKind::Source3 {
                vec!["deps", "cups", "pdfio", "library", "check"]
            } else {
                vec![
                    "deps",
                    "cups",
                    "pdfio",
                    "library",
                    "consumers",
                    "filter",
                    "check",
                    "autopkgtest",
                ]
            };
            assert_eq!(stages, expected);
        }
    }

    // Verify consumer build failures skip the filter but retain and aggregate regression results.
    #[test]
    fn pipeline_retains_regression_tests_after_consumer_failure() {
        let mut stages = Vec::new();
        let result = pipeline(CupsKind::Source25, |stage| {
            stages.push(stage.to_string());
            if stage == "consumers" || stage == "check" {
                Err(format!("{stage} failed").into())
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        assert_eq!(
            stages,
            [
                "deps",
                "cups",
                "pdfio",
                "library",
                "consumers",
                "check",
                "autopkgtest"
            ]
        );
        let error = result.unwrap_err().to_string();
        assert!(error.contains("consumers failed") && error.contains("check failed"));
    }

    // Verify a failed PDF smoke test does not prevent either regression suite from running.
    #[test]
    fn pipeline_retains_regression_tests_after_filter_failure() {
        let mut stages = Vec::new();
        let result = pipeline(CupsKind::System2, |stage| {
            stages.push(stage.to_string());
            if stage == "filter" {
                Err("filter failed".into())
            } else {
                Ok(())
            }
        });

        assert!(result.unwrap_err().to_string().contains("filter failed"));
        assert_eq!(
            stages,
            [
                "deps",
                "cups",
                "pdfio",
                "library",
                "consumers",
                "filter",
                "check",
                "autopkgtest"
            ]
        );
    }

    // Verify each required setup failure stops the pipeline before dependent stages.
    #[test]
    fn pipeline_stops_after_required_stage_failure() {
        let required_stages = ["deps", "cups", "pdfio", "library"];
        for (index, failed_stage) in required_stages.iter().enumerate() {
            let mut stages = Vec::new();
            let result = pipeline(CupsKind::System2, |stage| {
                stages.push(stage.to_string());
                if stage == *failed_stage {
                    Err(format!("{stage} failed").into())
                } else {
                    Ok(())
                }
            });

            assert!(result.is_err(), "{failed_stage}");
            assert_eq!(stages, required_stages[..=index], "{failed_stage}");
        }
    }

    // Verify compatibility option translation, ignored modifiers, and unsupported-option errors.
    #[test]
    fn legacy_cups_config_maps_only_supported_options() {
        assert_eq!(
            cups_config_option(OsStr::new("--version")).unwrap(),
            Some("--modversion")
        );
        assert_eq!(cups_config_option(OsStr::new("--image")).unwrap(), None);
        assert_eq!(
            cups_config_option(OsStr::new("--libs")).unwrap(),
            Some("--libs")
        );
        assert!(cups_config_option(OsStr::new("--invalid")).is_err());
    }

    // Verify default and explicit timeouts, rejecting zero and malformed durations.
    #[test]
    fn validates_command_timeouts() {
        assert_eq!(command_timeout(None).unwrap(), "30m");
        assert_eq!(command_timeout(Some("180m")).unwrap(), "180m");
        for value in ["", "0m", "-1m", "1.5h", "30", "30d"] {
            assert!(command_timeout(Some(value)).is_err(), "{value}");
        }
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        // Create a uniquely named temporary directory owned by one test.
        fn new() -> Self {
            let path = env::temp_dir().join(format!(
                "libcupsfilters-ci-test-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        // Remove only this test's owned temporary directory when it goes out of scope.
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("clean up owned test directory");
        }
    }

    // Verify page-count parsing accepts valid pdfinfo output and rejects invalid output.
    #[test]
    fn reads_page_count() {
        assert_eq!(page_count("Title: test\nPages:          12\n").unwrap(), 12);
        assert!(page_count("Pages: invalid\n").is_err());
        assert!(page_count("Title: test\n").is_err());
    }

    // Verify private search paths precede, rather than replace, existing system paths.
    #[test]
    fn prepends_search_paths() {
        let result = prepend_path(
            &[PathBuf::from("/checkout/.libs")],
            Some("/system/lib".into()),
        )
        .unwrap();
        assert_eq!(
            env::split_paths(&result).collect::<Vec<_>>(),
            vec![
                PathBuf::from("/checkout/.libs"),
                PathBuf::from("/system/lib")
            ]
        );
    }

    // Verify linkage validation rejects absent and unresolved libcupsfilters entries.
    #[test]
    fn rejects_missing_library() {
        assert!(verify_linkage(
            "libcups.so.2 => /usr/lib/libcups.so.2 (0x00)\n",
            Path::new(".")
        )
        .is_err());
        assert!(verify_linkage("libcupsfilters.so.2 => not found\n", Path::new(".")).is_err());
    }

    // Verify linkage validation accepts only a library resolved in the expected directory.
    #[test]
    fn rejects_system_library() {
        let root = env::current_dir().unwrap();
        let executable = env::current_exe().unwrap();
        let linkage = format!("libcupsfilters.so.2 => {} (0x00)\n", executable.display());
        assert!(verify_linkage(&linkage, &root).is_err());
        assert!(verify_linkage(&linkage, executable.parent().unwrap()).is_ok());
    }

    // Verify case cleanup removes stale generated outputs but keeps unrelated user files.
    #[test]
    fn clears_previous_results_but_preserves_other_files() {
        let directory = TestDirectory::new();
        for name in [
            "result.txt",
            "passthrough_3copies.pdf",
            "page-12.png",
            "notes.txt",
        ] {
            fs::write(directory.0.join(name), "old").unwrap();
        }
        clear_case(&directory.0).unwrap();
        assert!(!directory.0.join("result.txt").exists());
        assert!(!directory.0.join("passthrough_3copies.pdf").exists());
        assert!(!directory.0.join("page-12.png").exists());
        assert!(directory.0.join("notes.txt").exists());
    }

    // Verify unsuccessful commands return errors and retain output and exit-status logs.
    #[test]
    fn command_failures_are_logged_and_propagated() {
        let directory = TestDirectory::new();
        let runner = Runner {
            root: directory.0.clone(),
            work: directory.0.join("work"),
            prefix: directory.0.join("install"),
            results: directory.0.clone(),
        };
        assert!(runner.run("failure", &directory.0, "false", &[]).is_err());
        assert!(directory.0.join("failure.log").is_file());
        assert!(fs::read_to_string(directory.0.join("stages.log"))
            .unwrap()
            .contains("exit status: 1"));
    }

    // Verify a timed-out subprocess is recorded as a failed stage.
    #[test]
    fn timed_out_commands_fail() {
        let directory = TestDirectory::new();
        let runner = Runner {
            root: directory.0.clone(),
            work: directory.0.join("work"),
            prefix: directory.0.join("install"),
            results: directory.0.clone(),
        };
        assert!(runner
            .run(
                "timeout",
                &directory.0,
                "timeout",
                &["0.05s", "sleep", "10"]
            )
            .is_err());
        assert!(fs::read_to_string(directory.0.join("stages.log"))
            .unwrap()
            .contains("exit status: 124"));
    }

    // Verify a missing filter replaces a stale PASS with FAIL and retains linkage diagnostics.
    #[test]
    fn missing_filter_replaces_stale_success_with_failure() {
        let directory = TestDirectory::new();
        let results = directory.0.join("results");
        let case = results.join("passthrough_3copies");
        fs::create_dir_all(&case).unwrap();
        fs::write(case.join("result.txt"), "PASS").unwrap();
        let runner = Runner {
            root: directory.0.clone(),
            work: directory.0.join("missing"),
            prefix: directory.0.join("install"),
            results,
        };
        assert!(runner.filter().is_err());
        assert!(fs::read_to_string(case.join("result.txt"))
            .unwrap()
            .starts_with("FAIL:"));
        assert!(runner.results.join("pdftopdf-linkage.log").is_file());
    }
}
