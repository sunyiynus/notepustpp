use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use blake3::Hasher;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask", about = "Notepust repository tasks")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Writes deterministic fixtures and their hashes.
    Fixtures {
        /// Output directory.
        #[arg(long, default_value = "tests/generated")]
        out: PathBuf,
        /// Also generate the 100 MB text and 1 GB log samples.
        #[arg(long)]
        huge: bool,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Fixtures { out, huge } => fixtures(&out, huge),
    }
}

fn fixtures(out: &Path, huge: bool) -> Result<()> {
    std::fs::create_dir_all(out)?;
    let mut manifest = String::from("name\tbytes\tblake3\n");

    write_fixture(
        out,
        "small-mixed-eol.txt",
        &mut manifest,
        |writer, hasher| {
            writer.write_all("第一行\r\nsecond line\n第三行\rcode: 中文 😀\n".as_bytes())?;
            hasher.update("第一行\r\nsecond line\n第三行\rcode: 中文 😀\n".as_bytes());
            Ok(())
        },
    )?;

    write_fixture(out, "sample-10mb.log", &mut manifest, |writer, hasher| {
        let mut lcg = 0x1234_5678u64;
        let mut line = String::new();
        for index in 0..175_000u64 {
            lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1);
            line.clear();
            line.push_str("2026-11-21T12:00:00Z INFO ");
            line.push_str(&format!("seq={index:08} token={:016x}", lcg));
            if index % 10_000 == 0 {
                line.push_str(" 中文日志行 with emoji 😀");
            }
            line.push('\n');
            writer.write_all(line.as_bytes())?;
            hasher.update(line.as_bytes());
        }
        Ok(())
    })?;

    write_fixture(out, "long-line-1mb.txt", &mut manifest, |writer, hasher| {
        write_long_line(writer, hasher, 1024 * 1024)
    })?;
    write_fixture(
        out,
        "long-line-10mb.txt",
        &mut manifest,
        |writer, hasher| write_long_line(writer, hasher, 10 * 1024 * 1024),
    )?;

    if huge {
        write_fixture(out, "sample-100mb.txt", &mut manifest, |writer, hasher| {
            let target = 100 * 1024 * 1024usize;
            let mut written = 0usize;
            let mut index = 0u64;
            while written < target {
                let line = format!(
                    "line {index:09} — 中文内容 — payload {:016x}\n",
                    index.wrapping_mul(0x9E37_79B9_7F4A_7C15)
                );
                let bytes = line.as_bytes();
                let take = bytes.len().min(target - written);
                writer.write_all(&bytes[..take])?;
                hasher.update(&bytes[..take]);
                written += take;
                index += 1;
            }
            Ok(())
        })?;

        write_fixture(out, "sample-1gb.log", &mut manifest, |writer, hasher| {
            let target = 1024u64 * 1024 * 1024;
            let mut written = 0u64;
            let mut index = 0u64;
            while written < target {
                let line = format!(
                    "2026-11-21T00:00:00.000Z INFO worker=search seq={index:010} message=\"processing file {index:010} 中文\"\n"
                );
                let bytes = line.as_bytes();
                writer.write_all(bytes)?;
                hasher.update(bytes);
                written += bytes.len() as u64;
                index += 1;
            }
            Ok(())
        })?;
    }

    std::fs::write(out.join("manifest.tsv"), manifest)?;
    println!("fixtures written to {}", out.display());
    println!("{}", std::fs::read_to_string(out.join("manifest.tsv"))?);
    Ok(())
}

fn write_fixture(
    out: &Path,
    name: &str,
    manifest: &mut String,
    write: impl FnOnce(&mut BufWriter<File>, &mut Hasher) -> Result<()>,
) -> Result<()> {
    let path = out.join(name);
    let file = File::create(&path).with_context(|| format!("create {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    let mut hasher = Hasher::new();
    write(&mut writer, &mut hasher)?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    drop(writer);
    let bytes = std::fs::metadata(&path)?.len();
    let digest = hasher.finalize();
    manifest.push_str(&format!("{name}\t{bytes}\t{digest}\n"));
    Ok(())
}

fn write_long_line(writer: &mut BufWriter<File>, hasher: &mut Hasher, length: usize) -> Result<()> {
    let unit = "abcdefghij0123456789中😀";
    let bytes = unit.as_bytes();
    let mut written = 0usize;
    while written < length {
        let mut take = bytes.len().min(length - written);
        while take > 0 && !unit.is_char_boundary(take) {
            take -= 1;
        }
        writer.write_all(&bytes[..take])?;
        hasher.update(&bytes[..take]);
        written += take;
        if take == 0 {
            // Finish the exact requested byte count with ASCII if a partial
            // multi-byte character would otherwise be cut.
            let fill = length - written;
            let filler = vec![b'a'; fill];
            writer.write_all(&filler)?;
            hasher.update(&filler);
            written = length;
        }
    }
    Ok(())
}
