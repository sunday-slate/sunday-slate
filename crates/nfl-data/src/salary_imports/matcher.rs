use nfl_model::{Player, WeeklyRosterEntry};
use std::collections::HashMap;

fn normalize(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) struct PlayerIndex {
    by_norm: HashMap<String, Vec<Player>>,
}
impl PlayerIndex {
    pub(crate) fn build(players: &[Player]) -> Self {
        let mut by_norm: HashMap<String, Vec<Player>> = HashMap::new();
        for player in players {
            by_norm
                .entry(normalize(&player.full_name))
                .or_default()
                .push(player.clone());
        }
        Self { by_norm }
    }
    pub(crate) fn match_one(&self, name: &str, team: &str) -> Option<String> {
        let hits = self.by_norm.get(&normalize(name))?;
        match hits.as_slice() {
            [] => None,
            [p] => Some(p.gsis_id.clone()),
            many => {
                let mut teams = many
                    .iter()
                    .filter(|p| p.latest_team.as_ref().map(|t| t.0.as_str()) == Some(team));
                let first = teams.next()?;
                if teams.next().is_none() {
                    Some(first.gsis_id.clone())
                } else {
                    None
                }
            }
        }
    }
}

const SUFFIXES: &[&str] = &["jr", "sr", "ii", "iii", "iv", "v"];
fn strip_suffix(name: &str) -> String {
    let normalized = normalize(name);
    let mut parts: Vec<&str> = normalized.split(' ').collect();
    while parts.len() > 1 && SUFFIXES.contains(parts.last().unwrap()) {
        parts.pop();
    }
    parts.join(" ")
}
struct RosterHit {
    id: String,
    position: Option<String>,
}
pub(crate) struct RosterIndex {
    entries: Vec<(String, String, RosterHit)>,
}
impl RosterIndex {
    pub(crate) fn build(roster: &[WeeklyRosterEntry]) -> Self {
        let entries = roster
            .iter()
            .filter_map(|entry| {
                Some((
                    entry.team.0.clone(),
                    strip_suffix(&entry.full_name),
                    RosterHit {
                        id: entry.gsis_id.clone()?,
                        position: entry.position.clone(),
                    },
                ))
            })
            .collect();
        Self { entries }
    }
    pub(crate) fn match_one(&self, name: &str, team: &str, position: &str) -> Option<String> {
        let wanted = normalize(name);
        let stem = strip_suffix(name);
        for key in [&wanted, &stem] {
            let hits: Vec<&RosterHit> = self
                .entries
                .iter()
                .filter(|(t, n, _)| t == team && n == key)
                .map(|(_, _, h)| h)
                .collect();
            if let Some(found) = choose(&hits, position) {
                return Some(found);
            }
        }
        None
    }
    pub(crate) fn suggest_one(&self, name: &str, team: &str, position: &str) -> Option<String> {
        let normalized = strip_suffix(name);
        let surname = normalized.rsplit(' ').next().unwrap_or_default();
        let mut hits = self.entries.iter().filter(|(t, stem, hit)| {
            t == team
                && stem.rsplit(' ').next() == Some(surname)
                && hit.position.as_deref() == Some(position)
        });
        let first = &hits.next()?.2.id;
        if hits.next().is_none() {
            Some(first.clone())
        } else {
            None
        }
    }
}
fn choose(hits: &[&RosterHit], position: &str) -> Option<String> {
    match hits {
        [] => None,
        [hit] => Some(hit.id.clone()),
        many => {
            let mut selected = many
                .iter()
                .filter(|hit| hit.position.as_deref() == Some(position));
            let first = selected.next()?;
            if selected.next().is_none() {
                Some(first.id.clone())
            } else {
                None
            }
        }
    }
}
