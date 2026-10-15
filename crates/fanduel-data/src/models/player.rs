/// A player identified by FanDuel's player ID and name.
#[derive(Debug, Clone, PartialEq)]
pub struct Player {
    /// FanDuel player ID.
    pub id: u32,
    /// Player's first name.
    pub first_name: String,
    /// Player's last name.
    pub last_name: String,
}
