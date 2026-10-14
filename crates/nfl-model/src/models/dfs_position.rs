#[derive(
    Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, strum::IntoStaticStr, strum::EnumString,
)]
#[sqlx(rename_all = "UPPERCASE")]
#[strum(serialize_all = "UPPERCASE", ascii_case_insensitive)]
pub enum DfsPosition {
    Qb,
    Rb,
    Wr,
    Te,
    #[strum(
        to_string = "DST",
        serialize = "DST",
        serialize = "DEF",
        serialize = "D"
    )]
    Dst,
}

impl DfsPosition {
    pub fn is_defense(&self) -> bool {
        matches!(self, DfsPosition::Dst)
    }
}

impl From<DfsPosition> for String {
    fn from(value: DfsPosition) -> Self {
        let s: &str = value.into();
        s.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_positions_case_insensitively_and_accepts_defense_aliases() {
        assert_eq!(DfsPosition::try_from("qb"), Ok(DfsPosition::Qb));
        assert_eq!(DfsPosition::try_from("RB"), Ok(DfsPosition::Rb));
        assert_eq!(DfsPosition::try_from("Wr"), Ok(DfsPosition::Wr));
        assert_eq!(DfsPosition::try_from("TE"), Ok(DfsPosition::Te));
        for alias in ["DST", "DEF", "D"] {
            assert_eq!(DfsPosition::try_from(alias), Ok(DfsPosition::Dst));
        }
        assert!(DfsPosition::try_from("CPT").is_err());
    }

    #[test]
    fn uses_dst_as_string() {
        assert_eq!(String::from(DfsPosition::Dst), "DST".to_owned())
    }
}
