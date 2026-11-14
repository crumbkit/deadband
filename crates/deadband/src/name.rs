//! How `deadband status` and `deadband top` name a job from its workspace,
//! its label and its short command.
//!
//! The workspace and the label are separate fields. A display joins them
//! with a middle dot, and leaves the label out when it adds nothing: when it
//! equals the workspace, as for `mxv up`, whose label is the workspace name,
//! or when it equals the first word of the short command, which is the label
//! that `deadband run` gives when `--label` is absent. A job with no short
//! command, as from an older daemon, always shows its label.

use crate::protocol::{Label, Short};

#[cfg(test)]
mod tests;

/// Reports whether the job column of `deadband top` shows the label of a
/// job in `workspace`, labelled `label`, whose short command is `short`.
#[must_use]
pub fn shows_label(workspace: Option<&str>, label: &str, short: Option<&str>) -> bool {
    let Some(short) = short.filter(|short| !short.is_empty()) else {
        return true;
    };
    workspace != Some(label) && short.split(' ').next() != Some(label)
}

/// Returns the name that `deadband status` gives a job: the workspace and
/// the label joined by a middle dot, or the one of them that shows. A job
/// with no workspace is named by its label.
#[must_use]
pub fn name(workspace: Option<&Label>, label: &Label, short: Option<&Short>) -> String {
    let workspace = workspace.map(Label::as_str);
    let label = label.as_str();
    let short = short.map(Short::as_str);
    match workspace {
        Some(workspace) if workspace != label && shows_label(Some(workspace), label, short) => {
            format!("{workspace} · {label}")
        }
        Some(workspace) => workspace.to_owned(),
        None => label.to_owned(),
    }
}
