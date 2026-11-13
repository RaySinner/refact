use refact_core::trajectory_settings::TrajectoryRuntimeSettings;
use refact_runtime_api::runtime_settings::{install_live, reset_for_test};

pub struct SettingsGuard;

impl SettingsGuard {
    pub fn install(mutate: impl FnOnce(&mut TrajectoryRuntimeSettings)) -> Self {
        let mut settings = TrajectoryRuntimeSettings::default();
        mutate(&mut settings);
        install_live(&settings);
        Self
    }
}

impl Drop for SettingsGuard {
    fn drop(&mut self) {
        reset_for_test();
    }
}
