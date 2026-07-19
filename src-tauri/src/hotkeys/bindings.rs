use serde::{Deserialize, Serialize};

/// Left/right-specific modifier virtual-key codes as delivered by WH_KEYBOARD_LL.
/// Order defines the bit position in a modifier mask.
pub const MODIFIER_VKS: [u16; 8] = [
    0xA0, // VK_LSHIFT
    0xA1, // VK_RSHIFT
    0xA2, // VK_LCONTROL
    0xA3, // VK_RCONTROL
    0xA4, // VK_LMENU
    0xA5, // VK_RMENU
    0x5B, // VK_LWIN
    0x5C, // VK_RWIN
];

pub const VK_ESCAPE: u16 = 0x1B;
pub const VK_RCONTROL: u16 = 0xA3;
pub const VK_F9: u16 = 0x78;

pub fn is_modifier(vk: u16) -> bool {
    MODIFIER_VKS.contains(&vk)
}

pub fn modifier_bit(vk: u16) -> u8 {
    MODIFIER_VKS
        .iter()
        .position(|&m| m == vk)
        .map(|i| 1u8 << i)
        .unwrap_or(0)
}

/// A shortcut is a set of virtual keys that must all be physically down:
/// zero or more left/right-specific modifiers plus at most one normal key.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortcutBinding {
    pub vks: Vec<u16>,
}

impl ShortcutBinding {
    pub fn new(vks: Vec<u16>) -> Self {
        Self { vks }
    }

    pub fn is_empty(&self) -> bool {
        self.vks.is_empty()
    }

    /// Mask of the binding's modifier keys (bit positions per MODIFIER_VKS).
    pub fn modifier_mask(&self) -> u8 {
        self.vks
            .iter()
            .filter(|vk| is_modifier(**vk))
            .fold(0u8, |acc, vk| acc | modifier_bit(*vk))
    }

    /// The binding's single non-modifier key, if any.
    pub fn key(&self) -> Option<u16> {
        self.vks.iter().copied().find(|vk| !is_modifier(*vk))
    }

    pub fn contains(&self, vk: u16) -> bool {
        self.vks.contains(&vk)
    }

    pub fn label(&self) -> String {
        if self.is_empty() {
            return "Disabled".into();
        }
        self.vks
            .iter()
            .map(|vk| vk_display_name(*vk))
            .collect::<Vec<_>>()
            .join(" + ")
    }
}

pub fn vk_display_name(vk: u16) -> String {
    match vk {
        0xA0 => "Left Shift".into(),
        0xA1 => "Right Shift".into(),
        0xA2 => "Left Ctrl".into(),
        0xA3 => "Right Ctrl".into(),
        0xA4 => "Left Alt".into(),
        0xA5 => "Right Alt".into(),
        0x5B => "Left Win".into(),
        0x5C => "Right Win".into(),
        0x1B => "Esc".into(),
        0x20 => "Space".into(),
        0x09 => "Tab".into(),
        0x0D => "Enter".into(),
        0x14 => "Caps Lock".into(),
        0x2D => "Insert".into(),
        0x2E => "Delete".into(),
        0x24 => "Home".into(),
        0x23 => "End".into(),
        0x21 => "Page Up".into(),
        0x22 => "Page Down".into(),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        0x30..=0x39 | 0x41..=0x5A => char::from(vk as u8).to_string(),
        _ => format!("Key 0x{vk:02X}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_mask_and_key() {
        let b = ShortcutBinding::new(vec![VK_RCONTROL, VK_F9]);
        assert_eq!(b.modifier_mask(), modifier_bit(VK_RCONTROL));
        assert_eq!(b.key(), Some(VK_F9));

        let mod_only = ShortcutBinding::new(vec![VK_RCONTROL]);
        assert_eq!(mod_only.key(), None);
        assert_eq!(mod_only.modifier_mask(), modifier_bit(VK_RCONTROL));
    }

    #[test]
    fn labels() {
        assert_eq!(
            ShortcutBinding::new(vec![VK_RCONTROL]).label(),
            "Right Ctrl"
        );
        assert_eq!(ShortcutBinding::new(vec![VK_F9]).label(), "F9");
        assert_eq!(ShortcutBinding::default().label(), "Disabled");
    }
}
