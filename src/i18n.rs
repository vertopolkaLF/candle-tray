//! Embedded Fluent catalogs with per-message English fallback and live selection.
pub use crate::settings::Language;
use fluent_bundle::{FluentArgs, FluentBundle, FluentResource};
use std::{
    cell::RefCell,
    sync::atomic::{AtomicU8, Ordering},
};
const EN: &str = include_str!("../locales/en/app.ftl");
const RU: &str = include_str!("../locales/ru/app.ftl");
static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn set_language(language: Language) {
    CURRENT.store(
        match language {
            Language::System => 0,
            Language::En => 1,
            Language::Ru => 2,
        },
        Ordering::Release,
    );
}
pub fn current_language() -> Language {
    match CURRENT.load(Ordering::Acquire) {
        1 => Language::En,
        2 => Language::Ru,
        _ => system_language(),
    }
}
fn system_language() -> Language {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetUserDefaultUILanguage() -> u16;
        }
        if unsafe { GetUserDefaultUILanguage() } & 0x03ff == 0x19 {
            return Language::Ru;
        }
    }
    Language::En
}
fn bundle(tag: &str, source: &str) -> FluentBundle<FluentResource> {
    let resource = FluentResource::try_new(source.into()).expect("valid embedded Fluent catalog");
    let mut bundle = FluentBundle::new(vec![tag.parse().expect("language tag")]);
    bundle.set_use_isolating(false);
    bundle.add_resource(resource).expect("unique message ids");
    bundle
}
struct Catalogs {
    en: FluentBundle<FluentResource>,
    ru: FluentBundle<FluentResource>,
}
impl Catalogs {
    fn new() -> Self {
        Self {
            en: bundle("en", EN),
            ru: bundle("ru", RU),
        }
    }
    fn resolve(
        &self,
        language: Language,
        key: &str,
        args: Option<&FluentArgs<'_>>,
    ) -> Option<String> {
        let resolve = |bundle: &FluentBundle<FluentResource>| {
            let message = bundle.get_message(key)?;
            let mut errors = Vec::new();
            let result = bundle
                .format_pattern(message.value()?, args, &mut errors)
                .into_owned();
            (errors.is_empty() && !result.trim().is_empty()).then_some(result)
        };
        resolve(if language == Language::Ru {
            &self.ru
        } else {
            &self.en
        })
        .or_else(|| resolve(&self.en))
    }
}
thread_local! { static CATALOGS: RefCell<Catalogs> = RefCell::new(Catalogs::new()); }
pub fn tr(key: &str) -> String {
    translate(current_language(), key, None)
}
pub fn tr_args(key: &str, args: &FluentArgs<'_>) -> String {
    translate(current_language(), key, Some(args))
}
/// `tr_args` with plain string arguments.
pub fn format(key: &str, args: &[(&str, String)]) -> String {
    let mut fluent = FluentArgs::new();
    for (name, value) in args {
        fluent.set(*name, value.clone());
    }
    tr_args(key, &fluent)
}
fn translate(language: Language, key: &str, args: Option<&FluentArgs<'_>>) -> String {
    CATALOGS
        .with(|catalogs| catalogs.borrow().resolve(language, key, args))
        .unwrap_or_else(|| key.to_owned())
}
#[cfg(test)]
mod tests {
    use super::*;
    use fluent_syntax::ast::{Entry, Expression, InlineExpression, Pattern, PatternElement};
    use std::collections::{BTreeMap, BTreeSet};

    fn inline_variables(expression: &InlineExpression<&str>, result: &mut BTreeSet<String>) {
        match expression {
            InlineExpression::VariableReference { id } => {
                result.insert(id.name.into());
            }
            InlineExpression::Placeable { expression } => expression_variables(expression, result),
            _ => {}
        }
    }
    fn expression_variables(expression: &Expression<&str>, result: &mut BTreeSet<String>) {
        match expression {
            Expression::Inline(inline) => inline_variables(inline, result),
            Expression::Select { selector, variants } => {
                inline_variables(selector, result);
                for variant in variants {
                    pattern_variables(&variant.value, result);
                }
            }
        }
    }
    fn pattern_variables(pattern: &Pattern<&str>, result: &mut BTreeSet<String>) {
        for element in &pattern.elements {
            if let PatternElement::Placeable { expression } = element {
                expression_variables(expression, result);
            }
        }
    }
    fn catalog_parameters(source: &str) -> BTreeMap<String, BTreeSet<String>> {
        let resource = fluent_syntax::parser::parse(source).expect("valid Fluent syntax");
        let mut messages = BTreeMap::new();
        for entry in resource.body {
            if let Entry::Message(message) = entry {
                let mut variables = BTreeSet::new();
                pattern_variables(
                    message.value.as_ref().expect("message value"),
                    &mut variables,
                );
                assert!(
                    messages.insert(message.id.name.into(), variables).is_none(),
                    "duplicate message ID"
                );
            }
        }
        messages
    }

    #[test]
    fn locales_match_english_messages_and_parameters() {
        let expected = catalog_parameters(EN);
        assert_eq!(catalog_parameters(RU), expected);
        for (key, params) in expected {
            let mut args = FluentArgs::new();
            for param in &params {
                args.set(param.as_str(), "42");
            }
            for language in [Language::En, Language::Ru] {
                assert!(
                    Catalogs::new()
                        .resolve(language, &key, Some(&args))
                        .is_some(),
                    "{key}"
                );
            }
        }
    }
    #[test]
    fn missing_or_broken_translation_falls_back_per_message() {
        let catalogs = Catalogs {
            en: bundle("en", "greeting = Hello\nvalue = Value { $count }"),
            ru: bundle("ru", "value = Значение { $missing }"),
        };
        assert_eq!(
            catalogs.resolve(Language::Ru, "greeting", None).unwrap(),
            "Hello"
        );
        let mut args = FluentArgs::new();
        args.set("count", 7);
        assert_eq!(
            catalogs
                .resolve(Language::Ru, "value", Some(&args))
                .unwrap(),
            "Value 7"
        );
    }
    #[test]
    fn live_language_switch_and_arguments() {
        set_language(Language::Ru);
        assert_eq!(tr("tray-exit"), "Выход");
        let mut args = FluentArgs::new();
        args.set("percent", 50);
        assert_eq!(tr_args("tray-tooltip", &args), "Candle Tray — 50%");
        set_language(Language::En);
        assert_eq!(tr("tray-exit"), "Exit");
        set_language(Language::System);
    }
}
