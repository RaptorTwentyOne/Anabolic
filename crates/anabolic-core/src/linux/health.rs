// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Drive health on Linux, through smartmontools' `smartctl --json`: the
//! kernel hands SMART data and NVMe logs to root alone, and smartctl knows
//! every drive's quirks. A sleeping drive is left asleep (`-n standby`).

use std::process::Command;

use serde_json::Value;

use crate::health::{Attribute, Health};

fn model_of(name: &str) -> String {
    std::fs::read_to_string(format!("/sys/block/{name}/device/model"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

pub fn read() -> Vec<Health> {
    let names = crate::disk::physical_disks();
    let installed = Command::new("smartctl")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    names
        .iter()
        .enumerate()
        .map(|(n, name)| {
            let mut h = Health {
                number: n as u32,
                model: model_of(name),
                protocol: if name.starts_with("nvme") {
                    "NVMe"
                } else {
                    "SATA"
                },
                ..Health::default()
            };
            if !installed {
                h.note = Some("smartctl (smartmontools) is not installed".into());
                return h;
            }
            let out = Command::new("smartctl")
                .args(["--json=c", "-a", "-n", "standby", &format!("/dev/{name}")])
                .output();
            match out
                .ok()
                .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
            {
                Some(j) => from_json(&mut h, &j),
                None => h.note = Some("smartctl gave no answer".into()),
            }
            h.judge();
            h
        })
        .collect()
}

fn from_json(h: &mut Health, j: &Value) {
    if let Some(m) = j["model_name"].as_str() {
        h.model = m.to_string();
    }
    let messages: Vec<&str> = j["smartctl"]["messages"]
        .as_array()
        .map(|m| m.iter().filter_map(|m| m["string"].as_str()).collect())
        .unwrap_or_default();
    if messages
        .iter()
        .any(|m| m.contains("STANDBY") || m.contains("SLEEP"))
    {
        h.note = Some("It sleeps: not woken to ask".into());
        return;
    }
    if messages.iter().any(|m| m.contains("Permission denied")) {
        h.note = Some("Its health data needs root".into());
        return;
    }
    if j["smart_status"]["passed"].as_bool() == Some(false) {
        h.drive_warns = true;
    }
    let nvme = &j["nvme_smart_health_information_log"];
    if nvme.is_object() {
        let n = |k: &str| nvme[k].as_u64();
        h.drive_warns |= n("critical_warning").unwrap_or(0) & 0b0011_1101 != 0;
        h.temperature = n("temperature").map(|t| t as i16);
        h.spare = n("available_spare")
            .zip(n("available_spare_threshold"))
            .map(|(a, b)| (a as u8, b as u8));
        h.wear_used = n("percentage_used").map(|p| p as u32);
        h.read = n("data_units_read").map(|u| u.saturating_mul(512_000));
        h.written = n("data_units_written").map(|u| u.saturating_mul(512_000));
        h.power_cycles = n("power_cycles");
        h.power_on_hours = n("power_on_hours");
        h.unsafe_shutdowns = n("unsafe_shutdowns");
        h.media_errors = n("media_errors");
        return;
    }
    let table = j["ata_smart_attributes"]["table"].as_array();
    let attributes: Vec<Attribute> = table
        .map(|t| {
            t.iter()
                .filter_map(|a| {
                    Some(Attribute {
                        id: a["id"].as_u64()? as u8,
                        current: a["value"].as_u64().unwrap_or(0) as u8,
                        worst: a["worst"].as_u64().unwrap_or(0) as u8,
                        threshold: a["thresh"].as_u64().unwrap_or(0) as u8,
                        raw: a["raw"]["value"].as_u64().unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if attributes.is_empty() {
        h.note = Some("The drive does not report SMART data".into());
    } else {
        h.take_attributes(attributes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_smartctls_json() {
        let mut h = Health {
            protocol: "NVMe",
            ..Health::default()
        };
        from_json(
            &mut h,
            &serde_json::json!({
                "model_name": "Lexar SSD NM790 2TB",
                "smart_status": {"passed": true},
                "nvme_smart_health_information_log": {
                    "critical_warning": 0, "temperature": 43, "available_spare": 100,
                    "available_spare_threshold": 10, "percentage_used": 2,
                    "data_units_written": 1000, "power_on_hours": 812, "unsafe_shutdowns": 9
                }
            }),
        );
        h.judge();
        assert_eq!(h.model, "Lexar SSD NM790 2TB");
        assert_eq!(h.wear_used, Some(2));
        assert_eq!(h.written, Some(512_000_000));
        assert_eq!(h.verdict, crate::health::Verdict::Good);

        let mut s = Health {
            protocol: "SATA",
            ..Health::default()
        };
        from_json(
            &mut s,
            &serde_json::json!({
                "smart_status": {"passed": true},
                "ata_smart_attributes": {"table": [
                    {"id": 5, "value": 100, "worst": 100, "thresh": 10, "raw": {"value": 3}},
                    {"id": 9, "value": 98, "worst": 98, "thresh": 0, "raw": {"value": 33309}}
                ]}
            }),
        );
        s.judge();
        assert_eq!(s.power_on_hours, Some(33_309));
        assert_eq!(
            s.verdict,
            crate::health::Verdict::Watch,
            "sectors moved aside"
        );

        let mut asleep = Health::default();
        from_json(
            &mut asleep,
            &serde_json::json!({"smartctl": {"messages": [{"string": "Device is in STANDBY mode, exit(2)"}]}}),
        );
        assert!(asleep.note.unwrap().contains("sleeps"));
    }
}
