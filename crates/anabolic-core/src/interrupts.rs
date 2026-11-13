// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! How each PCI device interrupts the processor, in the manner of the MSI
//! utilities: line-based (a shared wire, the old way) or by message (MSI
//! and MSI-X: each device and queue its own), what the hardware can do,
//! what Windows' settings for it say, and which processors handle it. A
//! device can be set to messages or back, and its interrupts held to one
//! processor; both take effect when the device next starts, usually at a
//! restart. Drive controllers are only shown: if their driver cannot
//! start the new way, Windows cannot start at all.

#[cfg_attr(windows, path = "windows/interrupts.rs")]
#[cfg_attr(target_os = "linux", path = "linux/interrupts.rs")]
mod imp;
pub use imp::{read, set};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Graphics,
    Network,
    Audio,
    Usb,
    Storage,
    #[default]
    Other,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Graphics => "Graphics",
            Kind::Network => "Network",
            Kind::Audio => "Audio",
            Kind::Usb => "USB",
            Kind::Storage => "Storage",
            Kind::Other => "Other",
        }
    }

    /// From a PCI class code's first four hex digits ("0300", "0C03").
    pub fn from_class(class: &str) -> Kind {
        let c = class.to_uppercase();
        if c.starts_with("03") {
            Kind::Graphics
        } else if c.starts_with("02") {
            Kind::Network
        } else if c.starts_with("0401") || c.starts_with("0403") {
            Kind::Audio
        } else if c.starts_with("0C03") {
            Kind::Usb
        } else if c.starts_with("01") {
            Kind::Storage
        } else {
            Kind::Other
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Device {
    /// The system's name for it: PCI\VEN_10DE&DEV_2C02\4&….
    pub id: String,
    pub name: String,
    pub kind: Kind,
    /// What the hardware can do: line-based, MSI, MSI-X.
    pub line: bool,
    pub msi: bool,
    pub msix: bool,
    /// The most messages it can use.
    pub max_messages: u32,
    /// How it interrupts now: messages (how many), or None for the wire.
    pub messages_now: Option<u32>,
    /// Windows' setting, as the driver's INF or a tool left it: messages
    /// or the wire. Not set means the wire.
    pub msi_setting: Option<bool>,
    /// The processors its interrupts are held to, when they are.
    pub cores: Option<u64>,
}

impl Device {
    /// Its mode can be changed here: messages are possible, and it is no
    /// drive controller.
    pub fn changeable(&self) -> bool {
        (self.msi || self.msix) && self.kind != Kind::Storage
    }

    /// Messages at its next start, by the setting.
    pub fn messages_next(&self) -> bool {
        self.msi_setting == Some(true)
    }

    /// The setting differs from what it does now: waiting for a restart,
    /// or a driver that will not.
    pub fn pending(&self) -> bool {
        self.messages_next() != self.messages_now.is_some()
    }

    pub fn now_words(&self) -> String {
        match self.messages_now {
            Some(1) => "MSI, 1 message".into(),
            Some(n) => format!("MSI, {n} messages"),
            None => "Line-based".into(),
        }
    }

    pub fn can_words(&self) -> String {
        let mut can = Vec::new();
        if self.msix {
            can.push("MSI-X");
        }
        if self.msi {
            can.push("MSI");
        }
        if self.line {
            can.push("line-based");
        }
        let mut out = can.join(", ");
        if self.max_messages > 1 {
            out.push_str(&format!(" (up to {} messages)", self.max_messages));
        }
        out
    }
}

/// Processors as words: "4", "4-7", "0, 2".
pub fn cores_words(mask: u64) -> String {
    let set: Vec<u32> = (0..64).filter(|i| mask & (1 << i) != 0).collect();
    if set.is_empty() {
        return "none".into();
    }
    let mut parts = Vec::new();
    let mut i = 0;
    while i < set.len() {
        let mut j = i;
        while j + 1 < set.len() && set[j + 1] == set[j] + 1 {
            j += 1;
        }
        parts.push(if j > i {
            format!("{}-{}", set[i], set[j])
        } else {
            set[i].to_string()
        });
        i = j + 1;
    }
    parts.join(", ")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Interrupt by messages (true) or by the wire (false).
    Messages(bool),
    /// Hold its interrupts to these processors; None lets Windows choose.
    Cores(Option<u64>),
}

impl Change {
    pub fn words(&self) -> String {
        match self {
            Change::Messages(true) => "Use MSI".into(),
            Change::Messages(false) => "Use line-based interrupts".into(),
            Change::Cores(Some(m)) => {
                format!("Handle its interrupts on processor {}", cores_words(*m))
            }
            Change::Cores(None) => "Let Windows choose its processors".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_cores_and_kinds() {
        assert_eq!(cores_words(0b1111_0000), "4-7");
        assert_eq!(cores_words(0b101), "0, 2");
        assert_eq!(cores_words(1 << 31), "31");
        assert_eq!(Kind::from_class("0C03"), Kind::Usb);
        assert_eq!(Kind::from_class("0300"), Kind::Graphics);
        assert_eq!(Kind::from_class("0108"), Kind::Storage);
        let d = Device {
            msi: true,
            kind: Kind::Storage,
            ..Device::default()
        };
        assert!(!d.changeable());
    }
}
