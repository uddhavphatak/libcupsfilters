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
const OPTIONS: &str = "PageSize=A4 printer-resolution=600dpi copies=3";

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
        command
            .args(["--kill-after=30s", "30m", program])
            .current_dir(dir)
            .env("LC_ALL", "C")
            .env("DEBIAN_FRONTEND", "noninteractive")
            .env("NEEDRESTART_MODE", "a")
            .env("NEEDRESTART_SUSPEND", "1")
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
            "libcups2-dev",
            "libcupsimage2-dev",
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
        self.execute("deps-install", &mut install)
    }

    fn checkout(&self, name: &str, pin: (&str, &str, &str)) -> Result<PathBuf> {
        fs::create_dir_all(&self.work)?;
        let source = self.work.join(name);
        if !source.exists() {
            self.run(
                &format!("{name}-clone"),
                &self.work,
                "git",
                &["clone", "--depth", "1", "--branch", pin.1, pin.0, name],
            )?;
        }
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
            self.execute(&format!("{name}-install"), &mut command)?;
        }
        Ok(())
    }

    fn pdfio(&self) -> Result<()> {
        let source = self.checkout("pdfio", PDFIO)?;
        self.build("pdfio", &source, &["--enable-shared"], true)
    }

    fn library(&self) -> Result<()> {
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
        let libppd = self.checkout("libppd", LIBPPD)?;
        self.build("libppd", &libppd, &[], true)?;
        let filters = self.checkout("cups-filters", CUPS_FILTERS)?;
        self.build("cups-filters", &filters, &["--disable-foomatic"], false)
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

fn main() -> Result<()> {
    let runner = Runner::new()?;
    let action = env::args_os().nth(1).ok_or(
        "usage: libcupsfilters-ci <deps|pdfio|library|consumers|filter|check|autopkgtest>",
    )?;
    match action.to_str() {
        Some("deps") => runner.deps(),
        Some("pdfio") => runner.pdfio(),
        Some("library") => runner.library(),
        Some("consumers") => runner.consumers(),
        Some("filter") => {
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
        Some("check") => runner.run(
            "make-check",
            &runner.root,
            "make",
            &["check", "V=1", "VERBOSE=1"],
        ),
        Some("autopkgtest") => runner.run(
            "autopkgtest",
            &runner.root,
            "make",
            &["test-autopkgtest", "V=1"],
        ),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown action: {}", OsStr::new(&action).to_string_lossy()),
        )
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

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
