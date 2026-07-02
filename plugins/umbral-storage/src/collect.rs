//! The `collectstatic` management command and the bridge that routes its
//! writes through the unified [`Storage`] trait.
//!
//! Where umbral-static's command wrote through core's sync `StaticStorage`
//! trait directly (a `LocalStorage` for the filesystem, an S3 backend for
//! the bucket), the merged crate collects through the named
//! `storage_named("staticfiles")` instance instead: a
//! [`StaticStorageBridge`] adapts that async [`Storage`] onto core's sync
//! [`StaticStorage`] seam (`put(key, …)` for the exact-path write,
//! `exists` for the presence check), so the entire core collect engine
//! (`collect_into_with`: tree walk, content-hashing, `staticfiles.json`
//! manifest) is reused unchanged while the bytes land through ONE storage
//! abstraction shared with media.

use std::sync::Arc;

use umbral::static_files::{StaticError, StaticStorage};
use umbral::storage::{STATICFILES, Storage};

/// Adapts a unified async [`Storage`] onto core's synchronous
/// [`StaticStorage`] seam so `collect_into_with` can drive it.
///
/// `collect_into_with` is synchronous (it walks the source tree with
/// `std::fs` and calls `storage.put(rel_path, bytes)` per file), but the
/// unified [`Storage::put`] / [`Storage::exists`] are async. We bridge by
/// running each call to completion on the current Tokio runtime via
/// `block_in_place` + `Handle::block_on`, which is exactly how a
/// sync-over-async adapter is meant to wait without deadlocking the
/// multi-threaded runtime the command runs on.
pub(crate) struct StaticStorageBridge {
    inner: Arc<dyn Storage>,
}

impl StaticStorageBridge {
    pub(crate) fn new(inner: Arc<dyn Storage>) -> Self {
        Self { inner }
    }

    /// Run `fut` to completion from a synchronous context. The collect
    /// command runs on a multi-threaded runtime, so `block_in_place` moves
    /// this blocking wait off the async path and `block_on` drives the
    /// future on the current handle.
    fn block<T>(&self, fut: impl std::future::Future<Output = T>) -> T {
        let handle = tokio::runtime::Handle::current();
        tokio::task::block_in_place(|| handle.block_on(fut))
    }
}

impl StaticStorage for StaticStorageBridge {
    fn put(&self, rel_path: &str, bytes: &[u8]) -> Result<(), StaticError> {
        let content_type = mime_guess::from_path(rel_path)
            .first_or_octet_stream()
            .to_string();
        self.block(self.inner.put(rel_path, &content_type, bytes))
            .map(|_| ())
            .map_err(|e| StaticError::Backend(format!("put `{rel_path}` failed: {e}")))
    }

    fn exists(&self, rel_path: &str) -> Result<bool, StaticError> {
        self.block(self.inner.exists(rel_path))
            .map_err(|e| StaticError::Backend(format!("exists `{rel_path}` failed: {e}")))
    }
}

/// The `collectstatic` management command. Copies every registered
/// plugin's namespaced `static_dirs()` into `<static_root>/<namespace>/`
/// and every app/site `static_root_dirs()` into the `<static_root>/` root,
/// writing through the `"staticfiles"` storage instance.
///
/// It reads the static contributions from the ambient
/// [`umbral::static_files::published_static`] slot that `App::build`
/// populated.
pub(crate) struct CollectStaticCommand;

#[async_trait::async_trait]
impl umbral::cli::PluginCommand for CollectStaticCommand {
    fn command(&self) -> clap::Command {
        clap::Command::new("collectstatic")
            .about(
                "Collect every plugin's static_dirs() (namespaced) and static_root_dirs() \
                 (site dirs) into settings.static_root.",
            )
            .arg(
                clap::Arg::new("clear")
                    .long("clear")
                    .help(
                        "Empty static_root before collecting, dropping stale assets no plugin \
                         ships any more. No confirmation prompt.",
                    )
                    .action(clap::ArgAction::SetTrue),
            )
            .arg(
                clap::Arg::new("hashed")
                    .long("hashed")
                    .help(
                        "Write content-hashed copies (app.<hash>.css) alongside each asset and a \
                         staticfiles.json manifest. Use in \
                         PROD: the hashed filename changes when bytes do, so assets can carry \
                         far-future cache headers without stale-cache risk.",
                    )
                    .action(clap::ArgAction::SetTrue),
            )
            .arg(
                clap::Arg::new("storage")
                    .long("storage")
                    .value_name("BACKEND")
                    .help(
                        "Where to write collected assets: `local` (default, the on-disk \
                         static_root) or `s3` (upload to a bucket; requires the umbral-storage `s3` \
                         feature and UMBRAL_S3_BUCKET / UMBRAL_S3_REGION). Overrides \
                         UMBRAL_STATIC_STORAGE.",
                    ),
            )
    }

    async fn run(&self, matches: &clap::ArgMatches) -> Result<(), umbral::cli::CliError> {
        let clear = matches.get_flag("clear");
        let hashed = matches.get_flag("hashed");
        let static_root = umbral::settings::get().static_root.clone();

        // Backend selection: --storage flag, else UMBRAL_STATIC_STORAGE,
        // else local. `local` is always available; `s3` requires the
        // feature AND the bucket/region env vars.
        let backend = matches
            .get_one::<String>("storage")
            .cloned()
            .or_else(|| {
                umbral::settings::get()
                    .extra_str("static_storage")
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "local".to_string());

        let published =
            umbral::static_files::published_static().ok_or_else(|| -> umbral::cli::CliError {
                "collectstatic requires a built App; ensure App::build() ran before dispatching \
                 the command (umbral-cli::dispatch is called with the built App)."
                    .into()
            })?;

        // The unified "staticfiles" storage instance is the write target.
        // Resolve the backend the StoragePlugin registered for static
        // collection (an FsStorage by default, an S3Storage under
        // `--storage s3` / the s3 feature).
        let static_backend: Arc<dyn Storage> = match backend.as_str() {
            "local" => {
                // Local convention: ensure the root dir exists even for an
                // empty collect (a reverse proxy may point at it).
                if !(clear && std::path::Path::new(&static_root).exists()) {
                    std::fs::create_dir_all(&static_root).map_err(
                        |e| -> umbral::cli::CliError {
                            format!("collectstatic: cannot create static_root `{static_root}`: {e}")
                                .into()
                        },
                    )?;
                }
                // The "staticfiles" instance the plugin registered points
                // its FsStorage at static_root, so put("css/app.css", …)
                // writes <static_root>/css/app.css.
                umbral::storage::try_storage_named(STATICFILES).map_err(
                    |_| -> umbral::cli::CliError {
                        "collectstatic: no `staticfiles` storage backend registered; add \
                         StoragePlugin with a static side (.static_files / .embedded)."
                            .into()
                    },
                )?
            }
            "s3" => {
                #[cfg(feature = "s3")]
                {
                    let s3 =
                        crate::s3::S3Storage::from_env().map_err(|e| -> umbral::cli::CliError {
                            format!("collectstatic --storage s3: {e}").into()
                        })?;
                    Arc::new(s3) as Arc<dyn Storage>
                }
                #[cfg(not(feature = "s3"))]
                {
                    return Err(
                        "collectstatic --storage s3 requires the umbral-storage `s3` \
                                cargo feature (build with --features s3)."
                            .into(),
                    );
                }
            }
            other => {
                return Err(format!(
                    "collectstatic: unknown --storage backend `{other}` (expected `local` or \
                     `s3`)."
                )
                .into());
            }
        };

        let bridge = StaticStorageBridge::new(static_backend);
        let summary = umbral::static_files::collect_into_with(
            &published.contributions,
            &published.root_dirs,
            &static_root,
            &bridge,
            clear,
            hashed,
        )?;

        for missing in &summary.missing {
            eprintln!(
                "warning: collectstatic: plugin `{}` declares static namespace `{}` with source \
                 dir `{}`, which does not exist on disk — skipped.",
                missing.plugin,
                missing.namespace,
                missing.source_dir.display(),
            );
        }

        if summary.collected.is_empty() && summary.root_files == 0 {
            println!(
                "No static assets to collect (no plugin contributed an on-disk source or site \
                 dir)."
            );
            return Ok(());
        }

        for collected in &summary.collected {
            println!(
                "{} file(s) -> {}",
                collected.files,
                collected.destination.display(),
            );
        }
        if summary.root_files > 0 {
            println!(
                "{} site file(s) -> {}",
                summary.root_files,
                summary.static_root.display(),
            );
        }
        println!(
            "Collected {} file(s) into {}",
            summary.total_files() + summary.root_files,
            summary.static_root.display(),
        );
        Ok(())
    }
}
