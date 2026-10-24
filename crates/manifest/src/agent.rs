//! A loaded, validated, compiled manifest.

use std::cmp::Reverse;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use argus_reducer::ScreenMatch;
use regex::Regex;

use crate::error::ManifestError;
use crate::schema::{AgentManifest, Region, SESSION_UUID, ScreenRule};

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
    /// Whether any command line mentions [`SESSION_UUID`].
    wants_session_uuid: bool,
}

/// Which command line to build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// A fresh session: `launch.args` then `launch.new_session_args`.
    New,
    /// Reattach to earlier work: `launch.args` then `resume.args`.
    Resume,
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

        let mentions = |args: &[String]| args.iter().any(|arg| arg.contains(SESSION_UUID));
        let launch = &manifest.launch;
        let at_launch = mentions(&launch.args) || mentions(&launch.new_session_args);
        let at_resume = manifest.resume.as_ref().is_some_and(|r| mentions(&r.args));
        // Resuming by an id that no launch ever handed the agent would ask it
        // for a conversation that cannot exist. Caught here, against the file,
        // rather than as an agent's "no conversation found" days later.
        if at_resume && !at_launch {
            return Err(ManifestError::ResumeWithoutSessionUuid { id: manifest.id });
        }

        Ok(Self {
            manifest,
            rules,
            wants_session_uuid: at_launch,
        })
    }

    pub fn manifest(&self) -> &AgentManifest {
        &self.manifest
    }

    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    /// Whether each session of this agent needs a conversation id of its own,
    /// made up when it starts and kept for resuming it.
    pub fn wants_session_uuid(&self) -> bool {
        self.wants_session_uuid
    }

    /// Whether the manifest says how to reattach to earlier work at all.
    pub fn can_resume(&self) -> bool {
        self.manifest.resume.is_some()
    }

    /// The arguments to start a session with, `{session_uuid}` filled in.
    ///
    /// `None` for [`Start::Resume`] when the manifest has no `resume`. A
    /// `session_uuid` of `None` leaves the placeholder as it is, which an agent
    /// that wants one rejects loudly; a caller that knows the agent
    /// [wants one](Self::wants_session_uuid) should not pass `None`.
    pub fn args(&self, start: Start, session_uuid: Option<&str>) -> Option<Vec<String>> {
        let launch = &self.manifest.launch;
        let extra = match start {
            Start::New => &launch.new_session_args,
            Start::Resume => &self.manifest.resume.as_ref()?.args,
        };
        let fill = |arg: &String| match session_uuid {
            Some(uuid) => arg.replace(SESSION_UUID, uuid),
            None => arg.clone(),
        };
        Some(launch.args.iter().chain(extra).map(fill).collect())
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
            // Count from the last line with anything on it, not from the bottom
            // of the grid. A terminal that has not filled up yet has its content
            // at the top and blank rows below -- a fresh shell is three lines of
            // banner and prompt above twenty-seven empty ones. Counting from the
            // grid's bottom edge would make "the last three lines" mean "three
            // blank lines" and no bottom-anchored rule would ever match until
            // the screen had scrolled. Found by the first live session test.
            let content_end = lines
                .iter()
                .rposition(|line| !line.as_ref().trim().is_empty())
                .map_or(0, |last| last + 1);
            let start = content_end.saturating_sub(n);
            &lines[start..content_end]
        }
    };

    slice
        .iter()
        .map(AsRef::as_ref)
        .collect::<Vec<_>>()
        .join("\n")
}
