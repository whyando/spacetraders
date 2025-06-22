use prettytable::{Table, format, row};
use st::api_client::ApiClient;
use st::database::DbClient;
use st::universe::Universe;
use std::env;
use std::fs::File;
use std::io;
use std::sync::Arc;

#[tokio::main]
async fn main() -> io::Result<()> {
    dotenvy::dotenv().ok();
    pretty_env_logger::init_timed();

    let callsign = env::var("AGENT_CALLSIGN").expect("AGENT_CALLSIGN env var not set");

    let api_client = ApiClient::new(vec![]);
    let status = api_client.status().await.1.unwrap();
    let db = DbClient::new(&status.reset_date).await;
    let universe = Arc::new(Universe::new(&api_client, &db).await);

    let agent = api_client.get_agent_public(&callsign).await;
    let system_symbol = agent.headquarters.system();

    let markets = universe.get_system_markets(&system_symbol).await;

    // output to ./markets.txt
    let mut f = File::options()
        .write(true)
        .create(true)
        .truncate(true)
        .open("markets.txt")
        .unwrap();
    use std::io::Write as _;

    for (market_remote, market_opt) in markets {
        if let Some(market) = market_opt {
            writeln!(&mut f, "Market: {}", market.data.symbol)?;

            let mut table = Table::new();
            table.set_format(*format::consts::FORMAT_NO_LINESEP_WITH_TITLE);
            table.add_row(row![
                "Symbol",
                "Type",
                "Supply",
                "Activity",
                "Volume",
                "Buy Price",
                "Sell Price"
            ]);

            for trade_good in &market.data.trade_goods {
                let activity = match &trade_good.activity {
                    Some(x) => x.to_string(),
                    None => "".to_string(),
                };
                table.add_row(row![
                    trade_good.symbol,
                    trade_good._type,
                    trade_good.supply,
                    activity,
                    trade_good.trade_volume,
                    format!("${}", trade_good.purchase_price),
                    format!("${}", trade_good.sell_price)
                ]);
            }

            writeln!(&mut f, "{}", table)?;
            writeln!(&mut f)?;
        } else {
            writeln!(
                &mut f,
                "Market: {} (prices not available)",
                market_remote.symbol
            )?;

            let mut table = Table::new();
            table.set_format(*format::consts::FORMAT_NO_LINESEP_WITH_TITLE);
            table.add_row(row!["Symbol", "Type",]);

            for good in &market_remote.imports {
                table.add_row(row![good.symbol, "Import"]);
            }
            for good in &market_remote.exports {
                table.add_row(row![good.symbol, "Export"]);
            }
            for good in &market_remote.exchange {
                table.add_row(row![good.symbol, "Exchange"]);
            }

            writeln!(&mut f, "{}", table)?;
            writeln!(&mut f)?;
        }
    }
    log::info!("Wrote markets to markets.txt");
    Ok(())
}
