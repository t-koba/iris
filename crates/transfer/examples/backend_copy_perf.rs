use iris_backends::localfs::adapter::LocalFsAdapter;
use iris_backends::smb::adapter::SmbAdapter;
use iris_backends::smb::connection::{SmbConnectionConfig, DEFAULT_SMB_PORT};
use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};
use iris_core_domain::types::progress::ProgressInfo;
use iris_transfer::executor::client_relay_copy;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

#[derive(Clone, Copy, Debug)]
enum BackendKind {
    Local,
    Smb,
}

#[derive(Clone, Copy, Debug)]
enum CopyMode {
    File,
    Directory,
}

#[derive(Debug)]
struct Args {
    src_kind: BackendKind,
    src_root: PathBuf,
    src_path: PathBuf,
    src_smb_server: Option<String>,
    src_smb_share: Option<String>,
    src_smb_port: u16,
    dst_kind: BackendKind,
    dst_root: PathBuf,
    dst_path: PathBuf,
    dst_smb_server: Option<String>,
    dst_smb_share: Option<String>,
    dst_smb_port: u16,
    mode: CopyMode,
    iterations: usize,
}

#[tokio::main]
async fn main() {
    let args = Args::parse_or_exit();
    if let Err(error) = run(args).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

async fn run(args: Args) -> Result<(), String> {
    let src_backend = make_backend(
        args.src_kind,
        "perf-src",
        &args.src_root,
        args.src_smb_server.as_deref(),
        args.src_smb_share.as_deref(),
        args.src_smb_port,
    )?;
    let dst_backend = make_backend(
        args.dst_kind,
        "perf-dst",
        &args.dst_root,
        args.dst_smb_server.as_deref(),
        args.dst_smb_share.as_deref(),
        args.dst_smb_port,
    )?;
    src_backend
        .connect()
        .await
        .map_err(|error| format!("failed to connect source backend: {error:?}"))?;
    dst_backend
        .connect()
        .await
        .map_err(|error| format!("failed to connect destination backend: {error:?}"))?;

    let files = match args.mode {
        CopyMode::File => vec![args.src_path.clone()],
        CopyMode::Directory => collect_files(&args.src_root, &args.src_path)?,
    };
    let run_id = run_id();
    let started = Instant::now();
    let mut total_bytes = 0u64;
    let mut total_files = 0usize;

    for iteration in 0..args.iterations {
        for relative_src in &files {
            let relative_dst = destination_path(
                &args.src_path,
                relative_src,
                &args.dst_path,
                &run_id,
                iteration,
                args.mode,
            )?;
            let (tx, _rx) = watch::channel(ProgressInfo::single_file(0, None, false));
            let src_ref = object_ref("perf-src", relative_src)?;
            let dst_ref = object_ref("perf-dst", &relative_dst)?;
            let result = client_relay_copy(
                src_backend.as_ref(),
                &src_ref,
                dst_backend.as_ref(),
                &dst_ref,
                &tx,
            )
            .await
            .map_err(|error| {
                format!(
                    "failed to copy {} to {}: {error:?}",
                    relative_src.display(),
                    relative_dst.display()
                )
            })?;
            total_bytes = total_bytes.saturating_add(result.bytes_transferred);
            total_files += 1;
        }
    }

    let elapsed = started.elapsed();
    println!(
        "kind=backend-copy src={:?} dst={:?} mode={:?} bytes={} files={} iterations={} elapsed_ms={} throughput_mib_s={:.2}",
        args.src_kind,
        args.dst_kind,
        args.mode,
        total_bytes,
        total_files,
        args.iterations,
        elapsed.as_millis(),
        mib_per_second(total_bytes, elapsed.as_secs_f64())
    );
    Ok(())
}

impl Args {
    fn parse_or_exit() -> Self {
        match Self::parse(std::env::args().skip(1).collect()) {
            Ok(args) => args,
            Err(error) => {
                eprintln!("{error}");
                usage();
                std::process::exit(2);
            }
        }
    }

    fn parse(tokens: Vec<String>) -> Result<Self, String> {
        let mut parser = Parser::new(tokens);
        let mut args = Args {
            src_kind: BackendKind::Local,
            src_root: PathBuf::new(),
            src_path: PathBuf::new(),
            src_smb_server: None,
            src_smb_share: None,
            src_smb_port: DEFAULT_SMB_PORT,
            dst_kind: BackendKind::Local,
            dst_root: PathBuf::new(),
            dst_path: PathBuf::new(),
            dst_smb_server: None,
            dst_smb_share: None,
            dst_smb_port: DEFAULT_SMB_PORT,
            mode: CopyMode::File,
            iterations: 1,
        };

        while let Some(flag) = parser.next() {
            match flag.as_str() {
                "--src-kind" => args.src_kind = parse_backend_kind(&parser.value("--src-kind")?)?,
                "--src-root" => args.src_root = PathBuf::from(parser.value("--src-root")?),
                "--src-path" => {
                    args.src_path = parse_relative_path("--src-path", &parser.value("--src-path")?)?
                }
                "--src-smb-server" => args.src_smb_server = Some(parser.value("--src-smb-server")?),
                "--src-smb-share" => args.src_smb_share = Some(parser.value("--src-smb-share")?),
                "--src-smb-port" => {
                    args.src_smb_port =
                        parse_port("--src-smb-port", &parser.value("--src-smb-port")?)?
                }
                "--dst-kind" => args.dst_kind = parse_backend_kind(&parser.value("--dst-kind")?)?,
                "--dst-root" => args.dst_root = PathBuf::from(parser.value("--dst-root")?),
                "--dst-path" => {
                    args.dst_path = parse_relative_path("--dst-path", &parser.value("--dst-path")?)?
                }
                "--dst-smb-server" => args.dst_smb_server = Some(parser.value("--dst-smb-server")?),
                "--dst-smb-share" => args.dst_smb_share = Some(parser.value("--dst-smb-share")?),
                "--dst-smb-port" => {
                    args.dst_smb_port =
                        parse_port("--dst-smb-port", &parser.value("--dst-smb-port")?)?
                }
                "--mode" => {
                    args.mode = match parser.value("--mode")?.as_str() {
                        "file" => CopyMode::File,
                        "directory" => CopyMode::Directory,
                        other => return Err(format!("unsupported --mode {other:?}")),
                    };
                }
                "--iterations" => {
                    args.iterations = parser
                        .value("--iterations")?
                        .parse()
                        .map_err(|_| "--iterations must be a positive integer".to_string())?;
                }
                "-h" | "--help" => {
                    usage();
                    std::process::exit(0);
                }
                other => return Err(format!("unknown option: {other}")),
            }
        }

        if matches!(args.src_kind, BackendKind::Local) && !args.src_root.is_absolute() {
            return Err("--src-root must be absolute".to_string());
        }
        if matches!(args.dst_kind, BackendKind::Local) && !args.dst_root.is_absolute() {
            return Err("--dst-root must be absolute".to_string());
        }
        validate_smb_endpoint(
            args.src_kind,
            "--src-smb-server",
            args.src_smb_server.as_deref(),
            "--src-smb-share",
            args.src_smb_share.as_deref(),
        )?;
        validate_smb_endpoint(
            args.dst_kind,
            "--dst-smb-server",
            args.dst_smb_server.as_deref(),
            "--dst-smb-share",
            args.dst_smb_share.as_deref(),
        )?;
        if matches!(args.mode, CopyMode::Directory) && !matches!(args.src_kind, BackendKind::Local)
        {
            return Err("--mode directory requires --src-kind local".to_string());
        }
        if args.src_path.as_os_str().is_empty() {
            return Err("--src-path is required".to_string());
        }
        if args.dst_path.as_os_str().is_empty() {
            return Err("--dst-path is required".to_string());
        }
        if args.iterations == 0 {
            return Err("--iterations must be greater than zero".to_string());
        }
        Ok(args)
    }
}

struct Parser {
    tokens: std::vec::IntoIter<String>,
}

impl Parser {
    fn new(tokens: Vec<String>) -> Self {
        Self {
            tokens: tokens.into_iter(),
        }
    }

    fn next(&mut self) -> Option<String> {
        self.tokens.next()
    }

    fn value(&mut self, flag: &str) -> Result<String, String> {
        self.tokens
            .next()
            .ok_or_else(|| format!("{flag} requires a value"))
    }
}

fn parse_backend_kind(value: &str) -> Result<BackendKind, String> {
    match value {
        "local" => Ok(BackendKind::Local),
        "smb" => Ok(BackendKind::Smb),
        other => Err(format!("unsupported backend kind {other:?}")),
    }
}

fn parse_relative_path(flag: &str, value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        return Err(format!("{flag} must be relative to its root"));
    }
    Ok(path)
}

fn parse_port(flag: &str, value: &str) -> Result<u16, String> {
    value
        .parse()
        .map_err(|_| format!("{flag} must be a TCP port"))
}

fn validate_smb_endpoint(
    kind: BackendKind,
    server_flag: &str,
    server: Option<&str>,
    share_flag: &str,
    share: Option<&str>,
) -> Result<(), String> {
    if !matches!(kind, BackendKind::Smb) {
        return Ok(());
    }
    if server.is_none_or(|server| server.trim().is_empty()) {
        return Err(format!("{server_flag} is required for --*-kind smb"));
    }
    if share.is_none_or(|share| share.trim().is_empty()) {
        return Err(format!("{share_flag} is required for --*-kind smb"));
    }
    Ok(())
}

fn make_backend(
    kind: BackendKind,
    id: &str,
    root: &Path,
    smb_server: Option<&str>,
    smb_share: Option<&str>,
    smb_port: u16,
) -> Result<Box<dyn BackendAdapter>, String> {
    let backend_id = BackendId(id.to_string());
    match kind {
        BackendKind::Local => Ok(Box::new(LocalFsAdapter::new(
            root.to_path_buf(),
            backend_id,
        ))),
        BackendKind::Smb => Ok(Box::new(SmbAdapter::new(
            SmbConnectionConfig {
                server: smb_server.unwrap_or_default().to_string(),
                port: smb_port,
                share_name: smb_share.unwrap_or_default().to_string(),
                username: None,
                domain: None,
                password: None,
                workstation: None,
                allow_guest: true,
            },
            backend_id,
        ))),
    }
}

fn object_ref(backend_id: &str, relative_path: &Path) -> Result<ObjectRef, String> {
    let display_path = relative_path
        .to_str()
        .ok_or_else(|| format!("path {} is not valid UTF-8", relative_path.display()))?
        .to_string();
    Ok(ObjectRef {
        key: ObjectKey {
            backend_id: BackendId(backend_id.to_string()),
            stable_id: StableId(display_path.clone()),
        },
        parent_stable_id: None,
        display_path,
    })
}

fn collect_files(root: &Path, relative_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let mut stack = vec![relative_dir.to_path_buf()];
    while let Some(relative) = stack.pop() {
        let absolute = root.join(&relative);
        for entry in std::fs::read_dir(&absolute)
            .map_err(|error| format!("failed to read directory {}: {error}", absolute.display()))?
        {
            let entry = entry.map_err(|error| {
                format!(
                    "failed to iterate directory {}: {error}",
                    absolute.display()
                )
            })?;
            let child_relative = relative.join(entry.file_name());
            let metadata = entry
                .metadata()
                .map_err(|error| format!("failed to stat {}: {error}", entry.path().display()))?;
            if metadata.is_dir() {
                stack.push(child_relative);
            } else {
                out.push(child_relative);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn destination_path(
    src_root: &Path,
    src_relative: &Path,
    dst_path: &Path,
    run_id: &str,
    iteration: usize,
    mode: CopyMode,
) -> Result<PathBuf, String> {
    let iteration_root = PathBuf::from(run_id).join(format!("iter-{iteration:04}"));
    match mode {
        CopyMode::File => Ok(iteration_root.join(dst_path)),
        CopyMode::Directory => {
            let suffix = src_relative.strip_prefix(src_root).map_err(|error| {
                format!(
                    "source path {} is not under {}: {error}",
                    src_relative.display(),
                    src_root.display()
                )
            })?;
            Ok(iteration_root.join(dst_path).join(suffix))
        }
    }
}

fn run_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    format!("run-{millis}")
}

fn mib_per_second(bytes: u64, seconds: f64) -> f64 {
    if seconds <= f64::EPSILON {
        return 0.0;
    }
    bytes as f64 / 1024.0 / 1024.0 / seconds
}

fn usage() {
    eprintln!(
        "Usage: cargo run -p iris-transfer --release --example backend_copy_perf -- \\
  --src-kind local|smb --src-root ROOT --src-path REL [--src-smb-server HOST --src-smb-share SHARE --src-smb-port PORT] \\
  --dst-kind local|smb --dst-root ROOT --dst-path REL [--dst-smb-server HOST --dst-smb-share SHARE --dst-smb-port PORT] \\
  --mode file|directory [--iterations N]"
    );
}
