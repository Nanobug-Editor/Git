//! Exercise declared settings, atomic validation and the actual serialized hot-configuration entry.
use super::*;
use plugin_protocol::settings::{Apply, EffectiveValue, Scope};

/// One explicit user value replaces a declared color while other slots keep their defaults.
fn values(key: &str, color: &str) -> Effective {
    BTreeMap::from([(
        key.into(),
        EffectiveValue {
            value: serde_json::json!(color),
            source: Source::User,
        },
    )])
}

/// Public manifest declarations and rendering defaults cannot drift apart during future palette edits.
#[test]
fn manifest_palette_settings_are_user_only_and_hot() {
    let manifest: plugin_protocol::Manifest =
        serde_json::from_str(include_str!("../../../manifest.json")).unwrap();
    assert!(manifest.settings_hook);
    let requirement = &manifest.api.unwrap().required["configuration"];
    assert!(requirement.matches(&"1.1.0".parse().unwrap()));
    assert!(!requirement.matches(&"1.0.0".parse().unwrap()));
    assert!(!requirement.matches(&"2.0.0".parse().unwrap()));
    assert_eq!(manifest.settings.len(), graph::COLOR_COUNT * 2);
    for dark in [false, true] {
        for (index, color) in Palette::defaults(dark).into_iter().enumerate() {
            let key = format!(
                "graph.colors.{}.{}",
                if dark { "dark" } else { "light" },
                index + 1
            );
            let definition = &manifest.settings[&key];
            assert!(definition.valid());
            assert_eq!(definition.scope, Scope::User);
            assert_eq!(definition.apply, Apply::HotApply);
            assert_eq!(definition.default, format!("#{color:06x}"));
        }
    }
}

/// Accept both hex cases, distinguish both theme maps and reject malformed explicit settings.
#[test]
fn parse_palette_colors_and_reject_project_overrides() {
    assert_eq!(
        Palette::parse(&Effective::default()).unwrap(),
        Palette::default()
    );
    let palette = Palette::parse(&values("graph.colors.dark.8", "#12aB34")).unwrap();
    assert_eq!(palette.dark[7], 0x12ab34);
    assert_eq!(palette.light, Palette::defaults(false));
    for invalid in [
        "",
        "12ab34",
        "#123",
        "#12gg34",
        "#1234567",
        "#你好啊",
        "#12 456",
    ] {
        let error = Palette::parse(&values("graph.colors.light.1", invalid)).unwrap_err();
        assert!(error.errors.contains_key("graph.colors.light.1"));
    }
    let mut project = values("graph.colors.light.1", "#123456");
    project.values_mut().next().unwrap().source = Source::Project;
    assert!(Palette::parse(&project).is_err());
}

/// Validation and a failed application leave all previously accepted colors untouched.
#[test]
fn palette_validation_is_pure_and_apply_is_atomic() {
    let mut state = State::default();
    let valid = values("graph.colors.dark.1", "#123456");
    let proposal = state
        .configure_palette(Phase::Validate, &valid)
        .unwrap()
        .configuration
        .unwrap();
    assert!(proposal.errors.is_empty());
    assert_eq!(state.history_palette, Palette::default());
    state.configure_palette(Phase::Apply, &valid).unwrap();
    let accepted = state.history_palette.clone();
    let mut invalid = values("graph.colors.light.1", "#987654");
    invalid.extend(values("graph.colors.dark.8", "#xxxxxx"));
    assert!(state.configure_palette(Phase::Changed, &invalid).is_err());
    assert_eq!(state.history_palette, accepted);
    assert_eq!(state.revision, 0);
}

/// Serialized configuration bypasses normal query interruption and republishes only existing history.
#[test]
fn hot_palette_dispatch_keeps_queries_selection_scroll_and_commit_draft() {
    let handle = api::ResourceHandle {
        instance: "same-owner".into(),
        scope: "workspace".into(),
        resource: 3,
    };
    let mut state = State {
        canvas_width: 1000.,
        canvas_height: 280.,
        log_scroll: 84.,
        commit_message: "草稿 draft".into(),
        commits: (0..36)
            .map(|index| Commit {
                oid: index.to_string(),
                parents: vec![(index + 1).to_string()],
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    state.selected_commits.push("3".into());
    let initial = state.history_scroll_view();
    state.changed_views(initial);
    let epoch = state.history_drawing.epoch;
    state.running = Some(Running {
        handle: handle.clone(),
        command: Command::new(["log"], Purpose::Refs),
        output: Vec::new(),
        stderr: Vec::new(),
    });
    state
        .queue
        .push_back(Command::new(["status"], Purpose::Refs));
    state.automatic_observation = true;
    state.background_read = true;
    STATE.with(|cell| *cell.borrow_mut() = state);
    let input = api::Invocation {
        id: 42,
        message: api::Input::Event {
            panel: None,
            event: api::Notification::Configuration {
                phase: Phase::Changed,
                values: values("graph.colors.light.1", "#12ab34"),
            },
        },
    };
    let reply: api::Completion =
        serde_json::from_str(&feedback::dispatch(serde_json::to_string(&input).unwrap()).unwrap())
            .unwrap();
    let output = reply.result.unwrap();
    assert_eq!(output.views.len(), 1);
    assert_eq!(output.views[0].panel, "history");
    assert!(
        serde_json::to_string(&output.views)
            .unwrap()
            .contains("#12ab34")
    );
    let mut state = STATE.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
    assert_eq!(state.running.as_ref().unwrap().handle, handle);
    assert_eq!(state.queue.len(), 1);
    assert!(state.automatic_observation && state.background_read);
    assert_eq!(state.commit_message, "草稿 draft");
    assert_eq!(state.log_scroll, 84.);
    assert_eq!(state.selected_commits, vec!["3".to_owned()]);
    assert_eq!(state.history_drawing.epoch, epoch);
    assert!(
        !state
            .configure_palette(Phase::Changed, &values("graph.colors.dark.1", "#abcdef"))
            .unwrap()
            .views
            .is_empty()
    );
    // The complete effective map in this call restores the light slot while changing dark.
    let palette = state.history_palette.clone();
    let mut only_dark = values("graph.colors.dark.1", "#fedcba");
    only_dark.extend(values(
        "graph.colors.light.1",
        &format!("#{:06x}", palette.light[0]),
    ));
    assert!(
        state
            .configure_palette(Phase::Changed, &only_dark)
            .unwrap()
            .views
            .is_empty()
    );
    state.visible_panels.insert("history".into(), false);
    only_dark.extend(values("graph.colors.light.1", "#654321"));
    assert_eq!(
        state
            .configure_palette(Phase::Changed, &only_dark)
            .unwrap()
            .views
            .len(),
        1
    );
    assert_eq!(
        state.visible_panels["history"], false,
        "refreshing a cached view must not open its panel"
    );
}
