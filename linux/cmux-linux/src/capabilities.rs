use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy)]
pub struct PlatformInfo {
    pub id: &'static str,
    pub frontend: &'static str,
    pub window_multi: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct FeatureFlags {
    pub terminal: bool,
    pub workspace: bool,
    pub pane: bool,
    pub surface: bool,
    pub notification: bool,
    pub session_restore: bool,
    pub window_multi: bool,
    pub browser: bool,
    pub debug: bool,
}

#[derive(Debug, Clone)]
pub struct CapabilityProfile {
    pub platform: PlatformInfo,
    pub features: FeatureFlags,
    pub unsupported_methods: BTreeSet<&'static str>,
}

impl CapabilityProfile {
    pub fn enabled_feature_labels(&self) -> Vec<&'static str> {
        let mut labels = Vec::new();

        if self.features.terminal {
            labels.push("terminal");
        }
        if self.features.workspace {
            labels.push("workspace");
        }
        if self.features.pane {
            labels.push("pane");
        }
        if self.features.surface {
            labels.push("surface");
        }
        if self.features.notification {
            labels.push("notification");
        }
        if self.features.session_restore {
            labels.push("session_restore");
        }
        if self.features.window_multi {
            labels.push("window_multi");
        }
        if self.features.browser {
            labels.push("browser");
        }
        if self.features.debug {
            labels.push("debug");
        }

        labels
    }
}

pub fn linux_v1_capabilities() -> CapabilityProfile {
    let unsupported_methods = [
        "browser.open_split",
        "browser.navigate",
        "browser.back",
        "browser.forward",
        "browser.reload",
        "browser.url.get",
        "browser.snapshot",
        "browser.eval",
        "browser.wait",
    ]
    .into_iter()
    .collect();

    CapabilityProfile {
        platform: PlatformInfo {
            id: "linux",
            frontend: "gtk4-libadwaita",
            window_multi: true,
        },
        features: FeatureFlags {
            terminal: true,
            workspace: true,
            pane: true,
            surface: true,
            notification: true,
            session_restore: true,
            window_multi: true,
            browser: false,
            debug: true,
        },
        unsupported_methods,
    }
}
