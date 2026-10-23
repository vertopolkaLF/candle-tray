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

/// Top-level VCP codes of a capabilities string with the values listed for
/// each, e.g. `vcp(10 60(0F 11) D6(01 04))`. Values inside a feature's
/// parentheses are never codes themselves.
pub(super) fn vcp_features(capabilities: &str) -> Vec<(u8, Vec<u8>)> {
    let lower = capabilities.to_ascii_lowercase();
    let Some(start) = lower.find("vcp(") else {
        return Vec::new();
    };
    let mut features: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut depth = 0usize;
    let mut token = String::new();
    let flush = |token: &mut String, depth: usize, features: &mut Vec<(u8, Vec<u8>)>| {
        // Some displays list values without spaces ("0F1112").
        if token.len().is_multiple_of(2) {
            for pair in token.as_bytes().chunks(2) {
                let Some(value) = std::str::from_utf8(pair)
                    .ok()
                    .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                else {
                    continue;
                };
                match depth {
                    0 => features.push((value, Vec::new())),
                    1 => {
                        if let Some((_, values)) = features.last_mut() {
                            values.push(value);
                        }
                    }
                    _ => {}
                }
            }
        }
        token.clear();
    };
    for c in lower[start + 4..].chars() {
        if c.is_ascii_hexdigit() {
            token.push(c);
            continue;
        }
        flush(&mut token, depth, &mut features);
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => return features,
            ')' => depth -= 1,
            _ => {}
        }
    }
    flush(&mut token, depth, &mut features);
    features
}

/// The display lists VCP 0xD6 (power mode).
pub(super) fn has_power(capabilities: &str) -> bool {
    vcp_features(capabilities)
        .iter()
        .any(|(code, _)| *code == 0xd6)
}

/// Input sources the display lists for VCP 0x60, in its order.
pub(super) fn input_sources(capabilities: &str) -> Vec<u8> {
    let mut inputs = vcp_features(capabilities)
        .into_iter()
        .find(|(code, _)| *code == 0x60)
        .map(|(_, values)| values)
        .unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    inputs.retain(|value| *value != 0 && seen.insert(*value));
    inputs
}

/// Match physical handles in their API order. QDC order is only a last resort:
/// cloned displays can return QDC paths in the reverse of DISPLAY_DEVICE order.
pub(super) fn match_physical(
    descriptions: &[String],
    targets: &[(String, String)],
    devices: &[String],
) -> Vec<Option<String>> {
    let mut claimed = std::collections::HashSet::new();
    let mut matches = vec![None; descriptions.len()];
    // Reserve every exact name before weaker matches can consume its target.
    for (index, description) in descriptions.iter().enumerate() {
        let names: Vec<_> = targets
            .iter()
            .filter(|(_, name)| name == description)
            .collect();
        if !description.is_empty()
            && descriptions.iter().filter(|d| *d == description).count() == 1
            && names.len() == 1
            && claimed.insert(names[0].0.clone())
        {
            matches[index] = Some(names[0].0.clone());
        }
    }
    // Device identities outrank positional fallback, including identities
    // for handles that appear later in the API's physical-monitor order.
    for (index, matched) in matches.iter_mut().enumerate() {
        if matched.is_none()
            && let Some((_, id)) = devices.get(index).and_then(|device| identity(device))
            && targets.iter().any(|(candidate, _)| *candidate == id)
            && claimed.insert(id.clone())
        {
            *matched = Some(id);
        }
    }
    let mut remaining = targets.iter().filter(|(id, _)| !claimed.contains(id));
    for matched in &mut matches {
        if matched.is_none() {
            *matched = remaining.next().map(|(id, _)| id.clone());
        }
    }
    matches
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fallback_cannot_steal_a_later_name_or_device_match() {
        let targets = vec![
            ("B".into(), "Panel B".into()),
            ("A".into(), "Panel A".into()),
        ];
        assert_eq!(
            match_physical(&["Unknown".into(), "Panel B".into()], &targets, &[]),
            vec![Some("A".into()), Some("B".into())]
        );
        assert_eq!(
            match_physical(
                &["Unknown 1".into(), "Unknown 2".into()],
                &targets,
                &[String::new(), r"DISPLAY\MODEL\B".into()],
            ),
            vec![Some("A".into()), Some("B".into())]
        );
    }
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
    fn input_sources_come_from_vcp_60() {
        let caps = "(prot(monitor)type(LCD)vcp(02 10 12 14(05 08 0B) 60( 11 12 0F)                     D6(01 04 05) DF)mccs_ver(2.1))";
        assert_eq!(input_sources(caps), vec![0x11, 0x12, 0x0f]);
        assert!(has_power(caps));
        assert_eq!(input_sources("(vcp(60(0F1112)))"), vec![0x0f, 0x11, 0x12]);
        assert!(input_sources("(vcp(10 12))").is_empty());
        assert!(input_sources("(vcp(10(60 11)))").is_empty());
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
