// SPDX-License-Identifier: GPL-3.0-or-later

//! Single-view viewpoint selection logic, GTK-free: which local POVs a
//! correlated activity offers, how they are labelled, and which one to open.
//!
//! Multi-POV grid playback (synchronized 2–4 player grid, drift correction)
//! was removed from the product by maintainer decision (2026-07-22).

use warcraft_recorder::domain::{LibraryEntry, RecordingId};

use super::filters::spec_name;

/// One selectable local viewpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pov {
    pub id: RecordingId,
    /// "Player (Spec)" when known, else the entry title.
    pub label: String,
    /// The plain player name, the key `preferredViewpoint` remembers.
    pub player: Option<String>,
}

/// Distinct local POVs for a correlated activity, baseline-sorted: primary
/// first, then the correlated order, deduplicated by player label.
pub fn povs(entries: &[&LibraryEntry]) -> Vec<Pov> {
    let mut out: Vec<Pov> = Vec::new();
    for entry in entries {
        let player = entry
            .player
            .as_ref()
            .filter(|player| !player.name.is_empty())
            .map(|player| player.name.split('-').next().unwrap_or("").to_owned());
        let label = match (&player, entry.player.as_ref().and_then(|p| p.spec_id)) {
            (Some(name), Some(spec)) => match spec_name(spec) {
                Some(spec) => format!("{name} ({spec})"),
                None => name.clone(),
            },
            (Some(name), None) => name.clone(),
            _ => entry.title.clone(),
        };
        if out.iter().any(|existing| existing.label == label) {
            continue;
        }
        out.push(Pov {
            id: entry.id.clone(),
            label,
            player,
        });
    }
    out
}

/// The POV to open for a selection: the remembered preferred player when it
/// exists among these POVs, else the first (the primary/default).
pub fn choose<'a>(povs: &'a [Pov], preferred_player: Option<&str>) -> Option<&'a Pov> {
    preferred_player
        .and_then(|name| povs.iter().find(|pov| pov.player.as_deref() == Some(name)))
        .or_else(|| povs.first())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use warcraft_recorder::domain::{
        ActivityDetails, Category, Codec, GameFlavor, MediaFacts, Outcome, PlayerSummary,
    };

    fn entry(name: Option<(&str, u16)>, title: &str) -> LibraryEntry {
        LibraryEntry {
            id: RecordingId::new(),
            media_path: PathBuf::from("/rec/v.mkv"),
            sidecar_path: PathBuf::from("/rec/v.json"),
            category: Category::Raids,
            flavor: GameFlavor::Retail,
            title: title.to_owned(),
            start_unix_ms: 0,
            duration_ms: 60_000,
            outcome: Outcome::Win,
            protected: false,
            tag: None,
            activity_hash: Some("hash".to_owned()),
            player: name.map(|(name, spec)| PlayerSummary {
                name: name.to_owned(),
                realm: None,
                guid: None,
                class_id: None,
                spec_id: Some(spec),
            }),
            combatants: Vec::new(),
            details: ActivityDetails::Raid {
                zone_id: None,
                zone_name: None,
                encounter_id: None,
                encounter_name: None,
                difficulty_id: None,
                difficulty: None,
                pull: None,
                boss_percent: None,
            },
            timeline: Vec::new(),
            media: MediaFacts {
                fps: None,
                width: None,
                height: None,
                codec: Some(Codec::H264),
            },
        }
    }

    #[test]
    fn povs_are_labelled_deduplicated_and_preferred_selection_wins() {
        let alice = entry(Some(("Alice-Realm", 64)), "a");
        let bob = entry(Some(("Bob-Realm", 577)), "b");
        let alice_again = entry(Some(("Alice-Other", 64)), "c");
        let unnamed = entry(None, "Manual title");
        let list = povs(&[&alice, &bob, &alice_again, &unnamed]);
        let labels: Vec<&str> = list.iter().map(|pov| pov.label.as_str()).collect();
        assert_eq!(labels, vec!["Alice (Frost)", "Bob (Havoc)", "Manual title"]);

        // Preferred player is remembered by plain name; unknown falls back to
        // the first POV.
        assert_eq!(choose(&list, Some("Bob")).unwrap().label, "Bob (Havoc)");
        assert_eq!(choose(&list, Some("Carol")).unwrap().label, "Alice (Frost)");
        assert_eq!(choose(&list, None).unwrap().label, "Alice (Frost)");
    }
}
