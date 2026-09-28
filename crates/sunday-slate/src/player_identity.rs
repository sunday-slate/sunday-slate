use nfl_data::TeamAbbr;

pub(crate) fn normalize_name(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn normalize_team(code: &str) -> &str {
    match code {
        "JAC" => "JAX",
        "LAR" => "LA",
        other => other,
    }
}

pub(crate) fn canonical_team(team: &TeamAbbr) -> TeamAbbr {
    TeamAbbr(normalize_team(&team.0).to_owned())
}

pub(crate) fn provider_team(team: &str) -> &str {
    match team {
        "JAX" => "JAX",
        "LA" => "LAR",
        "WAS" => "WSH",
        other => other,
    }
}
