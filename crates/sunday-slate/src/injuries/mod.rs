//! Injury-badge presentation. nfl-data attaches a designation to each
//! salaried row; this module only maps it to the badge the pages render.

use nfl_data::InjuryDesignation;

pub struct InjuryChip {
    pub code: &'static str,
    pub title: &'static str,
    pub class: &'static str,
}

pub fn chip(designation: InjuryDesignation) -> InjuryChip {
    match designation {
        InjuryDesignation::Questionable => InjuryChip {
            code: "Q",
            title: "Questionable",
            class: "badge-warning badge-outline badge-xs",
        },
        InjuryDesignation::Doubtful => InjuryChip {
            code: "D",
            title: "Doubtful",
            class: "badge-error badge-xs",
        },
        InjuryDesignation::Out => InjuryChip {
            code: "O",
            title: "Out",
            class: "badge-error badge-xs",
        },
        InjuryDesignation::InjuredReserve => InjuryChip {
            code: "IR",
            title: "Injured Reserve",
            class: "badge-error badge-xs",
        },
    }
}
