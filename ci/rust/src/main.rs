use std::env;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const PDFIO: (&str, &str, &str) = (
    "https://github.com/michaelrsweet/pdfio.git",
    "v1.6.4",
    "48b185c650e2e732eed260d63909f4ec68ac9353",
);
const LIBPPD: (&str, &str, &str) = (
    "https://github.com/OpenPrinting/libppd.git",
    "2.1.1",
    "2b37a73c02126d1ba031270322b6c79034cc0d0f",
);
const CUPS_FILTERS: (&str, &str, &str) = (
    "https://github.com/OpenPrinting/cups-filters.git",
    "2.0.1",
    "5a73330fbd0cde494d984141f9add1565aef8171",
);
const CUPS_25: (&str, &str, &str) = (
    "https://github.com/OpenPrinting/cups.git",
    "master",
    "112bb79c5386372f337013c6b526d051122e9dfb",
);
const CUPS_3: (&str, &str, &str) = (
    "https://github.com/OpenPrinting/libcups.git",
    "master",
    "ab203c8bc157a4eb42bd04b2cc9bb69528edba88",
);
const OPTIONS: &str = "PageSize=A4 printer-resolution=600dpi copies=3";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CupsKind {
    System2,
    Source25,
    Source3,
}

impl CupsKind {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "system-2x" => Ok(Self::System2),
            "source-2.5.x" => Ok(Self::Source25),
            "source-3.x" => Ok(Self::Source3),
            _ => Err(format!("unknown CUPS_KIND: {value}").into()),
        }
    }

    fn from_env() -> Result<Self> {
        match env::var("CUPS_KIND") {
            Ok(value) => Self::parse(&value),
            Err(env::VarError::NotPresent) => Ok(Self::System2),
            Err(error) => Err(error.into()),
        }
    }

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
    fn new() -> Result<Self> {
        let root = env::current_dir()?.canonicalize()?;
        if !root
            .join("cupsfilters/test_files/test_file_4pg.pdf")
            .is_file()
        {
            return Err("run from the libcupsfilters repository root".into());
        }
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

    fn command(&self, program: &str, dir: &Path) -> Result<Command> {
        // GNU timeout owns the command's process group, including build children.
        let mut command = Command::new("timeout");
        let timeout = match env::var("CI_COMMAND_TIMEOUT") {
            Ok(value) => Some(value),
            Err(env::VarError::NotPresent) => None,
            Err(error) => return Err(error.into()),
        };
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

    fn library_path(&self) -> Result<OsString> {
        prepend_path(
            &[self.root.join(".libs"), self.prefix.join("lib")],
            env::var_os("LD_LIBRARY_PATH"),
        )
    }

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

    fn run(&self, name: &str, dir: &Path, program: &str, args: &[&str]) -> Result<()> {
        self.execute(name, self.command(program, dir)?.args(args))
    }

    fn deps(&self) -> Result<()> {
        let root_user = Command::new("id").arg("-u").output()?;
        if !root_user.status.success() {
            return Err("cannot determine user ID for dependency installation".into());
        }
        let privileged = String::from_utf8(root_user.stdout)?.trim() != "0";
        let program = if privileged { "sudo" } else { "apt-get" };
        let mut update = self.command(program, &self.root)?;
        if privileged {
            update.args(["-n", "env", "DEBIAN_FRONTEND=noninteractive", "apt-get"]);
        }
        update.arg("update");
        self.execute("deps-update", &mut update)?;
        let mut install = self.command(program, &self.root)?;
        if privileged {
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
        if CupsKind::from_env()? == CupsKind::System2 {
            install.args(["libcups2-dev", "libcupsimage2-dev"]);
        }
        self.execute("deps-install", &mut install)
    }

    fn checkout(&self, name: &str, pin: (&str, &str, &str)) -> Result<PathBuf> {
        fs::create_dir_all(&self.work)?;
        let source = self.work.join(name);
        if !source.exists() {
            self.run(&format!("{name}-clone"), &self.work, "git", &["init", name])?;
        }
        if !self
            .command("git", &source)?
            .args(["cat-file", "-e", &format!("{}^{{commit}}", pin.2)])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success()
        {
            self.run(
                &format!("{name}-fetch"),
                &source,
                "git",
                &["fetch", "--depth", "1", pin.0, pin.2],
            )?;
        }
        self.run(
            &format!("{name}-checkout"),
            &source,
            "git",
            &["checkout", "--detach", pin.2],
        )?;
        self.run(
            &format!("{name}-revision"),
            &source,
            "git",
            &["rev-parse", "HEAD"],
        )?;
        let revision = fs::read_to_string(self.results.join(format!("{name}-revision.log")))?;
        if revision.trim() != pin.2 {
            return Err(format!(
                "{name}: expected commit {}, found {}",
                pin.2,
                revision.trim()
            )
            .into());
        }
        Ok(source)
    }

    fn build(&self, name: &str, source: &Path, extra: &[&str], install: bool) -> Result<()> {
        if source.join("autogen.sh").is_file() {
            self.run(&format!("{name}-autogen"), source, "./autogen.sh", &[])?;
        }
        let mut configure = self.command("./configure", source)?;
        configure
            .arg(format!("--prefix={}", self.prefix.display()))
            .arg(format!("--libdir={}", self.prefix.join("lib").display()))
            .args(extra);
        self.execute(&format!("{name}-configure"), &mut configure)?;
        let jobs = std::thread::available_parallelism()?.get().to_string();
        self.run(&format!("{name}-build"), source, "make", &["-j", &jobs])?;
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

    fn pdfio(&self) -> Result<()> {
        let source = self.checkout("pdfio", PDFIO)?;
        self.build("pdfio", &source, &["--enable-shared"], true)
    }

    fn cups(&self) -> Result<()> {
        match CupsKind::from_env()? {
            CupsKind::System2 => {}
            CupsKind::Source25 => {
                let source = self.checkout("cups", CUPS_25)?;
                self.build("cups", &source, &["--with-components=libcups"], true)?;
            }
            CupsKind::Source3 => {
                let source = self.checkout("cups", CUPS_3)?;
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

    fn verify_cups(&self) -> Result<()> {
        let kind = CupsKind::from_env()?;
        let module = if kind == CupsKind::Source3 {
            "cups3"
        } else {
            "cups"
        };
        let exists = self
            .command("pkg-config", &self.root)?
            .args(["--exists", module])
            .status()?;
        let use_pkg_config = exists.success();
        if !use_pkg_config && kind != CupsKind::System2 {
            return Err(format!("selected CUPS package {module} is missing").into());
        }
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

    fn library(&self) -> Result<()> {
        self.verify_cups()?;
        self.build("libcupsfilters", &self.root, &[], true)?;
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

    fn consumers(&self) -> Result<()> {
        if CupsKind::from_env()? == CupsKind::Source3 {
            return Err("legacy consumer integration is only supported with CUPS 2".into());
        }
        let shim = self.prefix.join("bin/cups-config");
        if CupsKind::from_env()? == CupsKind::Source25 {
            fs::create_dir_all(self.prefix.join("bin"))?;
            fs::copy(env::current_exe()?, &shim)?;
        }
        let compatibility = format!("--with-cups-config={}", shim.display());
        let cups_args: &[&str] = if CupsKind::from_env()? == CupsKind::Source25 {
            &[&compatibility]
        } else {
            &[]
        };
        let libppd = self.checkout("libppd", LIBPPD)?;
        self.build("libppd", &libppd, cups_args, true)?;
        let filters = self.checkout("cups-filters", CUPS_FILTERS)?;
        let mut args = cups_args.to_vec();
        args.push("--disable-foomatic");
        self.build("cups-filters", &filters, &args, false)
    }

    fn filter(&self) -> Result<()> {
        let case = self.results.join("passthrough_3copies");
        fs::create_dir_all(&case)?;
        clear_case(&case)?;
        let outcome = self.filter_output(&case);
        let result = match &outcome {
            Ok(()) => "PASS: valid nonempty PDF and rendered previews".to_string(),
            Err(error) => format!("FAIL: {error}"),
        };
        fs::write(case.join("result.txt"), format!("{result}\n"))?;
        outcome
    }

    fn filter_output(&self, case: &Path) -> Result<()> {
        // Libtool wrappers may overwrite LD_LIBRARY_PATH. Use the actual ELF binary.
        let binary = self.work.join("cups-filters/.libs/pdftopdf");
        let binary = if binary.is_file() {
            binary
        } else {
            self.work.join("cups-filters/pdftopdf")
        };
        self.run("pdftopdf-linkage", &self.root, "ldd", &[path_str(&binary)?])?;
        let linkage = fs::read_to_string(self.results.join("pdftopdf-linkage.log"))?;
        verify_linkage(&linkage, &self.root.join(".libs"))?;
        fs::copy(
            self.results.join("pdftopdf-linkage.log"),
            case.join("linkage.log"),
        )?;
        let input = self.root.join("cupsfilters/test_files/test_file_4pg.pdf");
        let output = case.join("passthrough_3copies.pdf");
        fs::write(
            case.join("invocation.txt"),
            format!(
                "binary={}\nstdin={}\nCONTENT_TYPE=application/pdf\nFINAL_CONTENT_TYPE=application/pdf\nLD_LIBRARY_PATH={:?}\nargv=1 1 1 1 {OPTIONS:?}\n",
                binary.display(), input.display(), self.library_path()?
            ),
        )?;
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
        if fs::metadata(&output)?.len() == 0 {
            return Err("pdftopdf produced an empty output".into());
        }
        self.run("pdfinfo", &self.root, "pdfinfo", &[path_str(&output)?])?;
        let info = fs::read_to_string(self.results.join("pdfinfo.log"))?;
        if page_count(&info)? == 0 {
            return Err("pdftopdf produced a PDF without pages".into());
        }
        fs::write(case.join("pdfinfo.txt"), info)?;
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

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| "path is not valid UTF-8".into())
}

fn prepend_path(paths: &[PathBuf], existing: Option<OsString>) -> Result<OsString> {
    let mut entries = paths.to_vec();
    if let Some(existing) = existing {
        entries.extend(env::split_paths(&existing));
    }
    Ok(env::join_paths(entries)?)
}

fn page_count(info: &str) -> Result<usize> {
    let pages = info
        .lines()
        .find_map(|line| line.strip_prefix("Pages:"))
        .ok_or("pdfinfo did not report a page count")?;
    Ok(pages.trim().parse()?)
}

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

fn pipeline(kind: CupsKind, mut run: impl FnMut(&str) -> Result<()>) -> Result<()> {
    for stage in ["deps", "cups", "pdfio", "library"] {
        run(stage)?;
    }
    let mut failures = Vec::new();
    if kind != CupsKind::Source3 {
        match run("consumers") {
            Ok(()) => {
                if let Err(error) = run("filter") {
                    failures.push(format!("filter: {error}"));
                }
            }
            Err(error) => failures.push(format!("consumers: {error}")),
        }
    }
    if let Err(error) = run("check") {
        failures.push(format!("check: {error}"));
    }
    if kind != CupsKind::Source3 {
        if let Err(error) = run("autopkgtest") {
            failures.push(format!("autopkgtest: {error}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n").into())
    }
}

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

fn cups_config() -> Result<()> {
    // The pinned legacy consumers predate CUPS 2.5's switch to cups.pc.
    let options = env::args_os()
        .skip(1)
        .map(|arg| cups_config_option(&arg))
        .collect::<Result<Vec<_>>>()?;
    if options.is_empty() {
        return Err("cups-config requires an option".into());
    }
    for option in options.into_iter().flatten() {
        let status = Command::new("pkg-config").args([option, "cups"]).status()?;
        if !status.success() {
            return Err(format!("pkg-config {option} cups failed: {status}").into());
        }
    }
    Ok(())
}

fn run_action(runner: &Runner, action: &str) -> Result<()> {
    match action {
        "all" => pipeline(CupsKind::from_env()?, |stage| run_action(runner, stage)),
        "deps" => runner.deps(),
        "cups" => runner.cups(),
        "pdfio" => runner.pdfio(),
        "library" => runner.library(),
        "consumers" => runner.consumers(),
        "filter" => {
            let outcome = runner.filter();
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

fn main() -> Result<()> {
    if env::args_os()
        .next()
        .as_deref()
        .and_then(|arg| Path::new(arg).file_name())
        == Some(OsStr::new("cups-config"))
    {
        return cups_config();
    }
    CupsKind::from_env()?;
    let runner = Runner::new()?;
    let action = env::args().nth(1).ok_or(
        "usage: libcupsfilters-ci <all|deps|cups|pdfio|library|consumers|filter|check|autopkgtest>",
    )?;
    run_action(&runner, &action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

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

    #[test]
    fn pipeline_stops_after_library_failure() {
        let mut stages = Vec::new();
        assert!(pipeline(CupsKind::System2, |stage| {
            stages.push(stage.to_string());
            if stage == "library" {
                Err("library failed".into())
            } else {
                Ok(())
            }
        })
        .is_err());
        assert_eq!(stages, ["deps", "cups", "pdfio", "library"]);
    }

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
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("clean up owned test directory");
        }
    }

    #[test]
    fn reads_page_count() {
        assert_eq!(page_count("Title: test\nPages:          12\n").unwrap(), 12);
        assert!(page_count("Pages: invalid\n").is_err());
        assert!(page_count("Title: test\n").is_err());
    }

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

    #[test]
    fn rejects_missing_library() {
        assert!(verify_linkage(
            "libcups.so.2 => /usr/lib/libcups.so.2 (0x00)\n",
            Path::new(".")
        )
        .is_err());
        assert!(verify_linkage("libcupsfilters.so.2 => not found\n", Path::new(".")).is_err());
    }

    #[test]
    fn rejects_system_library() {
        let root = env::current_dir().unwrap();
        let executable = env::current_exe().unwrap();
        let linkage = format!("libcupsfilters.so.2 => {} (0x00)\n", executable.display());
        assert!(verify_linkage(&linkage, &root).is_err());
        assert!(verify_linkage(&linkage, executable.parent().unwrap()).is_ok());
    }

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
