//! Pure command-line parsing and display selection. No hardware or UI access.
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Selector {
    All,
    PartialId(String),
    Position(usize),
    Id(String),
    NameOrId(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Feedback {
    Overlay,
    Panel,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    List,
    WithFeedback(Box<Command>, Feedback),
    Vcp(u8, u16, Selector),
    /// Explicit ordered actions, also accepted by single-instance IPC.
    Actions(Vec<crate::actions::Action>),
    Set(u8, Selector),
    Offset(i32, Selector),
    Contrast(u8, Selector),
    Profile(String),
    Off,
    Input(u8, Selector),
    Show,
    InstallUpdate(String),
    /// Switch Windows Night Light on or off; `None` toggles it.
    NightLight(Option<bool>),
    /// Windows Night Light strength, 0..=100.
    NightLightStrength(u8),
}

fn position(value: &str) -> Result<usize> {
    let number = value.parse::<usize>()?;
    ensure!(number > 0, "display positions start at 1");
    Ok(number)
}

fn selector(value: &str) -> Result<Selector> {
    ensure!(!value.trim().is_empty(), "empty display selector");
    ensure!(
        !value
            .strip_prefix('-')
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|c| c.is_ascii_digit())),
        "display positions start at 1"
    );
    if value.eq_ignore_ascii_case("all") {
        Ok(Selector::All)
    } else if value.bytes().all(|c| c.is_ascii_digit()) {
        Ok(Selector::Position(position(value)?))
    } else {
        Ok(Selector::NameOrId(value.to_owned()))
    }
}

fn percent(value: &str) -> Result<u8> {
    let value = value.parse::<u8>()?;
    ensure!(value <= 100, "percentage must be in 0..=100");
    Ok(value)
}

fn input(value: &str) -> Result<u8> {
    match value.to_ascii_lowercase().as_str() {
        "hdmi1" => Ok(0x11),
        "hdmi2" => Ok(0x12),
        "dp1" => Ok(0x0f),
        "dp2" => Ok(0x10),
        "usb-c" => Ok(0x1b),
        hex if hex.starts_with("0x") => {
            let value = u8::from_str_radix(&hex[2..], 16)?;
            ensure!(value != 0, "input source must be nonzero");
            Ok(value)
        }
        _ => bail!("unknown input source: {value}"),
    }
}

/// Arguments exclude the executable name. Empty arguments retain normal startup.
/// One action and at most one selector are accepted, in either order.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Option<Command>> {
    let args = args.into_iter().collect::<Vec<_>>();
    if let [uri] = args.as_slice() {
        if uri == crate::notifications::SHOW_URI {
            return Ok(Some(Command::Show));
        }
        if let Some(version) = uri.strip_prefix(crate::notifications::INSTALL_URI_PREFIX) {
            let version = semver::Version::parse(version)?;
            return Ok(Some(Command::InstallUpdate(version.to_string())));
        }
    }
    let mut args = args.into_iter();
    let mut action = None;
    let mut feedback = None;
    let mut target = None;
    while let Some(arg) = args.next() {
        let (flag, inline) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(k, v)| (k, Some(v)));
        let flag = flag.to_ascii_lowercase();
        let takes_value = match flag.as_str() {
            "--all" | "--off" | "--show" | "--list" | "--panel" | "--overlay" => false,
            "--display"
            | "--monitornum"
            | "--monitorid"
            | "--set"
            | "--offset"
            | "--contrast"
            | "--profile"
            | "--input"
            | "--actions"
            | "--vcp"
            | "--nightlight"
            | "--nightlightstrength" => true,
            _ => bail!("unknown argument: {arg}"),
        };
        let value = if takes_value {
            let value = inline
                .map(str::to_owned)
                .or_else(|| args.next())
                .ok_or_else(|| anyhow::anyhow!("missing value for {flag}"))?;
            ensure!(
                !value.trim().is_empty() && !value.starts_with("--"),
                "missing value for {flag}"
            );
            value
        } else {
            ensure!(inline.is_none(), "{flag} does not take a value");
            String::new()
        };
        if flag == "--panel" || flag == "--overlay" {
            ensure!(feedback.is_none(), "only one presentation mode is allowed");
            feedback = Some(if flag == "--panel" {
                Feedback::Panel
            } else {
                Feedback::Overlay
            });
            continue;
        }
        let selected = match flag.as_str() {
            "--display" => Some(selector(&value)?),
            "--monitornum" => Some(Selector::Position(position(&value)?)),
            "--monitorid" => Some(Selector::PartialId(value.clone())),
            "--all" => Some(Selector::All),
            _ => None,
        };
        if let Some(selected) = selected {
            ensure!(
                target.replace(selected).is_none(),
                "only one display selector is allowed"
            );
            continue;
        }
        let command = match flag.as_str() {
            "--actions" => {
                let actions: Vec<crate::actions::Action> = serde_json::from_str(&value)?;
                ensure!(!actions.is_empty(), "action sequence is empty");
                for action in &actions {
                    action.validate()?;
                }
                Command::Actions(actions)
            }
            "--list" => Command::List,
            "--vcp" => {
                let (code, value) = value
                    .split_once(':')
                    .ok_or_else(|| anyhow::anyhow!("VCP requires code:value"))?;
                let parse = |s: &str| -> Result<u16> {
                    Ok(
                        if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                            u16::from_str_radix(hex, 16)?
                        } else {
                            s.parse()?
                        },
                    )
                };
                let code = u8::try_from(parse(code)?)?;
                ensure!(code != 0, "VCP code must be nonzero");
                let value = parse(value)?;
                crate::actions::Action {
                    selector: Selector::All,
                    control: crate::actions::Control::Vcp(code),
                    operation: crate::actions::Operation::Set(i32::from(value)),
                }
                .validate()?;
                Command::Vcp(code, value, Selector::All)
            }
            "--set" => Command::Set(percent(&value)?, Selector::All),
            "--offset" => Command::Offset(value.parse::<i32>()?, Selector::All),
            "--contrast" => Command::Contrast(percent(&value)?, Selector::All),
            "--profile" => {
                if value.bytes().all(|c| c.is_ascii_digit()) {
                    position(&value)?;
                }
                Command::Profile(value)
            }
            "--input" => Command::Input(input(&value)?, Selector::All),
            "--off" => Command::Off,
            "--show" => Command::Show,
            "--nightlight" => Command::NightLight(match value.to_ascii_lowercase().as_str() {
                "on" => Some(true),
                "off" => Some(false),
                "toggle" => None,
                _ => bail!("--NightLight takes on, off or toggle"),
            }),
            "--nightlightstrength" => Command::NightLightStrength(percent(&value)?),
            _ => unreachable!(),
        };
        ensure!(
            action.replace(command).is_none(),
            "only one command is allowed"
        );
    }
    if let Some(command) = action.as_mut() {
        match command {
            Command::Set(_, selected)
            | Command::Offset(_, selected)
            | Command::Contrast(_, selected)
            | Command::Vcp(_, _, selected) => {
                *selected = target.take().unwrap_or(Selector::All);
            }
            Command::Input(_, selected) => {
                *selected = target
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("--input requires --display"))?;
            }
            _ => ensure!(
                target.is_none(),
                "this command does not accept a display selector"
            ),
        }
    } else {
        ensure!(target.is_none(), "display selector requires a command");
    }
    if let Some(feedback) = feedback {
        ensure!(
            feedback != Feedback::Overlay || action.is_some(),
            "--Overlay requires a command"
        );
        let command = action.unwrap_or(Command::Show);
        ensure!(
            command != Command::List,
            "--List does not accept presentation flags"
        );
        action = Some(Command::WithFeedback(Box::new(command), feedback));
    }
    Ok(action)
}

impl Selector {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Position(position) => ensure!(*position > 0, "display positions start at 1"),
            Self::Id(value) | Self::PartialId(value) | Self::NameOrId(value) => {
                ensure!(!value.trim().is_empty(), "empty display selector")
            }
            Self::All => {}
        }
        Ok(())
    }
    /// Entries must already be in the user's visible display order. Exact IDs
    /// take precedence over names; a name substring selects every matching row.
    pub fn resolve(&self, entries: &[(&str, &str)]) -> Result<Vec<usize>> {
        self.validate()?;
        let indices = match self {
            Self::All => (0..entries.len()).collect(),
            Self::Position(position) => position
                .checked_sub(1)
                .filter(|index| *index < entries.len())
                .into_iter()
                .collect(),
            Self::PartialId(id) => {
                let exact = entries
                    .iter()
                    .position(|(candidate, _)| candidate.eq_ignore_ascii_case(id));
                if let Some(index) = exact {
                    vec![index]
                } else {
                    let query = id.to_lowercase();
                    entries
                        .iter()
                        .enumerate()
                        .filter(|(_, (candidate, _))| candidate.to_lowercase().contains(&query))
                        .map(|(index, _)| index)
                        .collect()
                }
            }
            Self::Id(id) => entries
                .iter()
                .enumerate()
                .filter(|(_, (candidate, _))| candidate.eq_ignore_ascii_case(id))
                .map(|(index, _)| index)
                .collect(),
            Self::NameOrId(query) => {
                if let Some(index) = entries
                    .iter()
                    .position(|(id, _)| id.eq_ignore_ascii_case(query))
                {
                    vec![index]
                } else {
                    let query = query.to_lowercase();
                    entries
                        .iter()
                        .enumerate()
                        .filter(|(_, (_, name))| name.to_lowercase().contains(&query))
                        .map(|(index, _)| index)
                        .collect()
                }
            }
        };
        ensure!(!indices.is_empty(), "no displays match {self:?}");
        Ok(indices)
    }
}

/// Read-only inventory for scripts. Does not acquire app ownership or create UI.
pub fn list_displays() -> Result<String> {
    use crate::monitors::{Event, MonitorService};
    let (sender, receiver) = std::sync::mpsc::channel();
    let service = MonitorService::start(move |event| {
        let _ = sender.send(event);
    });
    service.send(crate::monitors::Command::ReadInventory);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
    loop {
        let event =
            receiver.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))?;
        if let Event::Inventory(mut monitors) = event {
            let settings = crate::settings::Settings::default_path()
                .ok()
                .and_then(|path| std::fs::read_to_string(path).ok())
                .and_then(|text| toml::from_str::<crate::settings::Settings>(&text).ok())
                .unwrap_or_default();
            monitors.sort_by_key(|monitor| {
                (
                    settings
                        .order
                        .iter()
                        .position(|id| *id == monitor.id)
                        .unwrap_or(usize::MAX),
                    monitor.num,
                )
            });
            let mut position = 0usize;
            let rows: Vec<_> = monitors
                .into_iter()
                .map(|monitor| {
                    let entry = settings.monitor(&monitor.id);
                    let selector_number = if entry.hidden { None } else { position += 1; Some(position) };
                    serde_json::json!({ "id": monitor.id, "number": selector_number, "windows_number": monitor.num,
                    "name": entry.name.as_deref().unwrap_or(&monitor.name), "hidden": entry.hidden,
                    "brightness": monitor.brightness, "sdr": monitor.sdr.map(|v|v.percent),
                    "contrast": monitor.contrast, "inputs": monitor.inputs,
                    "vcp_codes": monitor.vcp_codes, "unresponsive": monitor.unresponsive })
                })
                .collect();
            return Ok(serde_json::to_string_pretty(&rows)?);
        }
    }
}

/// Profile names use the same case folding as display names. Numeric selectors
/// are one-based; names must identify exactly one profile.
pub fn resolve_profile(query: &str, names: &[String]) -> Result<usize> {
    if query.bytes().all(|c| c.is_ascii_digit()) {
        let index = position(query)? - 1;
        ensure!(index < names.len(), "profile number out of range");
        return Ok(index);
    }
    let matches: Vec<_> = names
        .iter()
        .enumerate()
        .filter(|(_, name)| name.to_lowercase() == query.to_lowercase())
        .map(|(index, _)| index)
        .collect();
    ensure!(
        matches.len() == 1,
        "profile name must match exactly one profile: {query}"
    );
    Ok(matches[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twinkle_vcp_feedback_and_partial_id_commands() {
        assert_eq!(parse_args(&["--List"]).unwrap(), Some(Command::List));
        assert_eq!(
            parse_args(&["--VCP=0x62:35", "--MonitorID=UID", "--Overlay"]).unwrap(),
            Some(Command::WithFeedback(
                Box::new(Command::Vcp(0x62, 35, Selector::PartialId("UID".into()))),
                Feedback::Overlay
            ))
        );
        assert_eq!(
            parse_args(&["--Panel"]).unwrap(),
            Some(Command::WithFeedback(
                Box::new(Command::Show),
                Feedback::Panel
            ))
        );
        let rows = [("DISPLAY#UID1", "Dell"), ("DISPLAY#UID2", "LG")];
        assert_eq!(
            Selector::PartialId("uid".into()).resolve(&rows).unwrap(),
            vec![0, 1]
        );
        assert!(Selector::Id("uid".into()).resolve(&rows).is_err());
        for args in [
            vec!["--VCP=0x100:1"],
            vec!["--VCP=0:1"],
            vec!["--VCP=0x60:0"],
            vec!["--VCP=0xD6:6"],
            vec!["--List", "--Panel"],
            vec!["--Set=50", "--Overlay", "--Panel"],
        ] {
            assert!(parse_args(&args).is_err(), "{args:?}");
        }
    }
    #[test]
    fn explicit_sequences_validate_before_ipc_and_keep_order() {
        use crate::actions::{Action, Control, Operation};
        let actions = vec![
            Action {
                selector: Selector::Id("desk".into()),
                control: Control::Power,
                operation: Operation::Set(1),
            },
            Action {
                selector: Selector::All,
                control: Control::Sdr,
                operation: Operation::Cycle(vec![20, 60]),
            },
            Action {
                selector: Selector::All,
                control: Control::Vcp(0x12),
                operation: Operation::Offset(-10),
            },
        ];
        let json = serde_json::to_string(&actions).unwrap();
        let command = parse_args(&["--actions", &json]).unwrap().unwrap();
        assert_eq!(command, Command::Actions(actions));
        assert_eq!(
            serde_json::from_slice::<Command>(&serde_json::to_vec(&command).unwrap()).unwrap(),
            command
        );
        for json in [
            "[]",
            "{}",
            r#"[{"selector":"All","control":{"Vcp":0},"operation":{"Set":1}}]"#,
            r#"[{"selector":"All","control":"Power","operation":{"Cycle":[1,6]}}]"#,
        ] {
            assert!(parse_args(&["--actions", json]).is_err());
        }
        assert!(parse_args(&["--actions", &json, "--display", "1"]).is_err());
    }
    fn parse_args(args: &[&str]) -> Result<Option<Command>> {
        parse(args.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn modern_commands_and_defaults() {
        for (args, expected) in [
            (vec![], None),
            (vec!["--set", "0"], Some(Command::Set(0, Selector::All))),
            (
                vec!["--set=100", "--display", "2"],
                Some(Command::Set(100, Selector::Position(2))),
            ),
            (
                vec!["--offset", "-10", "--display=LG"],
                Some(Command::Offset(-10, Selector::NameOrId("LG".into()))),
            ),
            (
                vec!["--contrast=70"],
                Some(Command::Contrast(70, Selector::All)),
            ),
            (
                vec!["--profile", "Night"],
                Some(Command::Profile("Night".into())),
            ),
            (vec!["--profile=2"], Some(Command::Profile("2".into()))),
            (vec!["--off"], Some(Command::Off)),
            (vec!["--show"], Some(Command::Show)),
        ] {
            assert_eq!(parse_args(&args).unwrap(), expected, "{args:?}");
        }
    }

    #[test]
    fn notification_protocol_is_strict_and_survives_ipc_serialization() {
        let command = parse_args(&["candle-tray://install-update/1.2.3"])
            .unwrap()
            .unwrap();
        assert_eq!(command, Command::InstallUpdate("1.2.3".into()));
        let payload = serde_json::to_vec(&command).unwrap();
        assert_eq!(
            serde_json::from_slice::<Command>(&payload).unwrap(),
            command
        );
        assert_eq!(
            parse_args(&["candle-tray://show"]).unwrap(),
            Some(Command::Show)
        );
        for args in [
            vec!["candle-tray://install-update/"],
            vec!["candle-tray://install-update/1.2.3?url=https://example.com"],
            vec!["candle-tray://install-update/1.2.3", "--off"],
        ] {
            assert!(parse_args(&args).is_err());
        }
    }

    #[test]
    fn twinkle_spellings_and_both_value_forms() {
        for args in [
            vec!["--MonitorNum=2", "--Set=50"],
            vec!["--Set", "50", "--MonitorNum", "2"],
        ] {
            assert_eq!(
                parse_args(&args).unwrap(),
                Some(Command::Set(50, Selector::Position(2)))
            );
        }
        for args in [
            vec!["--All", "--Offset=-30"],
            vec!["--Offset", "-30", "--All"],
        ] {
            assert_eq!(
                parse_args(&args).unwrap(),
                Some(Command::Offset(-30, Selector::All))
            );
        }
        for args in [
            vec!["--MonitorID=DISPLAY1", "--Set=50"],
            vec!["--Set", "50", "--MonitorID", "DISPLAY1"],
        ] {
            assert_eq!(
                parse_args(&args).unwrap(),
                Some(Command::Set(50, Selector::PartialId("DISPLAY1".into())))
            );
        }
    }

    #[test]
    fn input_names_and_hex() {
        for (name, expected) in [
            ("HDMI1", 0x11),
            ("hdmi2", 0x12),
            ("DP1", 0x0f),
            ("DP2", 0x10),
            ("USB-C", 0x1b),
            ("0X11", 0x11),
        ] {
            assert_eq!(
                parse_args(&["--input", name, "--display", "1"]).unwrap(),
                Some(Command::Input(expected, Selector::Position(1)))
            );
        }
    }

    #[test]
    fn invalid_arguments_are_rejected() {
        for args in [
            vec!["--unknown"],
            vec!["--Overlay"],
            vec!["50"],
            vec!["--set"],
            vec!["--set="],
            vec!["--set", "--show"],
            vec!["--set=-1"],
            vec!["--set=101"],
            vec!["--set=1.5"],
            vec!["--contrast=256"],
            vec!["--offset=NaN"],
            vec!["--offset=2147483648"],
            vec!["--display=0", "--set=1"],
            vec!["--display=-1", "--set=1"],
            vec!["--display="],
            vec!["--MonitorNum=-1", "--set=1"],
            vec!["--MonitorNum=abc", "--set=1"],
            vec!["--MonitorID=", "--set=1"],
            vec!["--All"],
            vec!["--set=1", "--off"],
            vec!["--set=1", "--set=2"],
            vec!["--All", "--display=1", "--set=1"],
            vec!["--off=true"],
            vec!["--show", "--display=1"],
            vec!["--off", "--All"],
            vec!["--profile=0"],
            vec!["--profile=Night", "--display=1"],
            vec!["--input=HDMI1"],
            vec!["--input=bogus", "--display=1"],
            vec!["--input=0x100", "--display=1"],
            vec!["--input=0x00", "--display=1"],
        ] {
            assert!(parse_args(&args).is_err(), "accepted {args:?}");
        }
    }

    #[test]
    fn display_selection_uses_supplied_order_and_exact_ids_first() {
        let rows = [
            ("id-b", "Dell office"),
            ("id-a", "DELL home"),
            ("id-c", "id-a"),
        ];
        assert_eq!(Selector::All.resolve(&rows).unwrap(), vec![0, 1, 2]);
        assert_eq!(Selector::Position(1).resolve(&rows).unwrap(), vec![0]);
        assert_eq!(
            Selector::NameOrId("dell".into()).resolve(&rows).unwrap(),
            vec![0, 1]
        );
        assert_eq!(
            Selector::NameOrId("ID-A".into()).resolve(&rows).unwrap(),
            vec![1]
        );
        assert_eq!(Selector::Id("ID-B".into()).resolve(&rows).unwrap(), vec![0]);
        for selector in [
            Selector::Position(0),
            Selector::Position(4),
            Selector::Id("id".into()),
            Selector::NameOrId("missing".into()),
        ] {
            assert!(selector.resolve(&rows).is_err());
        }
        assert!(Selector::All.resolve(&[]).is_err());
    }

    #[test]
    fn night_light_commands() {
        assert_eq!(
            parse_args(&["--NightLight=On"]).unwrap(),
            Some(Command::NightLight(Some(true)))
        );
        assert_eq!(
            parse_args(&["--nightlight", "off"]).unwrap(),
            Some(Command::NightLight(Some(false)))
        );
        assert_eq!(
            parse_args(&["--NightLight=toggle"]).unwrap(),
            Some(Command::NightLight(None))
        );
        assert_eq!(
            parse_args(&["--NightLightStrength=35"]).unwrap(),
            Some(Command::NightLightStrength(35))
        );
        for args in [
            vec!["--NightLight=maybe"],
            vec!["--NightLightStrength=101"],
            vec!["--NightLight=on", "--display=1"],
            vec!["--NightLight=on", "--set=50"],
        ] {
            assert!(parse_args(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn profile_selection_and_wire_round_trip() {
        let names = vec!["Night".into(), "День".into()];
        assert_eq!(resolve_profile("nIGHT", &names).unwrap(), 0);
        assert_eq!(resolve_profile("день", &names).unwrap(), 1);
        assert_eq!(resolve_profile("2", &names).unwrap(), 1);
        for query in ["0", "3", "missing", "18446744073709551616"] {
            assert!(resolve_profile(query, &names).is_err());
        }
        assert!(resolve_profile("Night", &["Night".into(), "night".into()]).is_err());
        let command = parse_args(&["--offset=-2147483648"]).unwrap().unwrap();
        assert_eq!(
            serde_json::from_slice::<Command>(&serde_json::to_vec(&command).unwrap()).unwrap(),
            command
        );
    }
}
