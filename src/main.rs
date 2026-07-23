#[cfg(target_os = "macos")]
mod macos;

fn main() -> anyhow::Result<()> {
    if let Some(result) = static_stream::updates::run_installer_from_args() {
        return result;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "static_stream=info".into()),
        )
        .with_target(false)
        .compact()
        .init();

    #[cfg(target_os = "macos")]
    {
        macos::run()
    }

    #[cfg(not(target_os = "macos"))]
    {
        anyhow::bail!("the desktop shell is currently implemented for macOS")
    }
}
