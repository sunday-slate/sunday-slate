use nfl_model::TeamAbbr;

/// How a page names one pick: the display facts every consumer of a gsis id
/// needs, resolved from whichever source carries the player.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerIdentity {
    pub name: String,
    pub team: Option<TeamAbbr>,
    pub position: Option<String>,
    /// Player photo URL from the `players` release. `None` for picks named
    /// only from weekly rosters (that source carries no headshot).
    pub headshot_url: Option<String>,
}
