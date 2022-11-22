pub mod args;
mod http;
mod lnd;
mod lsp;
pub mod scheduler;

use log::info;
use scheduler::Scheduler;

use crate::args::parse_args;

#[macro_use]
extern crate serde_derive;
extern crate configure_me;

configure_me::include_config!();

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init_from_env(
        env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "info"),
    );
    let (config, args) =
        Config::including_optional_config_files(std::iter::empty::<&str>()).unwrap_or_exit();

    let channel_batch = parse_args(args).expect("failed to parse remaining arguments");

    let scheduler = Scheduler::from_config(&config).await?;

    if let Some(batch) = channel_batch {
        let (bip21, _, _) = scheduler.schedule_payjoin(batch).await?;
        info!("{}", bip21);
    }

    let (tx, _) = tokio::sync::broadcast::channel(16);
    let bind_addr = (config.bind_ip, config.bind_port).into();
    http::serve(scheduler, tx, bind_addr).await?;

    Ok(())
}
