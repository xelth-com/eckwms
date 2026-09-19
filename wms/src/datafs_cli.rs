//! `wms datafs …` — offline management of the encrypted `data/` volume
//! (`.eck/DATA_AT_REST.md`).
//!
//! These subcommands exist so that **no shell script and no operator ever
//! handles the volume key**. They load `.env`, bring up the key ring exactly the
//! way a normal boot does (safe on eck1/2/3 → keyfile → nothing), derive
//! `HKDF(TMK, "datafs", epoch)` in RAM and hand the derived passphrase to
//! gocryptfs over a pipe. Nothing is written to disk, argv or the environment.
//!
//! ```text
//! wms datafs status                                  # is it configured / mounted?
//! wms datafs init    [--cipher DIR]                  # create an empty volume
//! wms datafs mount   [--cipher DIR] [--mount DIR]    # mount it here and now
//! wms datafs unmount [--mount DIR]                   # fusermount3 -u
//! wms datafs migrate --from data --cipher data.enc   # copy + verify, no deletes
//! ```
//!
//! `--cipher` / `--mount` default to `ECK_DATAFS_CIPHER_DIR` / `ECK_DATAFS_MOUNT`
//! from the environment, so on a configured node the flags are optional.
//!
//! Exit codes: `0` ok, `1` operation failed, `2` usage error, `78` (EX_CONFIG)
//! no key available.

use std::path::PathBuf;

use eck_core::vault::datafs;

const USAGE: &str = "\
usage: wms datafs <command> [options]

commands:
  status                       report {enabled, mounted, cipher_dir, mount}
  init      [--cipher DIR]     gocryptfs -init a new (empty) cipher dir
  mount     [--cipher DIR] [--mount DIR]
  unmount   [--mount DIR]
  migrate   --from DIR [--cipher DIR]
                               init + mount a temp view + cp -a + verify + unmount.
                               NEVER deletes or modifies the source tree.

options default to ECK_DATAFS_CIPHER_DIR / ECK_DATAFS_MOUNT from .env.
";

struct Args {
    cmd: String,
    from: Option<PathBuf>,
    cipher: Option<PathBuf>,
    mount: Option<PathBuf>,
}

fn parse(argv: &[String]) -> Result<Args, String> {
    let Some(cmd) = argv.first().cloned() else {
        return Err(USAGE.to_string());
    };
    let mut a = Args {
        cmd,
        from: None,
        cipher: None,
        mount: None,
    };
    let mut i = 1;
    while i < argv.len() {
        let take = |i: usize| -> Result<PathBuf, String> {
            argv.get(i + 1)
                .map(PathBuf::from)
                .ok_or_else(|| format!("{} needs a value", argv[i]))
        };
        match argv[i].as_str() {
            "--from" => a.from = Some(take(i)?),
            "--cipher" => a.cipher = Some(take(i)?),
            "--mount" => a.mount = Some(take(i)?),
            "-h" | "--help" => return Err(USAGE.to_string()),
            other => return Err(format!("unknown option {other}\n\n{USAGE}")),
        }
        i += 2;
    }
    Ok(a)
}

fn cipher_dir(a: &Args) -> Result<PathBuf, String> {
    a.cipher
        .clone()
        .or_else(|| datafs::config().map(|c| c.cipher_dir))
        .ok_or_else(|| format!("no cipher dir: pass --cipher or set {}", datafs::ENV_CIPHER_DIR))
}

fn mount_point(a: &Args) -> PathBuf {
    a.mount.clone().unwrap_or_else(datafs::mount_dir)
}

/// Entry point from `fn main`. Returns the process exit code.
pub fn main(argv: &[String]) -> i32 {
    // Console-only tracing: these commands run interactively and must not create
    // a log directory inside the very volume they are about to mount.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let args = match parse(argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };

    // Same config source as a boot: .env for the paths, the safe for the key.
    let _ = dotenvy::dotenv();

    if args.cmd == "status" {
        println!("{}", datafs::status_json());
        return 0;
    }

    // Everything else needs the volume key, which needs the key ring, which
    // needs the tokio runtime (the safe fetch is async).
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("runtime: {e}");
            return 1;
        }
    };
    // Use the node's existing identity, but NEVER mint one: `ensure_uuid_…`
    // writes a fresh INSTANCE_ID into `.env` as a side effect, and a maintenance
    // command must not give the node a new identity (nor litter `.env` when run
    // from a scratch directory). With no INSTANCE_ID there is no safe fetch to
    // make anyway — the key then has to come from ECK_DATAFS_KEYFILE.
    let raw = std::env::var("INSTANCE_ID").unwrap_or_default();
    let instance_id = if raw.trim().is_empty() {
        String::new()
    } else {
        eck_core::utils::identity::ensure_uuid_instance_id(&raw)
    };
    rt.block_on(eck_core::keyring::init(&reqwest::Client::new(), &instance_id));

    let key = match eck_core::keyring::datafs_key() {
        Some(k) => k,
        None => {
            eprintln!(
                "FATAL: no data-at-rest key (keyring source = {}). Enrol this node in the \
                 safe (ECK_VAULT_NODES + ECK_VAULT_ENROLL_CODE) or set ECK_DATAFS_KEYFILE.",
                eck_core::keyring::source().as_str()
            );
            return 78;
        }
    };

    let r = match args.cmd.as_str() {
        "init" => cipher_dir(&args).and_then(|c| {
            datafs::init_if_needed(&c, &key).map(|created| {
                println!(
                    "{}: {}",
                    c.display(),
                    if created { "initialised" } else { "already initialised" }
                );
            })
        }),
        "mount" => cipher_dir(&args).and_then(|c| {
            let m = mount_point(&args);
            // Never stack a second gocryptfs on the same mountpoint: it would
            // hide the first and leave an orphan daemon behind.
            match datafs::mount_state_of(&c, &m) {
                datafs::MountState::Ours => {
                    println!("already mounted: {} -> {}", c.display(), m.display());
                    Ok(())
                }
                datafs::MountState::Foreign(src) => Err(format!(
                    "{} already carries a gocryptfs mount of {src} — unmount it first",
                    m.display()
                )),
                datafs::MountState::Absent => datafs::mount(&c, &m, &key).map(|_| {
                    println!("mounted {} -> {}", c.display(), m.display());
                }),
            }
        }),
        "unmount" => {
            datafs::unmount_best_effort(&mount_point(&args));
            Ok(())
        }
        "migrate" => match (args.from.clone(), cipher_dir(&args)) {
            (None, _) => Err("migrate needs --from <plaintext dir>".to_string()),
            (Some(from), Ok(cipher)) => datafs::migrate(&from, &cipher, &key).and_then(|rep| {
                println!(
                    "copied {} dirs / {} files / {} bytes; sha256-verified {} files ({} bytes)",
                    rep.dirs, rep.files, rep.bytes, rep.sampled, rep.sampled_bytes
                );
                if rep.ok() {
                    println!("VERIFY OK — the encrypted volume matches {}", from.display());
                    println!(
                        "\nNext (operator, service stopped):\n  \
                         mv {f} {f}.plain-$(date +%F)\n  mkdir -m 700 {f}\n  \
                         # then set {c}={cd} in .env and start the service.\n  \
                         # KEEP the .plain-* copy until the node has booted clean.",
                        f = from.display(),
                        c = datafs::ENV_CIPHER_DIR,
                        cd = cipher.display()
                    );
                    Ok(())
                } else {
                    for p in &rep.problems {
                        eprintln!("  PROBLEM: {p}");
                    }
                    Err(format!(
                        "verification FAILED ({} problems) — do NOT swap the directories",
                        rep.problems.len()
                    ))
                }
            }),
            (Some(_), Err(e)) => Err(e),
        },
        other => Err(format!("unknown command {other}\n\n{USAGE}")),
    };

    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("ERROR: {e}");
            1
        }
    }
}
