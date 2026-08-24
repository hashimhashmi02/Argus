//! A loaded, validated, compiled manifest.

use std::cmp::Reverse;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use argus_reducer::ScreenMatch;
use regex::Regex;

use crate::error::ManifestError;
use crate::schema::{AgentManifest, Region, ScreenRule};

/// An agent definition that is ready to use.
///
/// Separate from [`AgentManifest`], which is the plain on-disk shape, because
/// compiling a regex is expensive and classification runs on every frame of
/// every session. Doing it once at load time also moves every authoring mistake
/// -- a broken pattern, a duplicate rule name -- to startup, where it can be
/// reported against a filename, rather than to the middle of a session.
pub struct Agent {
    manifest: AgentManifest,
    /// Rules sorted by descending priority, with their patterns compiled.
    rules: Vec<CompiledRule>,
}

struct CompiledRule {
    rule: ScreenRule,
    pattern: Regex,
}

impl Agent {
    /// Load and compile a manifest from a JSON string.
    ///
    /// `origin` is only used to make error messages point somewhere useful.
    pub fn from_json(json: &str, origin: impl Into<PathBuf>) -> Result<Self, ManifestError> {
        let origin = origin.into();
        let manifest: AgentManifest =
            serde_json::from_str(json).map_err(|source| ManifestError::Parse {
                path: origin,
                source,
            })?;
        Self::compile(manifest)
    }

    /// Load and compile a manifest from a file.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, ManifestError> {
        let path = path.as_ref();
        let json = std::fs::read_to_string(path).map_err(|source| ManifestError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_json(&json, path)
    }

    fn compile(manifest: AgentManifest) -> Result<Self, ManifestError> {
        if manifest.id.trim().is_empty() {
            return Err(ManifestError::MissingId);
        }
        if manifest.rules.is_empty() {
            return Err(ManifestError::NoRules { id: manifest.id });
        }

        // Rule names end up in logs and in `SessionState::rule`, where they are
        // the only thread back to the JSON that caused a status. Two rules
        // sharing a name would make that thread ambiguous exactly when someone
        // is trying to debug a misclassification.
        let mut seen = HashSet::new();
        for rule in &manifest.rules {
            if !seen.insert(rule.name.as_str()) {
                return Err(ManifestError::DuplicateRule {
                    id: manifest.id.clone(),
                    rule: rule.name.clone(),
                });
            }
        }

        let mut rules = manifest
            .rules
            .iter()
            .map(|rule| {
                Regex::new(&rule.pattern)
                    .map(|pattern| CompiledRule {
                        rule: rule.clone(),
                        pattern,
                    })
                    .map_err(|source| ManifestError::Pattern {
                        rule: rule.name.clone(),
                        source,
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;

        // Sorted once, here, so `classify` can stop at the first match.
        //
        // `Reverse` for descending priority, and `sort_by_key` because it is a
        // *stable* sort: ties keep their file order, which makes "the earlier
        // rule wins" a property an author can rely on rather than a coincidence
        // of whichever sort implementation is in use.
        rules.sort_by_key(|compiled| Reverse(compiled.rule.priority));

        Ok(Self { manifest, rules })
    }

    pub fn manifest(&self) -> &AgentManifest {
        &self.manifest
    }

    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    /// Classify a screen.
    ///
    /// Takes plain lines rather than a `Screen`, which keeps this crate free of
    /// any dependency on `argus-terminal-emu`. The two are composed by
    /// `argus-session` in step 4; neither needs to know the other exists.
    ///
    /// Returns the highest-priority match, or [`ScreenMatch::none`] if nothing
    /// matched -- which the reducer reads as "no information", not as "idle".
    pub fn classify<S: AsRef<str>>(&self, lines: &[S]) -> ScreenMatch {
        // Rules are pre-sorted by descending priority, so the first hit is the
        // winner and there is no reason to keep looking.
        for compiled in &self.rules {
            let haystack = region_text(lines, compiled.rule.region);
            if compiled.pattern.is_match(&haystack) {
                return ScreenMatch::new(compiled.rule.status, compiled.rule.name.clone());
            }
        }
        ScreenMatch::none()
    }
}

/// The slice of the screen a rule looks at, joined into one string.
fn region_text<S: AsRef<str>>(lines: &[S], region: Region) -> String {
    let slice = match region {
        Region::Screen => lines,
        Region::LastLines(n) => {
            let start = lines.len().saturating_sub(n);
            &lines[start..]
        }
    };

    slice
        .iter()
        .map(AsRef::as_ref)
        .collect::<Vec<_>>()
        .join("\n")
}
