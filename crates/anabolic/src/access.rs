// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Access masks of handles, in words: what a handle lets its holder do.

/// Rights every object type shares (the upper half of the mask).
const STANDARD: [(u32, &str); 5] = [
    (0x0001_0000, "delete"),
    (0x0002_0000, "read permissions"),
    (0x0004_0000, "change permissions"),
    (0x0008_0000, "take ownership"),
    (0x0010_0000, "synchronize"),
];

/// (all rights of the type, its specific rights).
fn specific(type_name: &str) -> Option<(u32, &'static [(u32, &'static str)])> {
    Some(match type_name {
        "Process" => (
            0x001F_FFFF,
            &[
                (0x0001, "terminate"),
                (0x0002, "create threads"),
                (0x0008, "change memory"),
                (0x0010, "read memory"),
                (0x0020, "write memory"),
                (0x0040, "duplicate handles"),
                (0x0080, "create processes"),
                (0x0100, "set quotas"),
                (0x0200, "set information"),
                (0x0400, "query information"),
                (0x0800, "suspend and resume"),
                (0x1000, "query limited information"),
            ],
        ),
        "Thread" => (
            0x001F_FFFF,
            &[
                (0x0001, "terminate"),
                (0x0002, "suspend and resume"),
                (0x0008, "read registers"),
                (0x0010, "write registers"),
                (0x0020, "set information"),
                (0x0040, "query information"),
                (0x0080, "set token"),
                (0x0100, "impersonate"),
                (0x0200, "direct impersonation"),
                (0x0400, "set limited information"),
                (0x0800, "query limited information"),
            ],
        ),
        "File" => (
            0x001F_01FF,
            &[
                (0x0001, "read data"),
                (0x0002, "write data"),
                (0x0004, "append data"),
                (0x0008, "read extended attributes"),
                (0x0010, "write extended attributes"),
                (0x0020, "execute"),
                (0x0040, "delete children"),
                (0x0080, "read attributes"),
                (0x0100, "write attributes"),
            ],
        ),
        "Key" => (
            0x000F_003F,
            &[
                (0x0001, "query values"),
                (0x0002, "set values"),
                (0x0004, "create subkeys"),
                (0x0008, "enumerate subkeys"),
                (0x0010, "notify"),
                (0x0020, "create links"),
            ],
        ),
        "Event" | "Semaphore" => (
            0x001F_0003,
            &[(0x0001, "query state"), (0x0002, "modify state")],
        ),
        "Mutant" => (0x001F_0001, &[(0x0001, "query state")]),
        "Section" => (
            0x000F_001F,
            &[
                (0x0001, "query"),
                (0x0002, "map for writing"),
                (0x0004, "map for reading"),
                (0x0008, "map for executing"),
                (0x0010, "extend size"),
            ],
        ),
        "Token" => (
            0x000F_01FF,
            &[
                (0x0001, "assign as primary"),
                (0x0002, "duplicate"),
                (0x0004, "impersonate"),
                (0x0008, "query"),
                (0x0010, "query source"),
                (0x0020, "adjust privileges"),
                (0x0040, "adjust groups"),
                (0x0080, "adjust defaults"),
                (0x0100, "adjust session"),
            ],
        ),
        "Directory" => (
            0x000F_000F,
            &[
                (0x0001, "query"),
                (0x0002, "traverse"),
                (0x0004, "create objects"),
                (0x0008, "create subdirectories"),
            ],
        ),
        "ALPC Port" => (0x001F_0001, &[(0x0001, "connect")]),
        _ => return None,
    })
}

/// The rights in `access`, named for objects of `type_name`; "full access"
/// when it has them all. Bits without a name come last, as hex.
pub fn rights(type_name: &str, access: u32) -> Vec<String> {
    let table = specific(type_name);
    if let Some((all, _)) = table
        && access & all == all
    {
        return vec!["full access".into()];
    }
    let mut out = Vec::new();
    let mut known = 0u32;
    for (bit, name) in table
        .map_or(&[][..], |(_, t)| t)
        .iter()
        .chain(STANDARD.iter())
    {
        known |= bit;
        if access & bit != 0 {
            out.push((*name).to_string());
        }
    }
    let rest = access & !known;
    if rest != 0 {
        out.push(format!("0x{rest:X}"));
    }
    out
}

/// What the attribute bits of a handle mean.
pub fn attributes(attributes: u32) -> Vec<&'static str> {
    [
        (0x01, "protected from closing"),
        (0x02, "inherited by child processes"),
        (0x04, "audited on close"),
    ]
    .iter()
    .filter(|(bit, _)| attributes & bit != 0)
    .map(|(_, name)| *name)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_rights_per_type() {
        assert_eq!(rights("Process", 0x1F_FFFF), ["full access"]);
        assert_eq!(
            rights("Process", 0x1400),
            ["query information", "query limited information"]
        );
        assert_eq!(
            rights("File", 0x12_0089),
            [
                "read data",
                "read extended attributes",
                "read attributes",
                "read permissions",
                "synchronize"
            ]
        );
        assert_eq!(rights("Event", 0x10_0002), ["modify state", "synchronize"]);
        // Unknown types still get the standard rights; the rest as hex.
        assert_eq!(
            rights("WaitCompletionPacket", 0x10_0001),
            ["synchronize", "0x1"]
        );
        assert_eq!(
            attributes(0x3),
            ["protected from closing", "inherited by child processes"]
        );
    }
}
