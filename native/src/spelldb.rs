// SPDX-License-Identifier: GPL-3.0-or-later

//! The bundled spell database powering damage-meter icons and tooltips.
//!
//! `data/spells/spells.json` is a name-keyed map produced by
//! `scripts/fetch-spell-data.py`:
//!
//! ```json
//! { "Fireball": ["Throws a fiery ball...", "spell_fire_flamebolt"], ... }
//! ```
//!
//! Each value is `[description, icon_basename]`. Rank variants fold onto
//! their base spell's entry, so a lookup by the name a combat log reports
//! resolves to the right icon and tooltip. Spells without a bundled entry
//! simply render without an icon or tooltip. No file I/O here: the caller
//! reads the resource and hands the JSON text to [`SpellDb::parse`].

use std::collections::HashMap;

use serde::Deserialize;

/// One spell's tooltip facts. Boxed strings: the index holds ~87k entries
/// for the process lifetime, so spare capacity words add up.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(from = "(Box<str>, Box<str>)")]
pub struct SpellInfo {
    pub description: Box<str>,
    /// Icon basename, e.g. `spell_fire_flamebolt`; the PNG lives at
    /// `/io/github/JohanWes/WarcraftRecorder/spells/{icon}.png`.
    pub icon: Box<str>,
}

impl From<(Box<str>, Box<str>)> for SpellInfo {
    fn from((description, icon): (Box<str>, Box<str>)) -> Self {
        Self { description, icon }
    }
}

/// An immutable spell-name index built once from the bundled JSON.
#[derive(Clone, Debug, Default)]
pub struct SpellDb {
    by_name: HashMap<Box<str>, SpellInfo>,
}

impl SpellDb {
    /// Parse the bundled spell JSON (`{name: [description, icon]}`) straight
    /// into the index.
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        Ok(Self {
            by_name: serde_json::from_str(json)?,
        })
    }

    /// The entry for `name`, if the database knows it.
    pub fn lookup(&self, name: &str) -> Option<&SpellInfo> {
        self.by_name.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "Fireball": ["Throws a fiery ball.", "spell_fire_flamebolt"],
        "Flash Heal": ["A fast spell.", "spell_holy_flashheal"]
    }"#;

    #[test]
    fn parses_and_looks_up_by_name() {
        let db = SpellDb::parse(SAMPLE).expect("valid sample");
        let fireball = db.lookup("Fireball").expect("found");
        assert_eq!(&*fireball.description, "Throws a fiery ball.");
        assert_eq!(&*fireball.icon, "spell_fire_flamebolt");
        assert!(db.lookup("Flash Heal").is_some());
    }

    #[test]
    fn malformed_json_is_an_error() {
        assert!(SpellDb::parse("{nope").is_err());
    }
    /// The real bundled database must parse and contain the well-known spells;
    /// skipped where the generated data is absent (plain `cargo test` on a
    /// checkout without `data/spells/`).
    #[test]
    fn bundled_database_parses() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("native/ has a parent")
            .join("data/spells/spells.json");
        if !path.exists() {
            return;
        }
        let json = std::fs::read_to_string(path).expect("read bundled spells.json");
        let db = SpellDb::parse(&json).expect("bundled spells.json parses");
        assert!(db.lookup("Fireball").is_some());
        assert!(db.lookup("Flash Heal").is_some());
        // These current player abilities use inventory-prefixed icon files;
        // keep them as regressions against filtering by icon basename.
        assert!(db.lookup("Voltaic Blaze").is_some());
        assert!(db.lookup("Deathstalker's Mark").is_some());
        assert!(db.lookup("Goremaw's Bite").is_some());
        for encounter_spell in [
            "Fel Steps",
            "Ferocious Leap",
            "Lightbloom Lashing",
            "Sappy Demise",
            "Savage Smash",
            "Umbral Rupture",
        ] {
            assert!(
                !db.lookup(encounter_spell)
                    .expect("current encounter spell is indexed")
                    .description
                    .is_empty(),
                "{encounter_spell} has tooltip text"
            );
        }
    }
}
