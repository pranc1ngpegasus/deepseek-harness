//! The standing instructions a `dsh tui` session starts with.

use alloc::format;
use alloc::string::String;

/// Build the system prompt for one workspace.
#[must_use]
pub fn system_prompt(workspace: &str) -> String {
    format!(
        "You are a coding agent working in {workspace}.\n\
         Use the provided tools to inspect and change the workspace. Read before you edit. \
         Prefer small, exact edits over rewriting a whole file. Run the relevant tests after a change and fix what fails.\n\
         All tool paths are relative to the workspace root. Shell commands run in the workspace root.\n\
         When the request is complete, answer with a short summary of what changed and what you verified. \
         Do not claim a command or test ran unless the tool output shows it."
    )
}

#[cfg(test)]
mod tests {
    use super::system_prompt;

    #[test]
    fn the_prompt_names_the_workspace_and_the_tool_rules() {
        let prompt = system_prompt("/work/project");
        assert!(prompt.contains("/work/project"));
        assert!(prompt.contains("Read before you edit"));
        assert!(prompt.contains("relative to the workspace root"));
        assert!(prompt.contains("Do not claim a command or test ran"));
    }
}
