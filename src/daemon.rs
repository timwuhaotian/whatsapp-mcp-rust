//! One process owns the WhatsApp session per data dir. `daemon` runs it and
//! serves MCP over a Unix socket; the default `whatsapp-mcp` command is a thin
//! stdio proxy to that socket, starting the daemon when it isn't running.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rmcp::ServiceExt;
use tokio::net::{UnixListener, UnixStream};

use crate::config::AppConfig;
use crate::mcp::McpServer;
use crate::wa::Wa;

fn socket_path(cfg: &AppConfig) -> PathBuf {
    cfg.data_dir.join("mcp.sock")
}

fn pid_path(cfg: &AppConfig) -> PathBuf {
    cfg.data_dir.join("daemon.pid")
}

fn log_path(cfg: &AppConfig) -> PathBuf {
    cfg.data_dir.join("daemon.log")
}

fn pid_alive(pid: libc::pid_t) -> bool {
    // kill(pid, 0) also succeeds for zombies (exited, not yet reaped by their
    // parent, e.g. a long-lived proxy), so ask ps and treat `Z` as gone.
    std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .map(|out| {
            let state = String::from_utf8_lossy(&out.stdout);
            let state = state.trim();
            !state.is_empty() && !state.starts_with('Z')
        })
        .unwrap_or(false)
}

/// Pid of a running daemon. Removes the pid file when its process is gone.
fn live_pid(cfg: &AppConfig) -> Option<libc::pid_t> {
    let pid: libc::pid_t = std::fs::read_to_string(pid_path(cfg))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    if pid_alive(pid) {
        Some(pid)
    } else {
        let _ = std::fs::remove_file(pid_path(cfg));
        None
    }
}

/// Fails while a daemon owns this data dir. login, logout and `doctor --connect`
/// call this: a second connection with the same device steals the session.
pub fn ensure_stopped(cfg: &AppConfig) -> Result<()> {
    if let Some(pid) = live_pid(cfg) {
        bail!("daemon is running (pid {pid}); run `whatsapp-mcp stop` first");
    }
    Ok(())
}

fn acquire_pid_file(cfg: &AppConfig) -> Result<()> {
    loop {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(pid_path(cfg))
        {
            Ok(mut f) => {
                writeln!(f, "{}", std::process::id())?;
                return Ok(());
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                if let Some(pid) = live_pid(cfg) {
                    bail!("daemon already running (pid {pid})");
                }
                // Stale file from a dead daemon: drop it and retry.
                let _ = std::fs::remove_file(pid_path(cfg));
            }
            Err(e) => return Err(e).context("create daemon pid file"),
        }
    }
}

/// Run the shared daemon in the foreground until SIGINT or SIGTERM.
pub async fn run(cfg: &AppConfig) -> Result<()> {
    acquire_pid_file(cfg)?;
    let result = run_owned(cfg).await;
    let _ = std::fs::remove_file(socket_path(cfg));
    let _ = std::fs::remove_file(pid_path(cfg));
    result
}

async fn run_owned(cfg: &AppConfig) -> Result<()> {
    let sock = socket_path(cfg);
    // A socket left by a dead daemon would make bind fail.
    let _ = std::fs::remove_file(&sock);
    let wa = Wa::open(cfg)?;
    wa.connect(cfg).await?;
    let listener = UnixListener::bind(&sock).with_context(|| format!("bind {}", sock.display()))?;
    std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600))?;
    tracing::info!("daemon listening on {}", sock.display());

    let server = McpServer::new(Arc::clone(&wa));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let server = server.clone();
                    tokio::spawn(async move {
                        let (reader, writer) = stream.into_split();
                        match server.serve((reader, writer)).await {
                            Ok(service) => {
                                let _ = service.waiting().await;
                            }
                            Err(e) => tracing::warn!("client handshake failed: {e}"),
                        }
                    });
                }
                Err(e) => tracing::warn!("accept failed: {e}"),
            },
            _ = tokio::signal::ctrl_c() => break,
            _ = terminate.recv() => break,
        }
    }
    tracing::info!("shutting down");
    let client = wa.client.read().unwrap().clone();
    if let Some(c) = client {
        c.disconnect().await;
    }
    Ok(())
}

/// Default command: forward stdio to the daemon, starting it if needed.
pub async fn proxy(cfg: &AppConfig) -> Result<()> {
    let mut sock = connect_or_start(cfg).await?;
    let mut stdio = tokio::io::join(tokio::io::stdin(), tokio::io::stdout());
    tokio::io::copy_bidirectional(&mut sock, &mut stdio).await?;
    Ok(())
}

async fn connect_or_start(cfg: &AppConfig) -> Result<UnixStream> {
    if let Ok(stream) = UnixStream::connect(socket_path(cfg)).await {
        return Ok(stream);
    }
    if live_pid(cfg).is_none() {
        spawn_daemon(cfg)?;
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match UnixStream::connect(socket_path(cfg)).await {
            Ok(stream) => return Ok(stream),
            Err(e) if Instant::now() >= deadline => bail!(
                "daemon did not start (see {}): {e}",
                log_path(cfg).display()
            ),
            Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
}

fn spawn_daemon(cfg: &AppConfig) -> Result<()> {
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(log_path(cfg))
        .context("open daemon log")?;
    std::process::Command::new(std::env::current_exe()?)
        .arg("--data-dir")
        .arg(&cfg.data_dir)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        // Own process group: the daemon survives the client that started it.
        .process_group(0)
        .spawn()
        .context("start daemon")?;
    Ok(())
}

/// `whatsapp-mcp stop`: SIGTERM the daemon and wait for it to clean up.
pub fn stop(cfg: &AppConfig) -> Result<()> {
    let Some(pid) = live_pid(cfg) else {
        println!("no daemon running");
        return Ok(());
    };
    // SAFETY: plain signal delivery to a pid we just read from our pid file.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let deadline = Instant::now() + Duration::from_secs(10);
    while pid_alive(pid) {
        if Instant::now() >= deadline {
            bail!("daemon (pid {pid}) did not exit within 10s");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = std::fs::remove_file(socket_path(cfg));
    let _ = std::fs::remove_file(pid_path(cfg));
    println!("stopped daemon (pid {pid})");
    Ok(())
}
