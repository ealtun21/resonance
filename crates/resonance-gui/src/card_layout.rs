//! Persisted arrangement of the GUI's movable control cards across the two side
//! columns. Pure logic (no egui) so the reorder maths is unit-tested.

use serde::{Deserialize, Serialize};

/// The movable control cards. The graph and EQ Bands are fixed anchors and are
/// deliberately absent here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum CardId {
    Effects,
    Applications,
    Outputs,
    DeviceMap,
    Profiles,
}

impl CardId {
    /// Every card, in canonical order. Also used to validate a loaded layout.
    pub(crate) const ALL: [CardId; 5] = [
        CardId::Effects,
        CardId::Applications,
        CardId::Outputs,
        CardId::DeviceMap,
        CardId::Profiles,
    ];

    /// Display name for the edit-mode tile.
    pub(crate) fn title(self) -> &'static str {
        match self {
            CardId::Effects => "Effects",
            CardId::Applications => "Applications",
            CardId::Outputs => "Outputs",
            CardId::DeviceMap => "Device → Profile",
            CardId::Profiles => "Profiles",
        }
    }
}

/// Which side column a card lives in. The center column is always EQ Bands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CardCol {
    Left,
    Right,
}

/// The user's card arrangement. Every `CardId` appears exactly once across the
/// two columns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CardLayout {
    pub(crate) left: Vec<CardId>,
    pub(crate) right: Vec<CardId>,
}

impl Default for CardLayout {
    fn default() -> Self {
        Self {
            left: vec![CardId::Effects],
            right: vec![
                CardId::Applications,
                CardId::Outputs,
                CardId::DeviceMap,
                CardId::Profiles,
            ],
        }
    }
}

impl CardLayout {
    pub(crate) fn column(&self, col: CardCol) -> &[CardId] {
        match col {
            CardCol::Left => &self.left,
            CardCol::Right => &self.right,
        }
    }

    fn column_mut(&mut self, col: CardCol) -> &mut Vec<CardId> {
        match col {
            CardCol::Left => &mut self.left,
            CardCol::Right => &mut self.right,
        }
    }

    fn locate(&self, id: CardId) -> Option<(CardCol, usize)> {
        if let Some(i) = self.left.iter().position(|&c| c == id) {
            return Some((CardCol::Left, i));
        }
        self.right
            .iter()
            .position(|&c| c == id)
            .map(|i| (CardCol::Right, i))
    }

    /// Move `id` to slot `to_idx` in `to_col`. Removes it from its current
    /// position first; `to_idx` is clamped to the target length and decremented
    /// when moving forward within the same column (the removal shifts later
    /// indices). No-op if `id` is not present.
    pub(crate) fn move_card(&mut self, id: CardId, to_col: CardCol, to_idx: usize) {
        let Some((from_col, from_idx)) = self.locate(id) else {
            return;
        };
        self.column_mut(from_col).remove(from_idx);
        let mut idx = to_idx;
        if from_col == to_col && from_idx < to_idx {
            idx -= 1;
        }
        let v = self.column_mut(to_col);
        let idx = idx.min(v.len());
        v.insert(idx, id);
    }

    /// True when the layout holds exactly the 5 known cards, once each.
    fn is_valid(&self) -> bool {
        let all: Vec<CardId> = self.left.iter().chain(&self.right).copied().collect();
        all.len() == CardId::ALL.len() && CardId::ALL.iter().all(|c| all.contains(c))
    }

    /// Parse persisted JSON, falling back to the default on any parse error or if
    /// the parsed layout doesn't contain exactly the known cards (guards corrupt
    /// or version-skewed storage).
    pub(crate) fn from_json_or_default(s: &str) -> Self {
        serde_json::from_str::<CardLayout>(s)
            .ok()
            .filter(CardLayout::is_valid)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_todays_arrangement() {
        let l = CardLayout::default();
        assert_eq!(l.left, vec![CardId::Effects]);
        assert_eq!(
            l.right,
            vec![
                CardId::Applications,
                CardId::Outputs,
                CardId::DeviceMap,
                CardId::Profiles
            ]
        );
        assert!(l.is_valid());
    }

    #[test]
    fn move_across_columns() {
        let mut l = CardLayout::default();
        l.move_card(CardId::Effects, CardCol::Right, 0);
        assert!(l.left.is_empty());
        assert_eq!(l.right[0], CardId::Effects);
        assert!(l.is_valid());
    }

    #[test]
    fn reorder_within_column_forward() {
        let mut l = CardLayout::default();
        l.move_card(CardId::Applications, CardCol::Right, 4);
        assert_eq!(
            l.right,
            vec![
                CardId::Outputs,
                CardId::DeviceMap,
                CardId::Profiles,
                CardId::Applications
            ]
        );
        assert!(l.is_valid());
    }

    #[test]
    fn reorder_within_column_backward() {
        let mut l = CardLayout::default();
        l.move_card(CardId::Profiles, CardCol::Right, 0);
        assert_eq!(
            l.right,
            vec![
                CardId::Profiles,
                CardId::Applications,
                CardId::Outputs,
                CardId::DeviceMap
            ]
        );
        assert!(l.is_valid());
    }

    #[test]
    fn move_last_card_out_leaves_empty_column() {
        let mut l = CardLayout::default();
        l.move_card(CardId::Effects, CardCol::Right, 4);
        assert!(l.left.is_empty());
        assert!(l.is_valid());
    }

    #[test]
    fn clamps_index_past_end() {
        let mut l = CardLayout::default();
        l.move_card(CardId::Effects, CardCol::Right, 999);
        assert_eq!(l.right.last(), Some(&CardId::Effects));
        assert!(l.is_valid());
    }

    #[test]
    fn always_five_cards_after_moves() {
        let mut l = CardLayout::default();
        l.move_card(CardId::Effects, CardCol::Right, 2);
        l.move_card(CardId::Profiles, CardCol::Left, 0);
        l.move_card(CardId::Outputs, CardCol::Left, 5);
        assert_eq!(l.left.len() + l.right.len(), 5);
        assert!(l.is_valid());
    }

    #[test]
    fn json_round_trip() {
        let mut l = CardLayout::default();
        l.move_card(CardId::Profiles, CardCol::Left, 0);
        let s = serde_json::to_string(&l).unwrap();
        assert_eq!(CardLayout::from_json_or_default(&s), l);
    }

    #[test]
    fn invalid_json_falls_back_to_default() {
        assert_eq!(
            CardLayout::from_json_or_default("garbage"),
            CardLayout::default()
        );
        let partial = r#"{"left":["Effects"],"right":["Profiles"]}"#;
        assert_eq!(
            CardLayout::from_json_or_default(partial),
            CardLayout::default()
        );
    }
}
