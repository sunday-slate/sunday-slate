use std::fmt::{Display, Formatter};

/// An NFL team identified by its canonical abbreviation.
///
/// String parsing is ASCII case-insensitive. `JAC` is accepted as an alias for
/// [`JAX`](Self::JAX), and `LAR` as an alias for [`LA`](Self::LA). Serde deserialization also
/// accepts these uppercase aliases.
///
/// [`as_str`](Self::as_str), display formatting, Serde serialization, and SQLx storage use
/// canonical uppercase abbreviations, including `JAX` for Jacksonville and `LA` for the Rams.
///
/// # Examples
///
/// ```
/// use nfl_model::TeamAbbr;
///
/// let team: TeamAbbr = "lar".parse()?;
/// assert_eq!(team, TeamAbbr::LA);
/// assert_eq!(team.as_str(), "LA");
/// assert_eq!(team.to_string(), "LA");
/// # Ok::<(), strum::ParseError>(())
/// ```
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    sqlx::Type,
    strum::AsRefStr,
    strum::EnumString,
)]
#[allow(non_camel_case_types)]
#[serde(rename_all = "UPPERCASE")]
#[sqlx(type_name = "TEXT", rename_all = "UPPERCASE")]
#[strum(serialize_all = "UPPERCASE", ascii_case_insensitive)]
pub enum TeamAbbr {
    /// Arizona Cardinals.
    ARI,
    /// Atlanta Falcons.
    ATL,
    /// Baltimore Ravens.
    BAL,
    /// Buffalo Bills.
    BUF,
    /// Carolina Panthers.
    CAR,
    /// Chicago Bears.
    CHI,
    /// Cincinnati Bengals.
    CIN,
    /// Cleveland Browns.
    CLE,
    /// Dallas Cowboys.
    DAL,
    /// Denver Broncos.
    DEN,
    /// Detroit Lions.
    DET,
    /// Green Bay Packers.
    GB,
    /// Houston Texans.
    HOU,
    /// Indianapolis Colts.
    IND,
    /// Jacksonville Jaguars; also accepts `JAC` when parsing.
    #[serde(alias = "JAC")]
    #[strum(serialize = "JAX", serialize = "JAC")]
    JAX,
    /// Kansas City Chiefs.
    KC,
    /// Los Angeles Chargers.
    LAC,
    /// Los Angeles Rams; also accepts `LAR` when parsing.
    #[serde(alias = "LAR")]
    #[strum(serialize = "LA", serialize = "LAR")]
    LA,
    /// Las Vegas Raiders.
    LV,
    /// Miami Dolphins.
    MIA,
    /// Minnesota Vikings.
    MIN,
    /// New England Patriots.
    NE,
    /// New Orleans Saints.
    NO,
    /// New York Giants.
    NYG,
    /// New York Jets.
    NYJ,
    /// Philadelphia Eagles.
    PHI,
    /// Pittsburgh Steelers.
    PIT,
    /// Seattle Seahawks.
    SEA,
    /// San Francisco 49ers.
    SF,
    /// Tampa Bay Buccaneers.
    TB,
    /// Tennessee Titans.
    TEN,
    /// Washington Commanders.
    WAS,
}

impl TeamAbbr {
    /// Return the canonical uppercase abbreviation, using `JAX` for Jacksonville and `LA`
    /// for the Rams regardless of the alias used when parsing.
    pub fn as_str(&self) -> &str {
        match self {
            Self::JAX => "JAX",
            Self::LA => "LA",
            _ => self.as_ref(),
        }
    }
}

impl Display for TeamAbbr {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::TeamAbbr;

    #[test]
    fn parses_legacy_abbreviations_to_canonical_variants() {
        assert_eq!("JAC".parse(), Ok(TeamAbbr::JAX));
        assert_eq!("LAR".parse(), Ok(TeamAbbr::LA));
        assert_eq!(TeamAbbr::JAX.as_str(), "JAX");
        assert_eq!(TeamAbbr::LA.as_str(), "LA");
    }
}
