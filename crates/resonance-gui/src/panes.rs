//! Which GUI panels the user has chosen to hide (Settings → Panes). Pure logic
//! (no egui) so the enum mapping and persistence parsing are unit-tested. The
//! FR graph itself is never hideable — it fills the window when everything else
//! is hidden.

use crate::card_layout::{CardCol, CardId};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// A hideable GUI panel. The five card panes map 1:1 to a `CardId`; `Bands` and
/// `ReferenceBar` are the two fixed anchors that are not movable cards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum PaneId {
    Effects,
    Applications,
    Outputs,
    DeviceMap,
    Profiles,
    Bands,
    ReferenceBar,
}

impl PaneId {
    /// Every hideable pane, in the order the Settings checkboxes list them.
    pub(crate) const ALL: [PaneId; 7] = [
        PaneId::Effects,
        PaneId::Applications,
        PaneId::Outputs,
        PaneId::DeviceMap,
        PaneId::Profiles,
        PaneId::Bands,
        PaneId::ReferenceBar,
    ];

    /// Label for the Settings checkbox.
    pub(crate) fn title(self) -> &'static str {
        match self {
            PaneId::Effects => "Effects",
            PaneId::Applications => "Applications",
            PaneId::Outputs => "Outputs",
            PaneId::DeviceMap => "Device → Profile",
            PaneId::Profiles => "Profiles",
            PaneId::Bands => "EQ bands",
            PaneId::ReferenceBar => "Reference bar",
        }
    }

    /// The pane for a movable card. The two fixed anchors (`Bands`,
    /// `ReferenceBar`) have no card and are never produced here.
    pub(crate) fn from_card(card: CardId) -> PaneId {
        match card {
            CardId::Effects => PaneId::Effects,
            CardId::Applications => PaneId::Applications,
            CardId::Outputs => PaneId::Outputs,
            CardId::DeviceMap => PaneId::DeviceMap,
            CardId::Profiles => PaneId::Profiles,
        }
    }

    /// The movable card this pane corresponds to, or `None` for the two fixed
    /// anchors (`Bands`, `ReferenceBar`). Inverse of [`from_card`](Self::from_card).
    pub(crate) fn card(self) -> Option<CardId> {
        Some(match self {
            PaneId::Effects => CardId::Effects,
            PaneId::Applications => CardId::Applications,
            PaneId::Outputs => CardId::Outputs,
            PaneId::DeviceMap => CardId::DeviceMap,
            PaneId::Profiles => CardId::Profiles,
            PaneId::Bands | PaneId::ReferenceBar => return None,
        })
    }
}

/// A pending arrange-mode mutation, applied once after the frame renders (so the
/// column / tray lists are never mutated mid-iteration).
#[derive(Debug, Clone, Copy)]
pub(crate) enum PaneAction {
    /// Place a card into `col` at absolute index `idx` (and unhide it).
    PlaceCard {
        card: CardId,
        col: CardCol,
        idx: usize,
    },
    /// Show a pane in its home (remove from the hidden set).
    Show(PaneId),
    /// Hide a pane (add to the hidden set; a card keeps its column slot).
    Hide(PaneId),
}

/// Parse the persisted hidden-panes set (a JSON array of `PaneId`). Any parse
/// error or unknown variant yields an empty set (all panes visible), so corrupt
/// or version-skewed storage never hides content unexpectedly.
pub(crate) fn hidden_from_json_or_default(s: &str) -> HashSet<PaneId> {
    serde_json::from_str::<HashSet<PaneId>>(s).unwrap_or_default()
}

/// How the live lower area lays out when EQ bands is hidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum BandsOffLayout {
    /// Cards render in their Left/Right columns side by side (two equal columns
    /// when both are populated; one fills the width when only one is).
    #[default]
    Columns,
    /// All visible cards stack in a single full-width column.
    Stacked,
}

impl BandsOffLayout {
    /// Parse the persisted value; anything unrecognised falls back to the
    /// default (`Columns`).
    pub(crate) fn from_storage(s: &str) -> Self {
        match s {
            "stacked" => Self::Stacked,
            _ => Self::Columns,
        }
    }

    /// The stable string written to storage.
    pub(crate) fn as_storage(self) -> &'static str {
        match self {
            Self::Columns => "columns",
            Self::Stacked => "stacked",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_has_seven_distinct_panes() {
        assert_eq!(PaneId::ALL.len(), 7);
        let set: HashSet<PaneId> = PaneId::ALL.iter().copied().collect();
        assert_eq!(set.len(), 7);
    }

    #[test]
    fn from_card_covers_the_five_card_panes_only() {
        let mapped: HashSet<PaneId> = CardId::ALL.iter().copied().map(PaneId::from_card).collect();
        // Injective: each card maps to a distinct pane.
        assert_eq!(mapped.len(), CardId::ALL.len());
        // The two fixed anchors are never produced by a card mapping.
        assert!(!mapped.contains(&PaneId::Bands));
        assert!(!mapped.contains(&PaneId::ReferenceBar));
    }

    #[test]
    fn card_round_trips_and_anchors_have_no_card() {
        for c in CardId::ALL {
            assert_eq!(PaneId::from_card(c).card(), Some(c));
        }
        assert_eq!(PaneId::Bands.card(), None);
        assert_eq!(PaneId::ReferenceBar.card(), None);
    }

    #[test]
    fn hidden_set_json_round_trips() {
        let hidden: HashSet<PaneId> = [PaneId::Bands, PaneId::Outputs].into_iter().collect();
        let json = serde_json::to_string(&hidden).unwrap();
        assert_eq!(hidden_from_json_or_default(&json), hidden);
    }

    #[test]
    fn invalid_or_unknown_json_falls_back_to_all_visible() {
        assert!(hidden_from_json_or_default("garbage").is_empty());
        // One unknown variant fails the whole parse → empty (all visible).
        assert!(hidden_from_json_or_default(r#"["Effects","Nope"]"#).is_empty());
    }

    #[test]
    fn bands_off_layout_storage_round_trip() {
        for v in [BandsOffLayout::Columns, BandsOffLayout::Stacked] {
            assert_eq!(BandsOffLayout::from_storage(v.as_storage()), v);
        }
    }

    #[test]
    fn bands_off_layout_defaults_to_columns() {
        assert_eq!(BandsOffLayout::default(), BandsOffLayout::Columns);
        assert_eq!(
            BandsOffLayout::from_storage("garbage"),
            BandsOffLayout::Columns
        );
    }

    #[test]
    fn titles_are_nonempty() {
        assert!(PaneId::ALL.iter().all(|p| !p.title().is_empty()));
    }
}
