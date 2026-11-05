use iris_remote_transport::{
    execute_sftp_plan, RemoteTransportKind, RemoteTransportMapping, SftpTransferDirection,
    SftpTransferEntry, SftpTransferPlan, SshAuthKind, DEFAULT_SFTP_PIPELINE_DEPTH,
};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Clone, Copy, Debug)]
enum Direction {
    Upload,
    Download,
}

#[derive(Debug)]
struct Args {
    host: String,
    port: u16,
    user: String,
    password: String,
    local_path: PathBuf,
    remote_path: String,
    direction: Direction,
    is_dir: bool,
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
    let local_mount_path = local_mount_path(&args.local_path)?;
    let mapping = RemoteTransportMapping {
        id: "perf-sftp".to_string(),
        local_mount_path,
        host: args.host.clone(),
        ssh_port: args.port,
        ssh_username: args.user.clone(),
        remote_base_path: "/".to_string(),
        transport: RemoteTransportKind::Sftp,
        auth: SshAuthKind::Password,
        ssh_identity_file: String::new(),
        ssh_password: args.password.clone(),
        enabled: true,
        auto_detected: false,
        notes: "P0 transfer performance harness".to_string(),
    };

    let direction = match args.direction {
        Direction::Upload => SftpTransferDirection::Upload,
        Direction::Download => SftpTransferDirection::Download,
    };
    let started = Instant::now();
    for _ in 0..args.iterations {
        execute_sftp_plan(&SftpTransferPlan {
            mapping: mapping.clone(),
            direction: direction.clone(),
            delete_sources: false,
            pipeline_depth: DEFAULT_SFTP_PIPELINE_DEPTH,
            entries: vec![SftpTransferEntry {
                local_path: args.local_path.clone(),
                remote_path: args.remote_path.clone(),
                is_dir: args.is_dir,
            }],
        })
        .await?;
    }

    let elapsed = started.elapsed();
    let bytes_per_iteration = path_size(&args.local_path)?;
    let total_bytes = bytes_per_iteration.saturating_mul(args.iterations as u64);
    let throughput = mib_per_second(total_bytes, elapsed.as_secs_f64());
    println!(
        "kind=sftp direction={:?} bytes={} iterations={} elapsed_ms={} throughput_mib_s={:.2}",
        args.direction,
        total_bytes,
        args.iterations,
        elapsed.as_millis(),
        throughput
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
            host: "127.0.0.1".to_string(),
            port: 2222,
            user: "iris".to_string(),
            password: "iris".to_string(),
            local_path: PathBuf::new(),
            remote_path: String::new(),
            direction: Direction::Upload,
            is_dir: false,
            iterations: 1,
        };

        while let Some(flag) = parser.next() {
            match flag.as_str() {
                "--host" => args.host = parser.value("--host")?,
                "--port" => {
                    args.port = parser
                        .value("--port")?
                        .parse()
                        .map_err(|_| "--port must be a number between 1 and 65535".to_string())?
                }
                "--user" => args.user = parser.value("--user")?,
                "--password" => args.password = parser.value("--password")?,
                "--local-path" => args.local_path = PathBuf::from(parser.value("--local-path")?),
                "--remote-path" => args.remote_path = parser.value("--remote-path")?,
                "--direction" => {
                    args.direction = match parser.value("--direction")?.as_str() {
                        "upload" => Direction::Upload,
                        "download" => Direction::Download,
                        other => {
                            return Err(format!("unsupported --direction {other:?}"));
                        }
                    }
                }
                "--dir" => args.is_dir = true,
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

        if args.local_path.as_os_str().is_empty() {
            return Err("--local-path is required".to_string());
        }
        if !args.local_path.is_absolute() {
            return Err("--local-path must be absolute".to_string());
        }
        if args.remote_path.trim().is_empty() || !args.remote_path.starts_with('/') {
            return Err("--remote-path must be an absolute remote path".to_string());
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

fn local_mount_path(local_path: &Path) -> Result<String, String> {
    let root = if local_path.is_dir() {
        local_path
    } else {
        local_path
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", local_path.display()))?
    };
    Ok(root.to_string_lossy().into_owned())
}

fn path_size(path: &Path) -> Result<u64, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("failed to stat {}: {error}", path.display()))?;
    if metadata.is_file() {
        return Ok(metadata.len());
    }

    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .map_err(|error| format!("failed to read directory {}: {error}", dir.display()))?
        {
            let entry = entry.map_err(|error| {
                format!("failed to iterate directory {}: {error}", dir.display())
            })?;
            let metadata = entry
                .metadata()
                .map_err(|error| format!("failed to stat {}: {error}", entry.path().display()))?;
            if metadata.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Ok(total)
}

fn mib_per_second(bytes: u64, seconds: f64) -> f64 {
    if seconds <= f64::EPSILON {
        return 0.0;
    }
    bytes as f64 / 1024.0 / 1024.0 / seconds
}

fn usage() {
    eprintln!(
        "Usage: cargo run -p iris-remote-transport --release --example sftp_perf_transfer -- \\
  --host 127.0.0.1 --port 2222 --user iris --password iris \\
  --direction upload|download --local-path PATH --remote-path /upload/file [--dir] [--iterations N]"
    );
}
