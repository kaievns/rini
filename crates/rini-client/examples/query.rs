use std::error::Error;

use rini_client::RiniMachClient;

fn main() -> Result<(), Box<dyn Error>> {
    let client = RiniMachClient::connect()?;
    let data = client.get_workspaces(None)?;

    println!("{}", serde_json::to_string_pretty(&data)?);
    Ok(())
}
