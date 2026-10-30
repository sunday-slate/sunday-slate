use sqlx::Error;
use nfl_model::DfsPositionSlotType;
use crate::NflData;
use crate::salaried_players::SalariedPlayer;
use crate::slates::SlateId;

pub async fn for_position(nfl: &NflData, slate_id: SlateId, pos: DfsPositionSlotType) -> Result<Vec<SalariedPlayer>, Error> {
       Ok(vec![])
}