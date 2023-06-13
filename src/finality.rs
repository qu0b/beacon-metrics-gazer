use serde::{Serialize, Deserialize};
use anyhow::{Result, Error};

#[derive(Debug, Deserialize, Serialize)]
pub struct FinalityCheckpointResposne {
    execution_optimistic: bool,
    finalized: bool,
    data: Checkpoints,
}

#[derive(Debug, Deserialize, Serialize)]
struct Checkpoints {
    previous_justified: CheckpointData,
    current_justified: CheckpointData,
    finalized: CheckpointData,
}

#[derive(Debug, Deserialize, Serialize)]
struct CheckpointData {
    epoch: String,
    root: String,
}


pub async fn fetch_checkpoint_finality(url: &str, state_id: &str) -> Result<FinalityCheckpointResposne, Error> {
    let response = reqwest::get(format!("{url}/eth/v1/beacon/states/{state_id}/finality_checkpoints")).await?;
    let data: FinalityCheckpointResposne = response.json().await?;
    Ok(data)
}
