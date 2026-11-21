mod line_ops;
mod syntax;
mod theme;
mod workspace;

#[cfg(test)]
mod editor_eol_tests;

use std::path::PathBuf;

use clap::Parser;
use gpui_kit::*;
use notepust_platform::{Claim, ForwardedOpen};
use tracing_subscriber::EnvFilter;
use workspace::Workspace;

#[derive(Parser, Debug)]
#[command(
    name = "notepust",
    version,
    about = "A fast, reliable Windows text, code and log editor"
)]
struct Cli {
    /// Files to open.
    files: Vec<PathBuf>,

    /// One-based line to reveal in the first opened file.
    #[arg(long)]
    line: Option<u64>,

    /// One-based grapheme column to reveal.
    #[arg(long)]
    column: Option<u64>,

    /// Open a separate window instead of forwarding to the running instance.
    #[arg(long)]
    new_window: bool,

    /// Register per-user file associations (Explorer "Open with").
    #[arg(long)]
    register_file_types: bool,

    /// Remove the per-user file associations registered by this app.
    #[arg(long)]
    unregister_file_types: bool,
}

fn main() {
    let _log_guard = init_logging();
    let cli = Cli::parse();

    if cli.register_file_types {
        match notepust_platform::register_file_associations() {
            Ok(report) => println!(
                "registered {} for {} extensions",
                report.prog_id, report.extensions
            ),
            Err(error) => {
                eprintln!("文件关联注册失败: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    if cli.unregister_file_types {
        match notepust_platform::unregister_file_associations() {
            Ok(()) => println!("file associations removed"),
            Err(error) => {
                eprintln!("文件关联移除失败: {error}");
                std::process::exit(1);
            }
        }
        return;
    }

    let files = cli.files.clone();
    let line = cli.line;
    let column = cli.column;
    let request = ForwardedOpen {
        paths: files
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
        line,
        column,
    };
    let mut forwarded = None;
    let mut _instance_guard = None;
    if !cli.new_window {
        match notepust_platform::claim("Notepust.SingleInstance.v1", &request) {
            Ok(Claim::Primary { receiver, guard }) => {
                forwarded = Some(receiver);
                _instance_guard = Some(guard);
            }
            Ok(Claim::Forwarded) => return,
            Err(error) => {
                tracing::warn!(%error, "single-instance forwarding unavailable");
                eprintln!("单实例转发不可用，将以新窗口启动: {error}");
            }
        }
    }

    application().run(move |cx| {
        gpui_kit::init(cx);
        let options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("Notepust".into()),
                appears_transparent: false,
                traffic_light_position: None,
            }),
            ..Default::default()
        };
        let files = files.clone();
        let forwarded = forwarded;
        let opened = open_window(options, cx, move |window, cx| {
            let workspace = cx.new(|cx| Workspace::new(window, cx, files.clone(), forwarded));
            let weak = workspace.downgrade();
            window.on_window_should_close(cx, move |_window, cx| {
                weak.update(cx, |workspace, cx| workspace.write_final_checkpoint(cx))
                    .unwrap_or(true)
            });
            workspace
        });

        match opened {
            Ok((window_handle, workspace)) => {
                if let Some(line) = line {
                    let column = column.unwrap_or(1);
                    window_handle
                        .update(cx, |_root, window, cx| {
                            workspace.update(cx, |workspace, cx| {
                                workspace.set_initial_goto(line, column, window, cx);
                            });
                        })
                        .ok();
                }
            }
            Err(error) => {
                tracing::error!(%error, "could not open the main window");
                eprintln!("Notepust could not open its window: {error}");
            }
        }
    });
}

fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let directory = notepust_platform::resolve_profile()
        .map(|profile| profile.log_dir())
        .unwrap_or_else(|_| PathBuf::from("."));
    if std::fs::create_dir_all(&directory).is_err() {
        return None;
    }
    let appender = tracing_appender::rolling::daily(&directory, "notepust.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .try_init();
    Some(guard)
}
