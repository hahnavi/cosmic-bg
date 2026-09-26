// SPDX-License-Identifier: MPL-2.0

pub mod state;

use cosmic_config::{Config as CosmicConfig, ConfigGet, ConfigSet};
use derive_setters::Setters;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashSet;
use std::path::PathBuf;

pub const NAME: &str = "com.system76.CosmicBackground";
pub const BACKGROUNDS: &str = "backgrounds";
pub const DEFAULT_BACKGROUND: &str = "all";
pub const SAME_ON_ALL: &str = "same-on-all";

/// Returns the cosmic-config storage key for an output.
/// "all" is stored as "all", while a specific output like "DP-1" is stored as "output.DP-1".
#[must_use]
pub fn output_storage_key(output: &str) -> String {
    if output == "all" {
        "all".to_string()
    } else {
        format!("output.{output}")
    }
}

/// Parses a storage key into an output name.
/// "all" returns Some("all"), "output.DP-1" returns Some("DP-1"), others return None.
#[must_use]
pub fn parse_output_storage_key(key: &str) -> Option<&str> {
    if key == "all" {
        Some("all")
    } else {
        key.strip_prefix("output.")
    }
}

/// Create a context to the `cosmic-bg` config.
///
/// # Errors
///
/// Fails if cosmic-config paths are missing or cannot be created.
pub fn context() -> Result<Context, cosmic_config::Error> {
    CosmicConfig::new(NAME, 1).map(Context)
}

#[derive(Clone, Debug)]
pub struct Context(pub CosmicConfig);

impl Context {
    /// Get all stored backgrounds from cosmic-config.
    ///
    /// # Errors
    ///
    /// Fails if the config is missing or fails to parse.
    pub fn backgrounds(&self) -> Vec<String> {
        match self.0.get::<Vec<String>>(BACKGROUNDS) {
            Ok(value) => value,
            Err(why) => {
                tracing::error!(?why, "error reading background config");
                Vec::new()
            }
        }
    }

    pub fn default_background(&self) -> Entry {
        self.entry("all").unwrap_or_else(|_| Entry::fallback())
    }

    /// Get the entry for an output from cosmic-config.
    ///
    /// # Errors
    ///
    /// Fails if the config is missing or fails to parse.
    pub fn entry(&self, output: &str) -> Result<Entry, cosmic_config::Error> {
        self.0.get::<Entry>(output)
    }

    #[must_use]
    pub fn same_on_all(&self) -> bool {
        if let Ok(value) = self.0.get::<bool>(SAME_ON_ALL) {
            return value;
        }

        let _res = self.0.set(SAME_ON_ALL, true);

        true
    }

    pub fn set_same_on_all(&self, value: bool) -> Result<(), cosmic_config::Error> {
        if self.same_on_all() != value {
            return self.0.set(SAME_ON_ALL, value);
        }

        Ok(())
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Setters)]
#[serde(deny_unknown_fields)]
#[must_use]
pub struct Entry {
    /// the configured output
    #[setters(skip)]
    pub output: String,
    /// the configured image source
    #[setters(skip)]
    pub source: Source,
    /// whether the images should be filtered by the active theme
    pub filter_by_theme: bool,
    /// frequency at which the wallpaper is rotated in seconds
    pub rotation_frequency: u64,
    /// filter used to scale images
    #[serde(default)]
    pub filter_method: FilterMethod,
    /// mode used to scale images,
    #[serde(default)]
    pub scaling_mode: ScalingMode,
    #[serde(default)]
    pub sampling_method: SamplingMethod,
}

/// A background image which is colored.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, PartialOrd)]
pub enum Color {
    Single([f32; 3]),
    Gradient(Gradient),
}

/// A background image which is colored by a gradient.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, PartialOrd)]
pub struct Gradient {
    pub colors: Cow<'static, [[f32; 3]]>,
    pub radius: f32,
}

/// The source of a background image.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub enum Source {
    /// Background image(s) from a path.
    Path(PathBuf),
    /// A background color or gradient.
    Color(Color),
}

impl Entry {
    /// Define a preferred background for a given output device.
    pub fn new(output: String, source: Source) -> Self {
        Self {
            output,
            source,
            filter_by_theme: false,
            rotation_frequency: 900,
            filter_method: FilterMethod::default(),
            scaling_mode: ScalingMode::default(),
            sampling_method: SamplingMethod::default(),
        }
    }

    /// Fallback in case config and default schema can't be loaded
    pub fn fallback() -> Self {
        Self {
            output: String::from("all"),
            source: Source::Path(PathBuf::from(
                "/usr/share/backgrounds/cosmic/orion_nebula_nasa_heic0601a.jpg",
            )),
            filter_by_theme: true,
            rotation_frequency: 3600,
            filter_method: FilterMethod::default(),
            scaling_mode: ScalingMode::default(),
            sampling_method: SamplingMethod::default(),
        }
    }
}

/// Image filtering method
#[derive(Debug, Deserialize, Serialize, Clone, Copy, Default, PartialEq, Eq)]
pub enum FilterMethod {
    // nearest neighbor filtering
    Nearest,
    // linear filtering
    Linear,
    // lanczos filtering with window 3
    #[default]
    Lanczos,
}

impl From<FilterMethod> for image::imageops::FilterType {
    fn from(method: FilterMethod) -> Self {
        match method {
            FilterMethod::Nearest => image::imageops::FilterType::Nearest,
            FilterMethod::Linear => image::imageops::FilterType::Triangle,
            FilterMethod::Lanczos => image::imageops::FilterType::Lanczos3,
        }
    }
}

/// Image filtering method
#[derive(Debug, Deserialize, Serialize, Clone, Copy, Default, PartialEq, Eq)]
pub enum SamplingMethod {
    // Rotate through images in Aplhanumeeric order
    #[default]
    Alphanumeric,
    // Rotate through images in Random order
    Random,
    // TODO GnomeWallpapers
}

/// Image scaling mode
#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
pub enum ScalingMode {
    // Fit the image and fill the rest of the area with the given RGB color
    Fit([f32; 3]),
    /// Stretch the image ignoring any aspect ratio to fit the area
    Stretch,
    /// Zoom the image so that it fill the whole area
    #[default]
    Zoom,
}

impl Entry {
    #[must_use]
    pub fn key(&self) -> String {
        self.output.to_string()
    }
}

#[must_use]
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub same_on_all: bool,
    pub outputs: HashSet<String>,
    pub backgrounds: Vec<Entry>,
    pub default_background: Entry,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            same_on_all: true,
            outputs: HashSet::new(),
            backgrounds: Vec::new(),
            default_background: Entry::fallback(),
        }
    }
}

impl Config {
    /// Load config with the provided name from cosmic-config.
    ///
    /// # Errors
    ///
    /// Fails if invalid iter are stored within cosmic-config at time of parsing them.
    pub fn load(context: &Context) -> Result<Self, cosmic_config::Error> {
        let mut config = Self {
            same_on_all: context.same_on_all(),
            ..Default::default()
        };

        config.default_background = context.default_background();
        config.load_backgrounds(context);

        Ok(config)
    }

    pub fn load_backgrounds(&mut self, context: &Context) {
        self.backgrounds.clear();
        self.outputs.clear();

        let entries = context
            .backgrounds()
            .into_iter()
            .filter_map(|output| context.entry(&output_storage_key(&output)).ok());

        for entry in entries {
            self.outputs.insert(entry.output.clone());
            self.backgrounds.push(entry);
        }

        self.default_background = context.default_background();
    }

    /// Returns the effective Entry to use for a given output.
    /// If `same_on_all` is true, this always returns `default_background`.
    /// If `same_on_all` is false, it returns the per-output entry if configured,
    /// otherwise falling back to `default_background`.
    pub fn effective_entry(&self, output_name: &str) -> Entry {
        if self.same_on_all {
            self.default_background.clone()
        } else if let Some(entry) = self.entry(output_name) {
            entry.clone()
        } else {
            self.default_background.clone()
        }
    }

    /// Get the entry for a given output.
    #[must_use]
    pub fn entry(&self, output: &str) -> Option<&Entry> {
        self.backgrounds.iter().find(|entry| entry.output == output)
    }

    /// get a mutable entry for a given output.
    #[must_use]
    pub fn entry_mut(&mut self, output: &str) -> Option<&mut Entry> {
        self.backgrounds
            .iter_mut()
            .find(|entry| entry.output == output)
    }

    /// Applies the entry for the given output to cosmic-config.
    ///
    /// # Errors
    ///
    /// Fails if the config could not be set in cosmic-config.
    pub fn set_entry(
        &mut self,
        context: &Context,
        entry: Entry,
    ) -> Result<(), cosmic_config::Error> {
        let is_all = entry.output == "all";
        let output_key = output_storage_key(&entry.output);

        if context.0.get(&output_key).ok().as_ref() != Some(&entry) {
            context.0.set(&output_key, entry.clone())?;
        }

        if is_all {
            self.default_background = entry;
        } else {
            self.outputs.insert(entry.output.clone());
            if let Some(old) = self.entry_mut(&entry.output) {
                *old = entry;
            } else {
                self.backgrounds.push(entry);
            }
        }

        let mut new_value = self.outputs.iter().cloned().collect::<Vec<_>>();
        new_value.sort();

        let mut cur_backgrounds = context.backgrounds();
        cur_backgrounds.sort();

        if cur_backgrounds != new_value
            && let Err(why) = context.0.set::<Vec<String>>(BACKGROUNDS, new_value)
        {
            tracing::error!(?why, "failed to update outputs");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_key_conversions() {
        assert_eq!(output_storage_key("all"), "all");
        assert_eq!(output_storage_key("DP-1"), "output.DP-1");
        assert_eq!(output_storage_key("HDMI-A-1"), "output.HDMI-A-1");

        assert_eq!(parse_output_storage_key("all"), Some("all"));
        assert_eq!(parse_output_storage_key("output.DP-1"), Some("DP-1"));
        assert_eq!(parse_output_storage_key("other"), None);
    }

    #[test]
    fn test_effective_entry_selection() {
        let default_entry = Entry::new("all".into(), Source::Color(Color::Single([0.1, 0.2, 0.3])));
        let dp1_entry = Entry::new("DP-1".into(), Source::Color(Color::Single([0.4, 0.5, 0.6])));

        let mut config = Config {
            same_on_all: true,
            default_background: default_entry.clone(),
            backgrounds: vec![dp1_entry.clone()],
            outputs: [String::from("DP-1")].into_iter().collect(),
        };

        // When same_on_all is true, all outputs get default_background
        assert_eq!(config.effective_entry("DP-1"), default_entry);
        assert_eq!(config.effective_entry("HDMI-1"), default_entry);

        // When same_on_all is false, configured outputs get their entry
        config.same_on_all = false;
        assert_eq!(config.effective_entry("DP-1"), dp1_entry);
        // Unconfigured outputs fall back to default_background
        assert_eq!(config.effective_entry("HDMI-1"), default_entry);
    }

    #[test]
    fn test_set_entry_in_memory_updates() {
        let temp_dir = tempfile::tempdir().unwrap();
        // Safe in tests: set XDG_CONFIG_HOME to tempdir so cosmic-config writes there
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", temp_dir.path());
        }

        let ctx = context().unwrap();
        let mut config = Config::default();

        let entry1 = Entry::new("DP-1".into(), Source::Color(Color::Single([1.0, 0.0, 0.0])));
        config.set_entry(&ctx, entry1.clone()).unwrap();

        assert_eq!(config.backgrounds.len(), 1);
        assert_eq!(config.entry("DP-1"), Some(&entry1));

        // Setting a second entry for the same output should UPDATE, not duplicate
        let entry2 = Entry::new("DP-1".into(), Source::Color(Color::Single([0.0, 1.0, 0.0])));
        config.set_entry(&ctx, entry2.clone()).unwrap();

        assert_eq!(config.backgrounds.len(), 1, "Should not duplicate entries");
        assert_eq!(
            config.entry("DP-1"),
            Some(&entry2),
            "Should return updated value"
        );

        // Setting "all" updates default_background
        let entry_all = Entry::new("all".into(), Source::Color(Color::Single([0.0, 0.0, 1.0])));
        config.set_entry(&ctx, entry_all.clone()).unwrap();
        assert_eq!(config.default_background, entry_all);
    }
}
