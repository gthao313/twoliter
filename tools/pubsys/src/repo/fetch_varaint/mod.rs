//! The fetch_variant module owns the 'fetch-variant' subcommand and provides methods for fetching
//! a given variant and download its image targets.

use crate::repo::{error as repo_error, repo_urls};
use crate::{repo, Args};
use clap::Parser;
use futures::TryStreamExt;
use futures::{stream, StreamExt};
use log::{info, trace};
use pubsys_config::InfraConfig;
use snafu::{OptionExt, ResultExt};
use std::io::{ErrorKind, Read};
use std::path::PathBuf;
use tokio_util::compat::FuturesAsyncReadCompatExt;
use tokio_util::io::SyncIoBridge;
use tough::{Repository, RepositoryLoader};
use url::Url;

/// fetching and downdloaing the image targets of a given variant
#[derive(Debug, Parser)]
pub(crate) struct FetchVariantArgs {
    #[arg(long)]
    /// Use this named repo infrastructure from Infra.toml
    repo: String,

    #[arg(long)]
    /// The architecture of the repo being validated
    arch: String,

    #[arg(long)]
    /// The variant of the repo being validated
    variant: String,

    #[arg(long)]
    /// Path to root.json for this repo
    root_role_path: PathBuf,

    #[arg(long)]
    /// Where to store the downloaded img files
    outdir: PathBuf,

    #[arg(long)]
    /// The varaint name witout extension
    buildsys_name_friendly: PathBuf,
}

async fn download_target(repo: Repository, target: &str, outdir: PathBuf) -> Result<(), Error> {
    let file_path = outdir.join(target);
    let target = target
        .try_into()
        .context(error::TargetNameSnafu { target })?;
    let stream = match repo.read_target(&target).await {
        Ok(Some(stream)) => stream,
        Ok(None) => {
            return error::TargetMissingSnafu {
                target: target.raw(),
            }
            .fail()
        }
        Err(e) => {
            return Err(e).context(error::TargetReadSnafu {
                target: target.raw(),
            })
        }
    };

    // Convert the stream to a blocking Read object.
    let mapped_err = stream.map(|next| next.map_err(|e| std::io::Error::new(ErrorKind::Other, e)));
    let lz4_async_read = mapped_err.into_async_read().compat();
    let lz4_bytes = SyncIoBridge::new(lz4_async_read);

    let mut reader = lz4::Decoder::new(lz4_bytes).context(error::Lz4DecodeSnafu {
        target: target.raw(),
    })?;

    // write the image file to the outdir
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&file_path)
        .context(error::OpenImageFileSnafu { path: file_path })?;
    std::io::copy(&mut reader, &mut file).context(error::WriteUpdateSnafu)?;
    Ok(())
}

async fn fetch_varaint(
    root_role_path: &PathBuf,
    metadata_url: Url,
    targets_url: &Url,
    outdir: PathBuf,
    buildsys_name_friendly: &str,
) -> Result<(), Error> {
    // Load the repository
    let repo = RepositoryLoader::new(
        &repo::root_bytes(root_role_path).await?,
        metadata_url.clone(),
        targets_url.clone(),
    )
    .load()
    .await
    .context(repo_error::RepoLoadSnafu {
        metadata_base_url: metadata_url.clone(),
    })?;

    let target = format!("{}.img.lz4", buildsys_name_friendly);

    // Retrieve the targets and download them
    download_target(repo, &target, outdir).await?;

    Ok(())
}

/// Common entrypoint from main()
pub(crate) async fn run(args: &Args, fetch_varaint_args: &FetchVariantArgs) -> Result<(), Error> {
    // If a lock file exists, use that, otherwise use Infra.toml
    let infra_config = InfraConfig::from_path_or_lock(&args.infra_config_path, false)
        .context(repo_error::ConfigSnafu)?;
    trace!("Parsed infra config: {:?}", infra_config);
    let repo_config = infra_config
        .repo
        .as_ref()
        .context(repo_error::MissingConfigSnafu {
            missing: "repo section",
        })?
        .get(&fetch_varaint_args.repo)
        .context(repo_error::MissingConfigSnafu {
            missing: format!("definition for repo {}", &fetch_varaint_args.repo),
        })?;

    let repo_urls = repo_urls(
        repo_config,
        &fetch_varaint_args.variant,
        &fetch_varaint_args.arch,
    )?
    .context(repo_error::MissingRepoUrlsSnafu {
        repo: &fetch_varaint_args.repo,
    })?;

    fetch_varaint(
        &fetch_varaint_args.root_role_path,
        repo_urls.0,
        &repo_urls.1,
        fetch_varaint_args.outdir.clone(),
        fetch_varaint_args.buildsys_name_friendly.to_str().unwrap(),
    )
    .await
}

mod error {
    use snafu::{Backtrace, Snafu};
    use std::path::PathBuf;

    #[derive(Debug, Snafu)]
    #[snafu(visibility(pub(super)))]
    pub(crate) enum Error {
        #[snafu(display("Failed to decode LZ4-compressed target {}: {}", target, source))]
        Lz4Decode {
            target: String,
            source: std::io::Error,
            backtrace: Backtrace,
        },

        #[snafu(display("Failed writing update data to file: {}", source))]
        WriteUpdate {
            source: std::io::Error,
            backtrace: Backtrace,
        },

        #[snafu(display("Failed to open image file path {}: {}", path.display(), source))]
        OpenImageFile {
            path: PathBuf,
            source: std::io::Error,
        },

        #[snafu(context(false), display("{}", source))]
        Repo {
            #[snafu(source(from(crate::repo::Error, Box::new)))]
            source: Box<crate::repo::Error>,
        },

        #[snafu(display("Error reading bytes from stream: {}", source))]
        Stream { source: tough::error::Error },

        #[snafu(display("Missing target: {}", target))]
        TargetMissing { target: String },

        #[snafu(display("Invalid target name '{}': {}", target, source))]
        TargetName {
            target: String,
            #[snafu(source(from(tough::error::Error, Box::new)))]
            source: Box<tough::error::Error>,
        },

        #[snafu(display("Failed to read target '{}' from repo: {}", target, source))]
        TargetRead {
            target: String,
            #[snafu(source(from(tough::error::Error, Box::new)))]
            source: Box<tough::error::Error>,
        },
    }
}
pub(crate) use error::Error;
