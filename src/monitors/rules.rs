//! Pure identity, capability and level conversions shared by the backends.
pub(super) fn identity(path: &str) -> Option<(String, String)> {
    let normalized = path.replace("&amp;", "&").replace('#', "\\");
    let mut parts = normalized
        .split('\\')
        .skip_while(|s| !s.eq_ignore_ascii_case("DISPLAY"));
    parts.next()?;
    let model = parts.next()?.to_ascii_uppercase();
    let id = parts.next()?.split('_').next()?.to_ascii_uppercase();
    (!model.is_empty() && !id.is_empty()).then_some((model, id))
}

pub(super) fn brightness_codes(model: &str) -> Vec<u8> {
    let rules: serde_json::Value =
        serde_json::from_str(include_str!("../../assets/monitor-rules.json"))
            .expect("embedded monitor rules must be valid JSON");
    let mut codes = Vec::new();
    if let Some(code) = rules["ddcBrightnessCodes"][model]
        .as_u64()
        .and_then(|v| u8::try_from(v).ok())
    {
        codes.push(code);
    }
    for code in [0x10, 0x13, 0x6b, 0x12] {
        if !codes.contains(&code) {
            codes.push(code);
        }
    }
    codes
}

pub(super) fn sdr_raw(percent: u8) -> u32 {
    (80 + u32::from(percent.min(100)) * 4) * 1000 / 80
}
pub(super) fn sdr_percent(raw: u32) -> u8 {
    ((u64::from(raw) * 80 / 1000).saturating_sub(80) / 4).min(100) as u8
}

/// Only top-level VCP tokens count; values inside a feature's parentheses do not.
pub(super) fn has_power(capabilities: &str) -> bool {
    let lower = capabilities.to_ascii_lowercase();
    let Some(start) = lower.find("vcp(") else {
        return false;
    };
    let mut depth = 0usize;
    let mut token = String::new();
    for c in lower[start + 4..].chars() {
        if depth == 0 && !c.is_ascii_hexdigit() {
            if token == "d6" {
                return true;
            }
            token.clear();
        }
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => return false,
            ')' => depth -= 1,
            c if depth == 0 && c.is_ascii_hexdigit() => token.push(c),
            _ => {}
        }
    }
    token == "d6"
}

/// Match physical handles in their API order. QDC order is only a last resort:
/// cloned displays can return QDC paths in the reverse of DISPLAY_DEVICE order.
pub(super) fn match_physical(
    descriptions: &[String],
    targets: &[(String, String)],
    devices: &[String],
) -> Vec<Option<String>> {
    let mut claimed = std::collections::HashSet::new();
    descriptions
        .iter()
        .enumerate()
        .map(|(index, description)| {
            let names: Vec<_> = targets
                .iter()
                .filter(|(_, name)| name == description)
                .collect();
            let by_name = (!description.is_empty()
                && descriptions.iter().filter(|d| *d == description).count() == 1
                && names.len() == 1)
                .then(|| names[0].0.clone());
            let by_device = devices
                .get(index)
                .and_then(|d| identity(d))
                .map(|(_, id)| id);
            let by_index = targets.get(index).map(|(id, _)| id.clone());
            let key = by_name
                .filter(|id| !claimed.contains(id))
                .or_else(|| by_device.filter(|id| !claimed.contains(id)))
                .or_else(|| by_index.filter(|id| !claimed.contains(id)));
            if let Some(key) = &key {
                claimed.insert(key.clone());
            }
            key
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identities_merge() {
        assert_eq!(
            identity(r"\\?\DISPLAY#DEL41D9#5&abc&0&UID123#{guid}"),
            identity(r"DISPLAY\DEL41D9\5&abc&0&UID123_0")
        );
        assert_eq!(identity("garbage"), None);
        assert_eq!(identity(r"DISPLAY\\instance"), None);
        assert_eq!(identity(r"DISPLAY\MODEL\_0"), None);
    }
    #[test]
    fn rules_precede_fallbacks() {
        assert_eq!(brightness_codes("FUS087C"), vec![0x6b, 0x10, 0x13, 0x12]);
        assert_eq!(brightness_codes("unknown"), vec![0x10, 0x13, 0x6b, 0x12]);
    }
    #[test]
    fn sdr_roundtrip() {
        for p in 0..=100 {
            assert_eq!(sdr_percent(sdr_raw(p)), p);
        }
        assert_eq!(sdr_percent(0), 0);
        assert_eq!(sdr_percent(u32::MAX), 100);
    }
    #[test]
    fn capability_tokens() {
        assert!(has_power("(vcp(10 12 D6(01 04 05)))"));
        assert!(!has_power("(vcp(10(D6) 12))"));
        assert!(!has_power("(model(D6))"));
    }
    #[test]
    fn clone_order_uses_display_devices_before_qdc() {
        let descriptions = vec!["Same panel".into(), "Same panel".into()];
        let targets = vec![
            ("B".into(), "Same panel".into()),
            ("A".into(), "Same panel".into()),
        ];
        let devices = vec![
            r"\\?\DISPLAY#MODEL#A#{guid}".into(),
            r"\\?\DISPLAY#MODEL#B#{guid}".into(),
        ];
        assert_eq!(
            match_physical(&descriptions, &targets, &devices),
            vec![Some("A".into()), Some("B".into())]
        );
    }
    #[test]
    fn unique_descriptions_win_and_handles_are_never_claimed_twice() {
        let descriptions = vec!["Panel A".into(), "Panel B".into(), "Unknown".into()];
        let targets = vec![
            ("B".into(), "Panel B".into()),
            ("A".into(), "Panel A".into()),
        ];
        let devices = vec![
            r"DISPLAY\MODEL\B".into(),
            r"DISPLAY\MODEL\A".into(),
            r"DISPLAY\MODEL\A".into(),
        ];
        assert_eq!(
            match_physical(&descriptions, &targets, &devices),
            vec![Some("A".into()), Some("B".into()), None]
        );
    }
}
